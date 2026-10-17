use super::*;

#[derive(Clone)]
pub(crate) struct SharedSessionHandle {
    source: PathBuf,
    root: PathBuf,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SharedCookie {
    pub(crate) name: String,
    pub(crate) value: String,
    pub(crate) expires: Option<String>,
}

#[derive(Clone)]
pub(crate) struct SharedSnapshot {
    pub(crate) identity: String,
    pub(crate) cookies: Vec<SharedCookie>,
    pub(crate) secrets: Vec<String>,
    generation: String,
}

impl Reviewer {
    pub(crate) fn shared_session(&self) -> SharedSessionHandle {
        SharedSessionHandle {
            source: self.cookie_path.clone(),
            root: self.root.clone(),
        }
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn valid_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && value
            .bytes()
            .all(|byte| (0x21..=0x7e).contains(&byte) && !b"\";,\\".contains(&byte))
}

fn eligible(cookie: &Cookie) -> bool {
    let domain = cookie.domain.strip_prefix('.').unwrap_or(&cookie.domain);
    domain.eq_ignore_ascii_case("apple.com")
        && (cookie.path.is_empty() || cookie.path == "/")
        && !cookie.name.starts_with("__Host-")
        && valid_name(&cookie.name)
        && valid_value(&cookie.value)
        && valid_expiry(&cookie.expires)
}

impl SharedSessionHandle {
    pub(crate) fn snapshot(&self) -> Result<SharedSnapshot> {
        let _lock = session::lock(&self.root)?;
        self.build()
    }

    pub(crate) fn ensure_current(&self, expected: &SharedSnapshot) -> Result<()> {
        let current = self.snapshot()?;
        if current.identity != expected.identity || current.cookies != expected.cookies {
            bail!("shared_session_changed");
        }
        Ok(())
    }

    pub(crate) fn with_current_authentication<T>(
        &self,
        expected: &SharedSnapshot,
        action: impl FnOnce(&SharedSnapshot) -> Result<T>,
    ) -> Result<T> {
        let _lock = session::lock(&self.root)?;
        let current = self.build()?;
        fn authentication(snapshot: &SharedSnapshot) -> Option<&str> {
            snapshot
                .cookies
                .iter()
                .find(|cookie| cookie.name == "myacinfo")
                .map(|cookie| cookie.value.as_str())
        }
        if current.identity != expected.identity
            || authentication(&current) != authentication(expected)
        {
            bail!("shared_session_changed");
        }
        action(&current)
    }

