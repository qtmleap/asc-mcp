use super::{PortalCookie, cookies, fs_safe, state};
use crate::reviewer::{SharedCookie, SharedSessionHandle, SharedSnapshot};
use anyhow::{Result, bail};
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};
use url::Url;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Suppression {
    name: String,
    expected: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SharedOverlay {
    schema: u32,
    identity: String,
    seed: String,
    cookies: Vec<PortalCookie>,
    suppressed: Vec<Suppression>,
    revoked: BTreeSet<String>,
}

pub(super) struct SharedState {
    handle: SharedSessionHandle,
    snapshot: SharedSnapshot,
    overlay: SharedOverlay,
    negative_changed: bool,
}

fn fingerprint(value: &str) -> String {
    state::digest(value.as_bytes())
}

fn seed(snapshot: &SharedSnapshot) -> Result<String> {
    snapshot
        .cookies
        .iter()
        .find(|cookie| cookie.name == "myacinfo")
        .map(|cookie| fingerprint(&cookie.value))
        .ok_or_else(|| anyhow::anyhow!("developer_session_required"))
}

fn digest_valid(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

impl SharedOverlay {
    fn fresh(snapshot: &SharedSnapshot) -> Result<Self> {
        Ok(Self {
            schema: 1,
            identity: snapshot.identity.clone(),
            seed: seed(snapshot)?,
            cookies: vec![],
            suppressed: vec![],
            revoked: BTreeSet::new(),
        })
    }

    fn validate(&self) -> Result<()> {
        let mut names = BTreeSet::new();
        if self.schema != 1
            || !digest_valid(&self.identity)
            || !digest_valid(&self.seed)
            || self.suppressed.len() > 150
            || self.revoked.len() > 1024
            || self.revoked.iter().any(|value| !digest_valid(value))
            || self.suppressed.iter().any(|entry| {
                !digest_valid(&entry.expected)
                    || entry.name.is_empty()
                    || entry.name.len() > 256
                    || !entry.name.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
                    })
                    || !names.insert(&entry.name)
            })
        {
            bail!("developer_overlay_invalid");
        }
        cookies::validate_jar(&self.cookies)
    }

    fn suppressed(&self, cookie: &SharedCookie) -> bool {
        self.suppressed
            .iter()
            .any(|entry| entry.name == cookie.name && entry.expected == fingerprint(&cookie.value))
    }

    fn rebase(&mut self, snapshot: &SharedSnapshot) {
        let stale = self
            .suppressed
            .iter()
            .filter(|entry| {
                !snapshot.cookies.iter().any(|cookie| {
                    cookie.name == entry.name && fingerprint(&cookie.value) == entry.expected
                })
            })
            .map(|entry| entry.name.clone())
            .collect::<BTreeSet<_>>();
        self.suppressed.retain(|entry| !stale.contains(&entry.name));
        self.cookies.retain(|cookie| {
            !(cookie.domain == "apple.com" && cookie.path == "/" && stale.contains(&cookie.name))
        });
    }
}

