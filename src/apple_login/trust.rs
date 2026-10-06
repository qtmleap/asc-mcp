//! Login-only trusted-device cookies (IDMSA origin), bound to an account fingerprint.
//! Kept in a private directory that the reviewer service never mounts or reads.
use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};
use url::Url;

use crate::reviewer::{atomic_json, private_read};

pub(super) const IDMSA_HOST: &str = "idmsa.apple.com";
const MAX_TRUST: usize = 256 * 1024;
const MAX_COOKIES: usize = 150;
const FILE: &str = "trust.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TrustCookie {
    pub name: String,
    pub value: String,
    pub origin_host: String,
    pub domain: String,
    pub host_only: bool,
    pub path: String,
    pub secure: bool,
    #[serde(default)]
    pub expires: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustFile {
    version: u32,
    account: String,
    cookies: Vec<TrustCookie>,
}

/// Deterministic account binding; the identity itself is never stored.
pub(super) fn fingerprint(email: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"asc-mcp/login-trust/v1\0");
    h.update(email.trim().to_lowercase().as_bytes());
    hex::encode(h.finalize())
}

fn live(expires: &Option<String>) -> bool {
    expires.as_ref().is_none_or(|e| {
        DateTime::parse_from_rfc3339(e).is_ok_and(|d| d.with_timezone(&Utc) > Utc::now())
    })
}

impl TrustCookie {
    pub fn valid(&self) -> bool {
        self.origin_host == IDMSA_HOST && self.valid_response()
    }
    pub fn valid_response(&self) -> bool {
        let token = |b: u8| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b);
        !self.name.is_empty()
            && self.name.len() <= 256
            && self.name.bytes().all(token)
            && !self.value.is_empty()
            && self.value.len() <= 4096
            && self
                .value
                .bytes()
                .all(|b| (0x21..=0x7e).contains(&b) && !b"\";,\\".contains(&b))
            && matches!(
                self.origin_host.as_str(),
                IDMSA_HOST | "appstoreconnect.apple.com"
            )
            && if self.host_only {
                self.domain == self.origin_host
            } else {
                self.domain == "apple.com" || self.domain == self.origin_host
            }
            && (!self.name.starts_with("__Secure-") || self.secure)
            && (!self.name.starts_with("__Host-")
                || (self.secure && self.host_only && self.path == "/"))
            && self.path.starts_with('/')
            && self.path.len() <= 256
            && self.path.bytes().all(|b| (0x20..0x7f).contains(&b))
            && self
                .expires
                .as_ref()
                .is_none_or(|e| DateTime::parse_from_rfc3339(e).is_ok())
    }
    pub fn unexpired(&self) -> bool {
        live(&self.expires)
    }
    fn applies(&self, url: &Url) -> bool {
        let host = url.host_str().unwrap_or_default();
        let host_ok = if self.host_only {
            host == self.domain
        } else {
            host == self.domain || host.ends_with(&format!(".{}", self.domain))
        };
        let path = url.path();
        let path_ok = path == self.path
            || (path.starts_with(&self.path)
                && (self.path.ends_with('/') || path[self.path.len()..].starts_with('/')));
        host_ok && path_ok && (!self.secure || url.scheme() == "https") && self.unexpired()
    }
}