    pub(crate) fn rotate(
        &self,
        expected: &SharedSnapshot,
        updates: &[SharedCookie],
    ) -> Result<SharedSnapshot> {
        let _lock = session::lock(&self.root)?;
        let current = self.build()?;
        if current.identity != expected.identity || current.cookies != expected.cookies {
            bail!("shared_session_changed");
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut changed = false;
        for update in updates {
            let Some(old) = expected
                .cookies
                .iter()
                .find(|cookie| cookie.name == update.name)
            else {
                bail!("shared_session_key_unknown");
            };
            if !seen.insert(update.name.as_str())
                || !valid_value(&update.value)
                || !valid_expiry(&update.expires)
            {
                bail!("shared_session_update_invalid");
            }
            changed |= old != update;
        }
        if !changed {
            return Ok(current);
        }
        let snapshot = session::read(&self.source, &self.root)?;
        if snapshot.generation != expected.generation {
            bail!("shared_session_changed");
        }
        let mut raw = snapshot.cookies;
        for update in updates {
            let Some(cookie) = raw
                .iter_mut()
                .find(|cookie| eligible(cookie) && cookie.name == update.name)
            else {
                bail!("shared_session_changed");
            };
            cookie.value = update.value.clone();
            cookie.expires = update.expires.clone();
        }
        let value = json!({"generation":snapshot.generation,"cookies":raw});
        if serde_json::to_vec_pretty(&value)?.len() > MAX_SESSION {
            bail!("reviewer_private_file_too_large");
        }
        atomic_json(&self.root.join("session.state.json"), &value)?;
        self.build()
    }

    fn build(&self) -> Result<SharedSnapshot> {
        let effective = session::read(&self.source, &self.root)?;
        let source = parse_cookies(&private_read(&self.source, MAX_SESSION)?)?;
        let mut cookies: Vec<SharedCookie> = effective
            .cookies
            .iter()
            .filter(|cookie| eligible(cookie))
            .map(|cookie| SharedCookie {
                name: cookie.name.clone(),
                value: cookie.value.clone(),
                expires: cookie.expires.clone(),
            })
            .collect();
        cookies.sort_by(|left, right| left.name.cmp(&right.name));
        if cookies.windows(2).any(|pair| pair[0].name == pair[1].name) {
            bail!("shared_session_ambiguous");
        }
        if cookies
            .iter()
            .filter(|cookie| cookie.name == "myacinfo")
            .count()
            != 1
        {
            bail!("shared_session_required");
        }
        let mut secrets: Vec<String> = source
            .iter()
            .chain(effective.cookies.iter())
            .map(|cookie| cookie.value.clone())
            .collect();
        secrets.sort();
        secrets.dedup();
        let mut digest = Sha256::new();
        for part in [
            "asc-mcp-shared-apple-session-v1",
            &self.source.to_string_lossy(),
            &self.root.to_string_lossy(),
            &effective.generation,
        ] {
            digest.update((part.len() as u64).to_be_bytes());
            digest.update(part.as_bytes());
        }
        Ok(SharedSnapshot {
            identity: hex::encode(digest.finalize()),
            cookies,
            secrets,
            generation: effective.generation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FUTURE: &str = "2999-01-01T00:00:00Z";

    fn cookie(name: &str, value: &str, domain: &str, path: &str, expires: Option<&str>) -> Value {
        json!({"name":name,"value":value,"domain":domain,"path":path,"expires":expires})
    }

    fn base() -> Vec<Value> {
        vec![
            cookie("myacinfo", "sso-old", ".apple.com", "/", Some(FUTURE)),
            cookie("dsid", "dsid-old", "apple.com", "", None),
        ]
    }

    fn setup(cookies: Vec<Value>) -> (tempfile::TempDir, SharedSessionHandle) {
        let dir = tempfile::tempdir().unwrap();
        let handle = SharedSessionHandle {
            source: dir.path().join("source.json"),
            root: dir.path().to_owned(),
        };
        atomic_json(&handle.source, &json!({ "cookies": cookies })).unwrap();
        (dir, handle)
    }

    fn update(name: &str, value: &str) -> SharedCookie {
        SharedCookie {
            name: name.into(),
            value: value.into(),
            expires: Some(FUTURE.into()),
        }
    }

    #[test]
    fn filters_to_valid_parent_root_cookies() {
        let mut cookies = base();
        cookies.extend([
            cookie("asc", "asc-v", "appstoreconnect.apple.com", "/", None),
            cookie("__Host-x", "host-v", "apple.com", "/", None),
            cookie("scoped", "scoped-v", "apple.com", "/path", None),
            cookie(
                "expired",
                "expired-v",
                "apple.com",
                "/",
                Some("2000-01-01T00:00:00Z"),
            ),
            cookie("bad", "a;b", "apple.com", "/", None),
            cookie("other", "other-v", "evil.example", "/", None),
        ]);
        let (_dir, handle) = setup(cookies);
        let snapshot = handle.snapshot().unwrap();
        let names: Vec<_> = snapshot
            .cookies
            .iter()
            .map(|cookie| cookie.name.as_str())
            .collect();
        assert_eq!(names, ["dsid", "myacinfo"]);
        assert_eq!(snapshot.identity.len(), 64);
        assert!(
            snapshot
                .identity
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        );
    }

    #[test]
    fn requires_exactly_one_unexpired_myacinfo() {
        let (_a, duplicate) = setup(vec![
            cookie("myacinfo", "one", "apple.com", "/", None),
            cookie("myacinfo", "two", ".apple.com", "", None),
        ]);
        assert!(duplicate.snapshot().is_err());
        let (_b, expired) = setup(vec![
            cookie(
                "myacinfo",
                "one",
                "apple.com",
                "/",
                Some("2000-01-01T00:00:00Z"),
            ),
            cookie("dsid", "d", "apple.com", "/", None),
        ]);
        assert!(expired.snapshot().is_err());
        let (_c, missing) = setup(vec![cookie("dsid", "d", "apple.com", "/", None)]);
        assert!(missing.snapshot().is_err());
    }

    #[test]
    fn rotation_writes_overlay_only_and_exposes_both_secrets() {
        let (dir, handle) = setup(base());
        let source_bytes = fs::read(&handle.source).unwrap();
        let first = handle.snapshot().unwrap();
        let rotated = handle
            .rotate(&first, &[update("myacinfo", "sso-new")])
            .unwrap();
        assert_eq!(fs::read(&handle.source).unwrap(), source_bytes);
        assert_eq!(rotated.identity, first.identity);
        assert!(
            rotated
                .cookies
                .iter()
                .any(|cookie| cookie.name == "myacinfo" && cookie.value == "sso-new")
        );
        assert!(rotated.secrets.contains(&"sso-old".to_string()));
        assert!(rotated.secrets.contains(&"sso-new".to_string()));
        assert!(dir.path().join("session.state.json").exists());
        assert!(handle.ensure_current(&first).is_err());
        handle.ensure_current(&rotated).unwrap();
    }

    #[test]
    fn stale_expected_value_is_rejected_and_newer_overlay_kept() {
        let (dir, handle) = setup(base());
        let first = handle.snapshot().unwrap();
        handle
            .rotate(&first, &[update("myacinfo", "sso-new")])
            .unwrap();
        let overlay = dir.path().join("session.state.json");
        let before = fs::read(&overlay).unwrap();
        assert!(
            handle
                .rotate(&first, &[update("myacinfo", "sso-stale")])
                .is_err()
        );
        assert_eq!(fs::read(&overlay).unwrap(), before);
    }

    #[test]
    fn source_replacement_or_removal_revokes() {
        let (_dir, handle) = setup(base());
        let first = handle.snapshot().unwrap();
        atomic_json(
            &handle.source,
            &json!({"cookies":[cookie("myacinfo", "sso-other", "apple.com", "/", None)]}),
        )
        .unwrap();
        assert!(handle.ensure_current(&first).is_err());
        assert!(handle.rotate(&first, &[update("myacinfo", "x")]).is_err());
        fs::remove_file(&handle.source).unwrap();
        assert!(handle.snapshot().is_err());
        assert!(handle.ensure_current(&first).is_err());
    }

    #[test]
    fn rejects_new_keys_invalid_updates_and_writes_nothing() {
        let (dir, handle) = setup(base());
        let first = handle.snapshot().unwrap();
        let overlay = dir.path().join("session.state.json");
        let expired = SharedCookie {
            expires: Some("2000-01-01T00:00:00Z".into()),
            ..update("myacinfo", "v")
        };
        for bad in [
            vec![update("newkey", "v")],
            vec![update("myacinfo", "")],
            vec![update("myacinfo", "a;b")],
            vec![expired],
            vec![update("myacinfo", "a"), update("myacinfo", "b")],
        ] {
            assert!(handle.rotate(&first, &bad).is_err());
            assert!(!overlay.exists());
        }
    }

    #[test]
    fn unchanged_rotation_does_not_write() {
        let (dir, handle) = setup(base());
        let first = handle.snapshot().unwrap();
        let same = first.cookies.clone();
        let again = handle.rotate(&first, &same).unwrap();
        assert!(again.cookies == first.cookies);
        assert!(!dir.path().join("session.state.json").exists());
    }

    #[tokio::test]
    async fn delayed_reviewer_rotation_cannot_overwrite_newer_shared_cas() {
        let (_dir, handle) = setup(base());
        let reviewer = Reusable::from_paths(handle.source.clone(), handle.root.clone())
            .await
            .unwrap()
            .unwrap();
        let first = handle.snapshot().unwrap();
        handle
            .rotate(&first, &[update("myacinfo", "developer-newer-sso")])
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "set-cookie",
            "myacinfo=delayed-reviewer-sso; Domain=apple.com; Path=/; Secure"
                .parse()
                .unwrap(),
        );
        reviewer
            .save_rotation(
                &headers,
                &Url::parse("https://appstoreconnect.apple.com/").unwrap(),
            )
            .await
            .unwrap();
        let current = handle.snapshot().unwrap();
        assert!(
            current
                .cookies
                .iter()
                .any(|cookie| cookie.name == "myacinfo" && cookie.value == "developer-newer-sso")
        );
    }
}
