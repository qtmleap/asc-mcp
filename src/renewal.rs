//! Native host renewal: Keychain/trust stay outside Docker; only cookies and sanitized status cross.
use crate::reviewer::{atomic_json, ensure_root, private_read};
use anyhow::{Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const REQUEST: &str = "renewal.request.json";
const STATUS: &str = "renewal.status.json";
const HOST_STATE: &str = "renewal.host.json";
const READ_REFUSED: &str = "renewal.read-refused.json";
const MAX_STATE: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum State {
    Ready,
    NeedsUser,
    RetryLater,
    Busy,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Reason {
    TwoFactor,
    CredentialsRejected,
    AccountAction,
    SecretUnavailable,
    Unsupported,
    AccountMissing,
    AccountMismatch,
    Keychain,
    ReadRefused,
    ProviderChanged,
    Storage,
    Network,
}
impl Reason {
    fn from_error(message: &str) -> Self {
        if message.starts_with("apple_login_session_not_confirmed")
            || message.starts_with("apple_login_invalid_email")
            || message.starts_with("apple_login_primary_email_required")
        {
            Self::AccountMismatch
        } else if message.starts_with("apple_login_keychain_unavailable") {
            Self::Keychain
        } else if message.starts_with("apple_login_2fa_required") {
            Self::TwoFactor
        } else if message.starts_with("apple_login_credentials_rejected")
            || message.starts_with("apple_login_authentication_rejected")
        {
            Self::CredentialsRejected
        } else if message.starts_with("apple_login_account_action_required") {
            Self::AccountAction
        } else if message.starts_with("apple_login_secret_store_unsupported") {
            Self::Unsupported
        } else if message.starts_with("apple_login_secret_unavailable")
            || message.starts_with("apple_login_secret_invalid")
        {
            Self::SecretUnavailable
        } else if message.starts_with("apple_login_provider_") {
            Self::ProviderChanged
        } else if message.starts_with("apple_login_account_required") {
            Self::AccountMissing
        } else if message.starts_with("reviewer_")
            || message.starts_with("apple_login_account_store")
            || message.starts_with("apple_login_trust_")
        {
            Self::Storage
        } else {
            Self::Network
        }
    }
    fn terminal(self) -> bool {
        !matches!(self, Self::Network | Self::Keychain | Self::Storage)
    }
    fn label(self) -> &'static str {
        match self {
            Self::TwoFactor => "Apple verification required",
            Self::CredentialsRejected => "stored password rejected",
            Self::AccountAction => "Apple account action required",
            Self::SecretUnavailable => "saved Keychain password missing or invalid",
            Self::Unsupported => "host secret store unsupported",
            Self::AccountMissing => "initial account enrollment required",
            Self::AccountMismatch => "Apple account identity could not be confirmed",
            Self::Keychain => "Keychain is locked or needs authorization",
            Self::ReadRefused => "Apple rejected a read after session renewal",
            Self::ProviderChanged => "Apple team changed or is unconfirmed",
            Self::Storage => "private authentication storage unavailable",
            Self::Network => "temporary authentication connection failure",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Status {
    version: u8,
    state: State,
    generation: String,
    checked_at: i64,
    retry_after: i64,
    reason: Option<Reason>,
    #[serde(default)]
    request_stamp: Option<u64>,
}
fn read_status(path: &Path) -> Result<Option<Status>> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => bail!("reviewer_renewal_storage_invalid"),
        Ok(_) => (),
    }
    let value: Status = serde_json::from_slice(&private_read(path, MAX_STATE)?)
        .map_err(|_| anyhow::anyhow!("reviewer_renewal_storage_invalid"))?;
    if value.version != 1
        || !(value.generation == "missing"
            || value.generation.len() == 64
                && value.generation.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        bail!("reviewer_renewal_storage_invalid");
    }
    Ok(Some(value))
}
pub(crate) fn source_generation(source: &Path) -> Result<String> {
    if matches!(fs::symlink_metadata(source),Err(e) if e.kind()==std::io::ErrorKind::NotFound) {
        return Ok("missing".into());
    }
    Ok(hex::encode(Sha256::digest(private_read(
        source,
        1024 * 1024,
    )?)))
}
pub(crate) fn configured_wait() -> Result<Duration> {
    let seconds = std::env::var("ASC_REVIEW_RENEWAL_WAIT_SECS")
        .unwrap_or_else(|_| "0".into())
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("invalid_reviewer_renewal_wait"))?;
    if seconds > 30 {
        bail!("invalid_reviewer_renewal_wait: maximum is 30 seconds");
    }
    Ok(Duration::from_secs(seconds))
}
fn action_error(status: &Status) -> Result<()> {
    if let Some(reason) = status.reason {
        bail!(
            "reviewer_auth_required: {}; run script/docker/reviewer-login.sh stack --fresh on the Mac host",
            reason.label()
        );
    }
    bail!("reviewer_auth_required: host renewal requires attention")
}
fn request_stamp(root: &Path) -> Result<Option<u64>> {
    let path = root.join(REQUEST);
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_STATE as u64 => Ok(Some(
            meta.modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
                .try_into()?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => bail!("reviewer_renewal_request_invalid"),
    }
}
pub(crate) async fn wait_for_replacement(
    source: &Path,
    root: &Path,
    generation: &str,
    wait: Duration,
) -> Result<bool> {
    if source_generation(source)? != generation {
        return Ok(true);
    }
    if let Some(status) = read_status(&root.join(STATUS))?
        && status.generation == generation
        && (status.state == State::NeedsUser
            || status.state == State::RetryLater && status.retry_after > Utc::now().timestamp())
    {
        return action_error(&status).map(|_| false);
    }
    let marker = root.join(REQUEST);
    if let Ok(meta) = fs::symlink_metadata(&marker)
        && (!meta.is_file() || meta.len() > MAX_STATE as u64)
    {
        bail!("reviewer_renewal_request_invalid");
    }
    // A fixed advisory marker only. Host never accepts an account, path, command or secret here.
    atomic_json(&marker, &json!({"version":1}))?;
    let sent_stamp = request_stamp(root)?;
    let deadline = tokio::time::Instant::now() + wait.min(Duration::from_secs(30));
    loop {
        if source_generation(source)? != generation {
            return Ok(true);
        }
        if let Some(status) = read_status(&root.join(STATUS))? {
            if status.generation == generation
                && (status.state == State::NeedsUser
                    || status.state == State::RetryLater
                        && status.retry_after > Utc::now().timestamp())
            {
                return action_error(&status).map(|_| false);
            }
            if status.generation == generation
                && status.state == State::Ready
                && status
                    .request_stamp
                    .zip(sent_stamp)
                    .is_some_and(|(ack, sent)| ack >= sent)
            {
                return Ok(true);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "reviewer_auth_required: host renewal unavailable or still running; check the Mac renewal agent"
            );
        }
        tokio::time::sleep(
            Duration::from_millis(10)
                .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
        )
        .await;
    }
}
struct HostLock(fs::File);
impl Drop for HostLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
fn host_lock(root: &Path) -> Result<Option<HostLock>> {
    if let Ok(meta) = fs::symlink_metadata(root) {
        if !meta.is_dir() {
            bail!("reviewer_renewal_private_directory_required");
        }
        #[cfg(unix)]
        if meta.permissions().mode() & 0o077 != 0 {
            bail!("reviewer_renewal_directory_permissions");
        }
    }
    ensure_root(root)?;
    let file = crate::reviewer::open_private_lock(root, ".renewal.lock")?;
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(Some(HostLock(file))),
        Err(e)
            if e.kind() == std::io::ErrorKind::WouldBlock
                || e.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
        {
            Ok(None)
        }
        Err(_) => bail!("reviewer_renewal_lock_unavailable"),
    }
}
async fn run_with<F, Fut>(source: &Path, shared: &Path, host: &Path, auth: F) -> Result<State>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<PathBuf>>,
{
    let Some(_lock) = host_lock(host)? else {
        return Ok(State::Busy);
    };
    let generation = source_generation(source)?;
    let observed_request = request_stamp(shared)?;
    // This container-written refusal is advisory. Invalid data must neither
    // select credentials nor prevent the host from checking its own session.
    if let Ok(Some(refused)) = read_status(&shared.join(READ_REFUSED))
        && refused.generation == generation
        && refused.state == State::NeedsUser
        && matches!(refused.reason, Some(Reason::ReadRefused))
    {
        atomic_json(&host.join(HOST_STATE), &serde_json::to_value(&refused)?)?;
        atomic_json(&shared.join(STATUS), &serde_json::to_value(&refused)?)?;
        return Ok(State::NeedsUser);
    }
    if let Some(status) = read_status(&host.join(HOST_STATE))?
        && status.generation == generation
        && (status.state == State::NeedsUser
            || status.state == State::RetryLater && status.retry_after > Utc::now().timestamp())
    {
        atomic_json(&shared.join(STATUS), &serde_json::to_value(&status)?)?;
        return Ok(status.state);
    }
    let result = tokio::time::timeout(Duration::from_secs(120), auth()).await;
    let (state, reason) = match result {
        Ok(Ok(_)) => (State::Ready, None),
        Ok(Err(e)) => {
            let reason = Reason::from_error(&e.to_string());
            (
                if reason.terminal() {
                    State::NeedsUser
                } else {
                    State::RetryLater
                },
                Some(reason),
            )
        }
        Err(_) => (State::RetryLater, Some(Reason::Network)),
    };
    let now = Utc::now().timestamp();
    let status = Status {
        version: 1,
        state,
        generation: source_generation(source)?,
        checked_at: now,
        retry_after: if state == State::RetryLater {
            now + 300
        } else {
            0
        },
        reason,
        request_stamp: observed_request,
    };
    atomic_json(&host.join(HOST_STATE), &serde_json::to_value(&status)?)?;
    atomic_json(&shared.join(STATUS), &serde_json::to_value(&status)?)?;
    Ok(state)
}
/// Runs on the Mac host, never inside the reviewer MCP request or Linux container.
pub async fn run_once() -> Result<Value> {
    let source = crate::reviewer::session_source()?;
    let shared = crate::reviewer::state_root()?;
    let host = crate::reviewer::trust_root()?;
    let state = run_with(&source, &shared, &host, || async {
        let account = crate::apple_login::resolve_account(None)?
            .ok_or_else(|| anyhow::anyhow!("apple_login_account_required"))?;
        crate::apple_login::login_unattended(&account).await
    })
    .await?;
    Ok(json!({"state":state}))
}
/// A human-confirmed successful login re-arms automatic renewal.
fn clear_after_login(host: &Path) -> Result<()> {
    let status = Status {
        version: 1,
        state: State::Ready,
        generation: source_generation(&crate::reviewer::session_source()?)?,
        checked_at: Utc::now().timestamp(),
        retry_after: 0,
        reason: None,
        request_stamp: None,
    };
    atomic_json(&host.join(HOST_STATE), &serde_json::to_value(&status)?)?;
    atomic_json(
        &crate::reviewer::state_root()?.join(READ_REFUSED),
        &serde_json::to_value(&status)?,
    )?;
    atomic_json(
        &crate::reviewer::state_root()?.join(STATUS),
        &serde_json::to_value(&status)?,
    )?;
    Ok(())
}

/// Serializes human and automatic login attempts on the same host-only lock.
pub async fn interactive_login(
    email: &str,
    fresh: bool,
    accept_provider_change: bool,
) -> Result<PathBuf> {
    let host = crate::reviewer::trust_root()?;
    let Some(_lock) = host_lock(&host)? else {
        bail!("reviewer_renewal_busy: automatic renewal is running; retry after it finishes");
    };
    let source =
        crate::apple_login::login_with_provider_override(email, fresh, accept_provider_change)
            .await?;
    clear_after_login(&host)?;
    Ok(source)
}

pub(crate) fn mark_read_refused(
    source: &Path,
    shared: &Path,
    observed_generation: &str,
) -> Result<()> {
    if source_generation(source)? != observed_generation {
        return Ok(());
    }
    let status = Status {
        version: 1,
        state: State::NeedsUser,
        generation: observed_generation.to_owned(),
        checked_at: Utc::now().timestamp(),
        retry_after: 0,
        reason: Some(Reason::ReadRefused),
        request_stamp: None,
    };
    let value = serde_json::to_value(&status)?;
    atomic_json(&shared.join(READ_REFUSED), &value)?;
    atomic_json(&shared.join(STATUS), &value)?;
    atomic_json(&shared.join(REQUEST), &json!({"version":1}))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reviewer::atomic_json;
    use serde_json::json;

    #[tokio::test]
    async fn renewal_marker_wakes_host_and_observes_new_generation() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("session.cookie");
        atomic_json(&source, &json!({"cookies":[]})).unwrap();
        let old = source_generation(&source).unwrap();
        let replacement = source.clone();
        let marker = dir.path().join("renewal.request.json");
        let host = tokio::spawn(async move {
            for _ in 0..100 {
                if marker.exists() {
                    atomic_json(&replacement, &json!({"cookies":[],"renewed":true})).unwrap();
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        let response =
            wait_for_replacement(&source, dir.path(), &old, Duration::from_millis(500)).await;
        host.await.unwrap();
        assert!(response.unwrap());
    }
    #[tokio::test]
    async fn rejected_credentials_latch_without_repeating_authentication() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("credentials/session.cookie");
        atomic_json(&source, &json!({"cookies":[]})).unwrap();
        let shared = dir.path().join("state");
        let host = dir.path().join("login-only");
        let first = run_with(&source, &shared, &host, || async {
            bail!("apple_login_credentials_rejected")
        })
        .await
        .unwrap();
        assert_eq!(first, State::NeedsUser);
        let second = run_with(&source, &shared, &host, || async {
            panic!("latched password must not be sent again")
        })
        .await
        .unwrap();
        assert_eq!(second, State::NeedsUser);
    }

    #[tokio::test]
    async fn malformed_advisory_read_refusal_does_not_prevent_host_renewal() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("credentials/session.cookie");
        atomic_json(&source, &json!({"cookies":[]})).unwrap();
        let shared = dir.path().join("state");
        ensure_root(&shared).unwrap();
        fs::write(shared.join(READ_REFUSED), b"malformed advisory data").unwrap();
        #[cfg(unix)]
        fs::set_permissions(shared.join(READ_REFUSED), fs::Permissions::from_mode(0o600)).unwrap();
        let state = run_with(&source, &shared, &dir.path().join("host"), || async {
            Ok(source.clone())
        })
        .await
        .unwrap();
        assert_eq!(state, State::Ready);
    }

    #[tokio::test]
    async fn replacement_session_clears_previous_user_action_latch() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("credentials/session.cookie");
        atomic_json(&source, &json!({"cookies":[]})).unwrap();
        let shared = dir.path().join("state");
        let host = dir.path().join("login-only");
        assert_eq!(
            run_with(&source, &shared, &host, || async {
                bail!("apple_login_2fa_required")
            })
            .await
            .unwrap(),
            State::NeedsUser
        );
        atomic_json(&source, &json!({"cookies":[],"new-login":true})).unwrap();
        let result = run_with(&source, &shared, &host, || async { Ok(source.clone()) })
            .await
            .unwrap();
        assert_eq!(result, State::Ready);
    }

    #[tokio::test]
    async fn concurrent_host_jobs_only_authenticate_once() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("credentials/session.cookie");
        atomic_json(&source, &json!({"cookies":[]})).unwrap();
        let shared = dir.path().join("state");
        let host = dir.path().join("login-only");
        let calls = Arc::new(AtomicUsize::new(0));
        let a = calls.clone();
        let b = calls.clone();
        let first = run_with(&source, &shared, &host, || async {
            a.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(25)).await;
            Ok(source.clone())
        });
        let second = run_with(&source, &shared, &host, || async {
            b.fetch_add(1, Ordering::SeqCst);
            Ok(source.clone())
        });
        let (x, y) = tokio::join!(first, second);
        assert_eq!(x.unwrap(), State::Ready);
        assert_eq!(y.unwrap(), State::Busy);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn preflight_acknowledgement_retries_a_read_without_source_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("session.cookie");
        atomic_json(&source, &json!({"cookies":[]})).unwrap();
        let generation = source_generation(&source).unwrap();
        let shared = dir.path().to_owned();
        let stamp_generation = generation.clone();
        let host = tokio::spawn(async move {
            for _ in 0..100 {
                if request_stamp(&shared).unwrap().is_some() {
                    let status = Status {
                        version: 1,
                        state: State::Ready,
                        generation: stamp_generation,
                        checked_at: Utc::now().timestamp(),
                        retry_after: 0,
                        reason: None,
                        request_stamp: request_stamp(&shared).unwrap(),
                    };
                    atomic_json(&shared.join(STATUS), &serde_json::to_value(status).unwrap())
                        .unwrap();
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        let result =
            wait_for_replacement(&source, dir.path(), &generation, Duration::from_millis(300))
                .await;
        host.await.unwrap();
        assert!(result.unwrap());
    }
    #[cfg(unix)]
    #[test]
    fn insecure_host_login_directory_is_rejected_without_permission_repair() {
        let dir = tempfile::tempdir().unwrap();
        let host = dir.path().join("login-only");
        fs::create_dir(&host).unwrap();
        fs::set_permissions(&host, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(host_lock(&host).is_err());
        assert_eq!(
            fs::metadata(&host).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
    #[tokio::test]
    async fn account_mismatch_is_latched_before_a_second_password_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("credentials/session.cookie");
        atomic_json(&source, &json!({"cookies":[]})).unwrap();
        let shared = dir.path().join("state");
        let host = dir.path().join("host");
        assert_eq!(
            run_with(&source, &shared, &host, || async {
                bail!("apple_login_session_not_confirmed")
            })
            .await
            .unwrap(),
            State::NeedsUser
        );
        assert_eq!(
            run_with(&source, &shared, &host, || async {
                panic!("must not repeat mismatched account authentication")
            })
            .await
            .unwrap(),
            State::NeedsUser
        );
    }

    #[tokio::test]
    async fn expired_network_cooldown_can_wake_the_host_again() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("session.cookie");
        atomic_json(&source, &json!({"cookies":[]})).unwrap();
        let generation = source_generation(&source).unwrap();
        let status = Status {
            version: 1,
            state: State::RetryLater,
            generation: generation.clone(),
            checked_at: 0,
            retry_after: 0,
            reason: Some(Reason::Network),
            request_stamp: None,
        };
        atomic_json(
            &dir.path().join(STATUS),
            &serde_json::to_value(status).unwrap(),
        )
        .unwrap();
        let _ =
            wait_for_replacement(&source, dir.path(), &generation, Duration::from_millis(10)).await;
        assert!(
            dir.path().join(REQUEST).exists(),
            "expired cooldown must not prevent an advisory wake"
        );
    }

    #[tokio::test]
    async fn keychain_unavailability_is_transient_and_reported_distinctly() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("credentials/session.cookie");
        atomic_json(&source, &json!({"cookies":[]})).unwrap();
        let shared = dir.path().join("state");
        let host = dir.path().join("host");
        assert_eq!(
            run_with(&source, &shared, &host, || async {
                bail!("apple_login_keychain_unavailable")
            })
            .await
            .unwrap(),
            State::RetryLater
        );
        let status: Value =
            serde_json::from_slice(&private_read(&shared.join(STATUS), MAX_STATE).unwrap())
                .unwrap();
        assert_eq!(status["reason"], "keychain");
    }

    #[tokio::test]
    async fn refused_read_after_ready_preflight_requires_user_action() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("credentials/session.cookie");
        atomic_json(&source, &json!({"cookies":[]})).unwrap();
        let shared = dir.path().join("state");
        let host = dir.path().join("host");
        assert_eq!(
            run_with(&source, &shared, &host, || async { Ok(source.clone()) })
                .await
                .unwrap(),
            State::Ready
        );
        mark_read_refused(&source, &shared, &source_generation(&source).unwrap()).unwrap();
        assert_eq!(
            run_with(&source, &shared, &host, || async {
                panic!("a refused replay must stop automatic authentication")
            })
            .await
            .unwrap(),
            State::NeedsUser
        );
        let status: Value =
            serde_json::from_slice(&private_read(&shared.join(STATUS), MAX_STATE).unwrap())
                .unwrap();
        assert_eq!(status["reason"], "read_refused");
    }
}
