use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use url::Url;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PortalCookie {
    pub name: String,
    pub value: String,
    pub origin_host: String,
    pub domain: String,
    pub path: String,
    pub host_only: bool,
    pub secure: bool,
    pub expires: Option<String>,
}

impl PortalCookie {
    pub(super) fn key(&self) -> (String, String, String) {
        (self.domain.clone(), self.path.clone(), self.name.clone())
    }

    pub(crate) fn valid(&self) -> bool {
        let origin = self.origin_host.as_str();
        let token = |value: &str| {
            !value.is_empty()
                && value.len() <= 4096
                && value
                    .bytes()
                    .all(|byte| (33..127).contains(&byte) && !b";,\"\\".contains(&byte))
        };
        token(&self.name)
            && self.name.len() <= 256
            && self
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            && token(&self.value)
            && self.secure
            && matches!(
                origin,
                "idmsa.apple.com" | "appstoreconnect.apple.com" | "developer.apple.com"
            )
            && (self.domain == origin || (!self.host_only && self.domain == "apple.com"))
            && (!self.host_only || self.domain == origin)
            && self.path.starts_with('/')
            && self.path.len() <= 4096
            && self
                .path
                .bytes()
                .all(|byte| (33..127).contains(&byte) && byte != b';')
            && (!self.name.starts_with("__Host-") || (self.host_only && self.path == "/"))
            && self
                .expires
                .as_ref()
                .is_none_or(|value| DateTime::parse_from_rfc3339(value).is_ok())
    }

    pub(crate) fn unexpired(&self) -> bool {
        self.expires.as_ref().is_none_or(|value| {
            DateTime::parse_from_rfc3339(value)
                .is_ok_and(|stamp| stamp.timestamp() > Utc::now().timestamp())
        })
    }

    fn applies(&self, target: &Url) -> bool {
        let host = target.host_str().unwrap_or_default();
        let domain = if self.host_only {
            host == self.domain
        } else {
            host == self.domain || host.ends_with(&format!(".{}", self.domain))
        };
        let path = target.path();
        let path_matches = path == self.path
            || (path.starts_with(&self.path)
                && (self.path.ends_with('/')
                    || path.as_bytes().get(self.path.len()) == Some(&b'/')));
        domain && path_matches && target.scheme() == "https" && self.valid() && self.unexpired()
    }
}

pub(super) fn validate(cookies: &[PortalCookie]) -> Result<()> {
    if cookies.is_empty() {
        bail!("developer_cookie_invalid");
    }
    validate_jar(cookies)
}

pub(super) fn validate_jar(cookies: &[PortalCookie]) -> Result<()> {
    let mut keys = BTreeSet::new();
    if cookies.len() > 150
        || cookies
            .iter()
            .any(|item| !item.valid() || !keys.insert(item.key()))
    {
        bail!("developer_cookie_invalid");
    }
    Ok(())
}

pub(super) fn header(cookies: &[PortalCookie], target: &Url) -> Result<String> {
    validate_jar(cookies)?;
    let selected: Vec<_> = cookies.iter().filter(|item| item.applies(target)).collect();
    if selected
        .iter()
        .filter(|item| item.name == "myacinfo")
        .count()
        != 1
    {
        bail!("developer_session_required");
    }
    Ok(selected
        .iter()
        .map(|item| format!("{}={}", item.name, item.value))
        .collect::<Vec<_>>()
        .join("; "))
}

pub(super) fn rotate(
    cookies: &mut Vec<PortalCookie>,
    headers: &reqwest::header::HeaderMap,
    target: &Url,
) -> Result<()> {
    let mut changed = cookies.clone();
    let mut response_keys = BTreeSet::new();
    let mut deletions = BTreeSet::new();
    let mut invalid = false;
    for raw in headers.get_all(reqwest::header::SET_COOKIE) {
        let (item, deleting) = match response_cookie(raw, target) {
            Ok(value) => value,
            Err(_) => {
                invalid = true;
                continue;
            }
        };
        if changed
            .iter()
            .any(|old| old.key() == item.key() && old.host_only != item.host_only)
        {
            invalid = true;
            continue;
        }
        if deleting {
            deletions.insert(item.key());
        }
        if !response_keys.insert(item.key()) {
            invalid = true;
            continue;
        }
        changed.retain(|old| old.key() != item.key());
        if !deleting {
            changed.push(item);
        }
    }
    let authentication_count = changed
        .iter()
        .filter(|item| item.name == "myacinfo" && item.applies(target))
        .count();
    if invalid || validate_jar(&changed).is_err() || authentication_count > 1 {
        cookies.retain(|item| !deletions.contains(&item.key()));
        bail!("developer_cookie_invalid");
    }
    *cookies = changed;
    Ok(())
}

fn response_cookie(
    raw: &reqwest::header::HeaderValue,
    target: &Url,
) -> Result<(PortalCookie, bool)> {
    let raw = raw
        .to_str()
        .map_err(|_| anyhow::anyhow!("developer_cookie_invalid"))?;
    let parsed = cookie::Cookie::parse(raw.to_owned())
        .map_err(|_| anyhow::anyhow!("developer_cookie_invalid"))?;
    let origin = "developer.apple.com".to_owned();
    let domain = parsed
        .domain()
        .unwrap_or(&origin)
        .trim_start_matches('.')
        .to_ascii_lowercase();
    let default_path =
        target.path().rsplit_once('/').map_or(
            "/",
            |(prefix, _)| if prefix.is_empty() { "/" } else { prefix },
        );
    let deleting = parsed.value_trimmed().is_empty()
        || parsed.max_age().is_some_and(|age| age.whole_seconds() <= 0);
    let expires = parsed
        .max_age()
        .map(|age| Utc::now().timestamp().saturating_add(age.whole_seconds()))
        .or_else(|| {
            parsed
                .expires_datetime()
                .map(|stamp| stamp.unix_timestamp())
        });
    let expires = match expires {
        Some(stamp) => Some(
            DateTime::from_timestamp(stamp, 0)
                .ok_or_else(|| anyhow::anyhow!("developer_cookie_invalid"))?
                .to_rfc3339(),
        ),
        None => None,
    };
    let item = PortalCookie {
        name: parsed.name().into(),
        value: if deleting {
            "deleted".into()
        } else {
            parsed.value_trimmed().into()
        },
        origin_host: origin,
        domain,
        path: parsed.path().unwrap_or(default_path).into(),
        host_only: parsed.domain().is_none(),
        secure: parsed.secure() == Some(true),
        expires,
    };
    if !item.valid() {
        bail!("developer_cookie_invalid");
    }
    let deleting = deleting || !item.unexpired();
    Ok((item, deleting))
}