/// URL-selected Cookie header for IDMSA only, merged with the live jar's cookies (jar wins on
/// name collisions) so exactly one header is sent. DES* values are sent quoted, as fastlane does.
pub(super) fn cookie_header(
    cookies: &[TrustCookie],
    url: &Url,
    jar: Option<&str>,
) -> Option<String> {
    if url.host_str() != Some(IDMSA_HOST) {
        return None;
    }
    let mut selected: Vec<_> = cookies
        .iter()
        .filter(|c| c.valid() && c.applies(url))
        .collect();
    let pairs: Vec<_> = jar
        .into_iter()
        .flat_map(|j| j.split(';'))
        .filter_map(|p| {
            let (name, raw) = p.trim().split_once('=')?;
            let value = raw
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(raw);
            let valid_name = !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
            let valid_value = !value.is_empty()
                && value
                    .bytes()
                    .all(|b| (0x21..=0x7e).contains(&b) && !b"\";,\\".contains(&b));
            let prefix_ok = !name.starts_with("__Secure-") && !name.starts_with("__Host-")
                || selected.iter().any(|c| c.name == name && c.value == value);
            (valid_name && valid_value && prefix_ok).then_some((name, value))
        })
        .collect();
    // Fresh response metadata normally already contains the live jar's values. Preserve
    // same-name cookies at different paths; replace stale seed values only if unrecorded.
    let overrides: Vec<_> = pairs
        .iter()
        .filter(|(n, v)| !selected.iter().any(|c| c.name == *n && c.value == *v))
        .map(|(n, _)| *n)
        .collect();
    selected.retain(|c| !overrides.contains(&c.name.as_str()));
    selected.sort_by(|a, b| b.path.len().cmp(&a.path.len()).then(a.name.cmp(&b.name)));
    let render = |name: &str, value: &str| {
        if name.contains("DES") {
            format!("{name}=\"{value}\"")
        } else {
            format!("{name}={value}")
        }
    };
    let mut parts: Vec<_> = selected.iter().map(|c| render(&c.name, &c.value)).collect();
    for (name, value) in pairs {
        if !selected.iter().any(|c| c.name == name) {
            parts.push(render(name, value));
        }
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

#[cfg(unix)]
fn check_private_dir(meta: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if meta.permissions().mode() & 0o077 != 0 {
        bail!("apple_login_trust_permissions: directory must have mode 700");
    }
    Ok(())
}
#[cfg(not(unix))]
fn check_private_dir(_: &fs::Metadata) -> Result<()> {
    Ok(())
}

/// `Ok(true)` if the directory exists and is a private real directory; `Ok(false)` if absent.
fn root_state(root: &Path) -> Result<bool> {
    match fs::symlink_metadata(root) {
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
            bail!("apple_login_trust_directory_invalid")
        }
        Ok(m) => check_private_dir(&m).map(|()| true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => bail!("apple_login_trust_directory_invalid"),
    }
}

pub(super) fn path_in(root: &Path) -> PathBuf {
    root.join(FILE)
}

/// Trust for exactly this account. Missing trust, or trust for another account, is normal and
/// yields nothing; malformed, insecure or symlinked storage fails closed.
pub(super) fn load_from(root: &Path, email: &str) -> Result<Vec<TrustCookie>> {
    if !root_state(root)? {
        return Ok(Vec::new());
    }
    let path = path_in(root);
    match fs::symlink_metadata(&path) {
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => bail!("apple_login_trust_invalid"),
    }
    let file: TrustFile = serde_json::from_slice(&private_read(&path, MAX_TRUST)?)
        .map_err(|_| anyhow::anyhow!("apple_login_trust_invalid"))?;
    if file.version != 1
        || file.account.len() != 64
        || file.cookies.len() > MAX_COOKIES
        || !file.cookies.iter().all(TrustCookie::valid)
    {
        bail!("apple_login_trust_invalid");
    }
    if file.account != fingerprint(email) {
        return Ok(Vec::new());
    }
    Ok(file
        .cookies
        .into_iter()
        .filter(TrustCookie::unexpired)
        .collect())
}

/// Atomic 0600 write inside a 0700 directory; replaces any other account's trust.
pub(super) fn save_to(root: &Path, email: &str, cookies: &[TrustCookie]) -> Result<PathBuf> {
    root_state(root)?;
    let path = path_in(root);
    if let Ok(m) = fs::symlink_metadata(&path)
        && !m.is_file()
    {
        bail!("apple_login_trust_invalid");
    }
    let cookies: Vec<_> = cookies
        .iter()
        .filter(|c| c.valid() && c.unexpired())
        .take(MAX_COOKIES)
        .collect();
    let data = json!({"version":1,"account":fingerprint(email),"cookies":cookies});
    if serde_json::to_vec_pretty(&data)?.len() > MAX_TRUST {
        bail!("apple_login_trust_too_large");
    }
    atomic_json(&path, &data)?;
    #[cfg(unix)]
    fs::File::open(root)?.sync_all()?;
    Ok(path)
}