impl SharedState {
    pub fn load(root: &Path, handle: SharedSessionHandle) -> Result<Self> {
        let snapshot = handle.snapshot()?;
        let mut overlay = SharedOverlay::fresh(&snapshot)?;
        if let Some(bytes) = state::optional_read(&root.join("shared-overlay.json"), 1 << 20)? {
            let stored: SharedOverlay = serde_json::from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("developer_overlay_invalid"))?;
            stored.validate()?;
            if stored.identity == snapshot.identity {
                overlay = stored;
                if overlay.seed != seed(&snapshot)? {
                    overlay.seed = seed(&snapshot)?;
                    overlay.cookies.clear();
                    overlay.suppressed.clear();
                }
                overlay.rebase(&snapshot);
            }
        }
        let shared = Self {
            handle,
            snapshot,
            overlay,
            negative_changed: false,
        };
        shared.header(
            &shared.overlay.cookies,
            &Url::parse("https://developer.apple.com/")?,
        )?;
        Ok(shared)
    }

    pub fn identity(&self) -> &str {
        &self.snapshot.identity
    }

    pub fn response_cookies(&self) -> Vec<PortalCookie> {
        self.overlay.cookies.clone()
    }

    pub fn secrets(&self) -> BTreeSet<String> {
        self.snapshot
            .secrets
            .iter()
            .cloned()
            .chain(
                self.overlay
                    .cookies
                    .iter()
                    .map(|cookie| cookie.value.clone()),
            )
            .collect()
    }

    pub fn ensure_current(&self) -> Result<()> {
        self.handle.ensure_current(&self.snapshot)
    }

    pub fn header(&self, responses: &[PortalCookie], target: &Url) -> Result<String> {
        Self::build_header(&self.snapshot, &self.overlay, responses, target)
    }

    fn build_header(
        snapshot: &SharedSnapshot,
        overlay: &SharedOverlay,
        responses: &[PortalCookie],
        target: &Url,
    ) -> Result<String> {
        if target.scheme() != "https"
            || target.host_str() != Some("developer.apple.com")
            || overlay.revoked.contains(&seed(snapshot)?)
        {
            bail!("developer_session_required");
        }
        cookies::validate_jar(responses)?;
        let mut selected = Vec::new();
        let mut authentication = 0;
        for cookie in &snapshot.cookies {
            if overlay.suppressed(cookie) {
                continue;
            }
            if responses.iter().any(|response| {
                response.applies(target)
                    && response.domain == "apple.com"
                    && response.path == "/"
                    && response.name == cookie.name
            }) {
                bail!("developer_cookie_invalid");
            }
            authentication += usize::from(cookie.name == "myacinfo");
            selected.push(format!("{}={}", cookie.name, cookie.value));
        }
        for cookie in responses.iter().filter(|cookie| cookie.applies(target)) {
            authentication += usize::from(cookie.name == "myacinfo");
            selected.push(format!("{}={}", cookie.name, cookie.value));
        }
        if authentication != 1 {
            bail!("developer_session_required");
        }
        Ok(selected.join("; "))
    }

    pub fn rotate(
        &mut self,
        responses: &mut Vec<PortalCookie>,
        headers: &HeaderMap,
        target: &Url,
    ) -> Result<()> {
        let sent = self
            .snapshot
            .cookies
            .iter()
            .filter(|cookie| !self.overlay.suppressed(cookie))
            .map(|cookie| cookie.name.clone())
            .collect::<BTreeSet<_>>();
        let mut candidate = responses.clone();
        let mut overlay = self.overlay.clone();
        let rotation = cookies::rotate(&mut candidate, headers, target);
        let mut updates = Vec::new();
        for raw in headers.get_all(reqwest::header::SET_COOKIE) {
            let Ok((cookie, deleting)) = cookies::response_cookie(raw, target) else {
                continue;
            };
            if cookie.domain != "apple.com" || cookie.path != "/" {
                continue;
            }
            let Some(inherited) = self
                .snapshot
                .cookies
                .iter()
                .find(|old| old.name == cookie.name)
            else {
                continue;
            };
            if deleting {
                self.negative_changed = true;
                overlay.suppressed.retain(|entry| entry.name != cookie.name);
                overlay.suppressed.push(Suppression {
                    name: cookie.name.clone(),
                    expected: fingerprint(&inherited.value),
                });
                if cookie.name == "myacinfo" {
                    overlay.revoked.insert(seed(&self.snapshot)?);
                }
            } else if sent.contains(&cookie.name) {
                updates.push(SharedCookie {
                    name: cookie.name,
                    value: cookie.value,
                    expires: cookie.expires,
                });
            }
        }
        responses.retain(|old| {
            !headers
                .get_all(reqwest::header::SET_COOKIE)
                .iter()
                .any(|raw| {
                    cookies::response_cookie(raw, target)
                        .is_ok_and(|(cookie, deleting)| deleting && cookie.key() == old.key())
                })
        });
        self.overlay.suppressed = overlay.suppressed.clone();
        self.overlay.revoked = overlay.revoked.clone();
        self.ensure_current()?;
        rotation?;
        candidate.retain(|cookie| {
            !(cookie.domain == "apple.com" && cookie.path == "/" && sent.contains(&cookie.name))
        });
        for update in &updates {
            overlay.suppressed.retain(|entry| entry.name != update.name);
        }
        let mut next = self.snapshot.clone();
        for update in &updates {
            let old = next
                .cookies
                .iter_mut()
                .find(|cookie| cookie.name == update.name)
                .ok_or_else(|| anyhow::anyhow!("developer_cookie_invalid"))?;
            *old = update.clone();
        }
        Self::build_header(&next, &overlay, &candidate, target)?;
        if !updates.is_empty() {
            next = self.handle.rotate(&self.snapshot, &updates)?;
        }
        overlay.seed = seed(&next)?;
        overlay.cookies = candidate.clone();
        self.snapshot = next;
        self.overlay = overlay;
        *responses = candidate;
        Ok(())
    }

    pub fn persist(&self, root: &Path, responses: &[PortalCookie]) -> Result<()> {
        let mut overlay = self.overlay.clone();
        overlay.cookies = responses.to_vec();
        overlay.validate()?;
        let bytes =
            serde_json::to_vec(&overlay).map_err(|_| anyhow::anyhow!("developer_state_invalid"))?;
        if bytes.len() > 1 << 20 {
            bail!("developer_state_too_large");
        }
        if self.negative_changed {
            return self
                .handle
                .with_current_authentication(&self.snapshot, |current| {
                    fs_safe::atomic_write(&root.join("shared-overlay.json"), &bytes)?;
                    if current.cookies != self.snapshot.cookies {
                        bail!("shared_session_changed");
                    }
                    Ok(())
                });
        }
        self.ensure_current()?;
        fs_safe::atomic_write(&root.join("shared-overlay.json"), &bytes)
    }
}
