//! Private response-cookie overlay. The imported authentication source is never written.
use super::*;
use reqwest::header::SET_COOKIE;
use std::time::Instant;

pub(super) struct Snapshot {
    pub generation: String,
    pub cookies: Vec<Cookie>,
}

pub(super) enum Update {
    Set {
        cookie: Cookie,
        expected: Option<String>,
    },
    Remove {
        key: (String, String, String),
        expected: Option<String>,
    },
}
impl Update {
    pub fn cookie(&self) -> Option<&Cookie> {
        match self {
            Self::Set { cookie, .. } => Some(cookie),
            _ => None,
        }
    }
}
fn key(c: &Cookie) -> (String, String, String) {
    (
        c.name.clone(),
        c.domain.trim_start_matches('.').to_ascii_lowercase(),
        if c.path.is_empty() {
            "/".into()
        } else {
            c.path.clone()
        },
    )
}
pub(super) struct Lock(fs::File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
pub(super) fn lock(root: &Path) -> Result<Lock> {
    let file = super::open_private_lock(root, ".session.lock")?;
    let start = Instant::now();
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(Lock(file)),
            Err(e)
                if (e.kind() == std::io::ErrorKind::WouldBlock
                    || e.raw_os_error() == fs2::lock_contended_error().raw_os_error())
                    && start.elapsed() < Duration::from_secs(5) =>
            {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(_) => bail!("reviewer_session_busy"),
        }
    }
}

/// Publish a confirmed login under the same lock as response-cookie rotation.
pub(super) fn replace(source: &Path, root: &Path, data: &Value) -> Result<()> {
    let _lock = lock(root)?;
    atomic_json(source, data)?;
    Ok(())
}
pub(super) fn read(source: &Path, root: &Path) -> Result<Snapshot> {
    // Source removal revokes this client; never silently continue with an old overlay.
    let bytes = private_read(source, MAX_SESSION)?;
    let generation = hex::encode(Sha256::digest(&bytes));
    let state_path = root.join("session.state.json");
    if state_path.exists() {
        let state: Value = serde_json::from_slice(&private_read(&state_path, MAX_SESSION)?)
            .map_err(|_| anyhow::anyhow!("reviewer_session_invalid"))?;
        if state["generation"].as_str() == Some(&generation) {
            return Ok(Snapshot {
                generation,
                cookies: parse_cookies(&serde_json::to_vec(&state)?)?,
            });
        }
    }
    let cookies = parse_cookies(&bytes)?;
    cookie_header(&cookies)?;
    Ok(Snapshot {
        generation,
        cookies,
    })
}
pub(super) async fn load(source: PathBuf, root: PathBuf) -> Result<Snapshot> {
    tokio::task::spawn_blocking(move || {
        let _lock = lock(&root)?;
        read(&source, &root)
    })
    .await
    .map_err(|_| anyhow::anyhow!("reviewer_session_worker_failed"))?
}
pub(super) async fn persist(
    source: PathBuf,
    root: PathBuf,
    generation: String,
    updates: Vec<Update>,
) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let _lock = lock(&root)?;
        let mut current = read(&source, &root)?;
        if current.generation != generation {
            return Ok(());
        }
        for update in updates {
            match update {
                Update::Set { cookie, expected } => {
                    let k = key(&cookie);
                    if let Some(i) = current.cookies.iter().position(|c| key(c) == k) {
                        if expected.as_ref() == Some(&current.cookies[i].value) {
                            current.cookies[i] = cookie;
                        }
                    } else if expected.is_none() && current.cookies.len() < 150 {
                        current.cookies.push(cookie);
                    }
                }
                Update::Remove { key: k, expected } => current
                    .cookies
                    .retain(|c| key(c) != k || Some(&c.value) != expected.as_ref()),
            }
        }
        current.cookies.retain(|c| valid_expiry(&c.expires));
        let value = json!({"generation":current.generation,"cookies":current.cookies});
        if serde_json::to_vec_pretty(&value)?.len() > MAX_SESSION {
            bail!("reviewer_private_file_too_large");
        }
        atomic_json(&root.join("session.state.json"), &value)?;
        Ok(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("reviewer_session_worker_failed"))?
}

pub(super) fn updates(headers: &HeaderMap, url: &Url, sent: &[Cookie]) -> Vec<Update> {
    let mut observed: BTreeMap<_, _> = sent.iter().map(|c| (key(c), c.value.clone())).collect();
    let host = url.host_str().unwrap_or_default();
    // Mock servers represent ASC in tests; production accepts only the fixed HTTPS origin.
    #[cfg(test)]
    let host = if host == "127.0.0.1" || host == "localhost" {
        "appstoreconnect.apple.com"
    } else {
        host
    };
    let mut result = Vec::new();
    for raw in headers.get_all(SET_COOKIE).iter().take(50) {
        let Ok(raw) = raw.to_str() else { continue };
        if raw.len() > 4096 {
            continue;
        }
        let Ok(c) = cookie::Cookie::parse(raw) else {
            continue;
        };
        let domain = c
            .domain()
            .unwrap_or(host)
            .trim_start_matches('.')
            .to_ascii_lowercase();
        if !matches!(domain.as_str(), "apple.com" | "appstoreconnect.apple.com")
            || !(host == domain || host.ends_with(&format!(".{domain}")))
        {
            continue;
        }
        // Keep the established root-cookie scope. A missing Path defaults to the request directory.
        let default_path = url
            .path()
            .rsplit_once('/')
            .map(|(p, _)| if p.is_empty() { "/" } else { p })
            .unwrap_or("/");
        if c.path().unwrap_or(default_path) != "/" {
            continue;
        }
        if c.name().starts_with("__Secure-") && c.secure() != Some(true) {
            continue;
        }
        if c.name().starts_with("__Host-")
            && (c.secure() != Some(true) || c.domain().is_some() || c.path() != Some("/"))
        {
            continue;
        }
        if c.secure() == Some(true) && url.scheme() != "https" {
            continue;
        }
        let expires = match c.max_age() {
            Some(age) => {
                // Every non-positive Max-Age is deletion, including values outside datetime range.
                let seconds = age.whole_seconds().max(0);
                let Some(duration) = chrono::Duration::try_seconds(seconds) else {
                    continue;
                };
                let Some(date) = Utc::now().checked_add_signed(duration) else {
                    continue;
                };
                let value = date.to_rfc3339();
                if DateTime::parse_from_rfc3339(&value).is_err() {
                    continue;
                }
                Some(value)
            }
            None => c
                .expires_datetime()
                .and_then(|d| DateTime::from_timestamp(d.unix_timestamp(), 0))
                .map(|d| d.to_rfc3339()),
        };
        let candidate = Cookie {
            name: c.name().into(),
            value: c.value().into(),
            domain,
            path: "/".into(),
            expires,
        };
        // Validate syntax before either replacement or deletion.
        let valid_name = !candidate.name.is_empty()
            && candidate
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
        let valid_value = candidate
            .value
            .bytes()
            .all(|b| (0x21..=0x7e).contains(&b) && !b"\";,\\".contains(&b));
        if !valid_name || !valid_value {
            continue;
        }
        if candidate.value.is_empty()
            || !valid_expiry(&candidate.expires)
            || c.max_age().is_some_and(|age| age.whole_seconds() <= 0)
        {
            let k = key(&candidate);
            result.push(Update::Remove {
                expected: observed.remove(&k),
                key: k,
            });
        } else {
            let expected = observed.insert(key(&candidate), candidate.value.clone());
            result.push(Update::Set {
                cookie: candidate,
                expected,
            });
        }
    }
    result
}
