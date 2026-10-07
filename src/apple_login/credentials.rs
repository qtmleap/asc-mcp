//! Private remembered account and host-native password storage. Neither is exposed to MCP.
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};
use zeroize::Zeroizing;

const ACCOUNT_FILE: &str = "account.json";
const MAX_ACCOUNT: usize = 4096;
#[cfg(target_os = "macos")]
const SERVICE: &str = "asc-mcp.apple-account";

#[derive(Clone, Copy)]
pub(super) enum Access {
    Interactive,
    Unattended,
}

/// Secret boundaries are injected in tests; the production store is native macOS Keychain.
pub(super) trait SecretStore {
    fn supported(&self) -> bool;
    fn get(&self, account: &str, access: Access) -> Result<Option<Zeroizing<String>>>;
    fn set(&self, account: &str, password: &str) -> Result<()>;
}
pub(super) struct NativeStore;

pub(super) fn normalize_account(account: &str) -> Result<String> {
    ensure!(
        !account.is_empty() && account.len() <= 320 && !account.chars().any(char::is_control),
        "apple_login_invalid_email"
    );
    let account = account.trim().to_lowercase();
    ensure!(
        !account.is_empty() && account.contains('@'),
        "apple_login_primary_email_required"
    );
    Ok(account)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountFile {
    version: u32,
    account: String,
    #[serde(default)]
    provider_id: Option<u64>,
}

fn private_root_exists(root: &Path) -> Result<bool> {
    match fs::symlink_metadata(root) {
        Ok(meta) => {
            ensure!(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "apple_login_account_store_invalid"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                ensure!(
                    meta.permissions().mode() & 0o077 == 0,
                    "apple_login_account_store_permissions"
                );
            }
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => bail!("apple_login_account_store_invalid"),
    }
}

fn read_account(root: &Path) -> Result<Option<AccountFile>> {
    if !private_root_exists(root)? {
        return Ok(None);
    }
    let path = root.join(ACCOUNT_FILE);
    match fs::symlink_metadata(&path) {
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => bail!("apple_login_account_store_invalid"),
    }
    let mut data: AccountFile =
        serde_json::from_slice(&crate::reviewer::private_read(&path, MAX_ACCOUNT)?)
            .map_err(|_| anyhow::anyhow!("apple_login_account_store_invalid"))?;
    ensure!(
        data.version == 1 && data.provider_id != Some(0),
        "apple_login_account_store_invalid"
    );
    data.account = normalize_account(&data.account)?;
    Ok(Some(data))
}
pub(super) fn resolve_from(root: &Path, explicit: Option<&str>) -> Result<Option<String>> {
    if let Some(account) = explicit {
        return normalize_account(account).map(Some);
    }
    Ok(read_account(root)?.map(|data| data.account))
}
pub(super) fn pinned_provider(root: &Path, account: &str) -> Result<Option<u64>> {
    let account = normalize_account(account)?;
    Ok(read_account(root)?
        .filter(|data| data.account == account)
        .and_then(|data| data.provider_id))
}
#[cfg(test)]
pub(super) fn remember_account(root: &Path, account: &str) -> Result<()> {
    remember_identity(root, account, None)
}
pub(super) fn remember_identity(
    root: &Path,
    account: &str,
    provider_id: Option<u64>,
) -> Result<()> {
    let account = normalize_account(account)?;
    ensure!(provider_id != Some(0), "apple_login_provider_required");
    private_root_exists(root)?;
    let path = root.join(ACCOUNT_FILE);
    if let Ok(meta) = fs::symlink_metadata(&path) {
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "apple_login_account_store_invalid"
        );
    }
    crate::reviewer::atomic_json(
        &path,
        &serde_json::json!({"version":1,"account":account,"provider_id":provider_id}),
    )?;
    Ok(())
}

/// Unattended access reports missing enrollment instead of falling back to a prompt.
pub(super) fn load_password(
    store: &impl SecretStore,
    account: &str,
    access: Access,
) -> Result<Option<Zeroizing<String>>> {
    let account = normalize_account(account)?;
    if !store.supported() {
        return match access {
            Access::Interactive => Ok(None),
            Access::Unattended => bail!(
                "apple_login_secret_store_unsupported: automatic renewal requires native macOS Keychain"
            ),
        };
    }
    let password = store.get(&account, access).map_err(|error| {
        // Only our typed invalid-payload marker is terminal. Permission/I/O/API failures
        // are retryable reads; never echo an underlying Keychain error or try SRP without a secret.
        match error.downcast_ref::<SecretReadFailure>() {
            #[cfg(any(target_os = "macos", test))]
            Some(SecretReadFailure::InvalidSecret) => {
                anyhow::Error::from(SecretReadFailure::InvalidSecret)
            }
            _ => anyhow::Error::from(SecretReadFailure::KeychainUnavailable),
        }
    })?;
    if matches!(access, Access::Unattended) && password.is_none() {
        bail!(
            "apple_login_secret_unavailable: run native reviewer-login --fresh once to save the password in Keychain"
        );
    }
    if let Some(value) = &password {
        ensure!(
            !value.is_empty() && value.len() <= 4096,
            "apple_login_secret_invalid"
        );
    }
    Ok(password)
}

pub(super) fn save_password(store: &impl SecretStore, account: &str, password: &str) -> Result<()> {
    if store.supported() {
        store.set(&normalize_account(account)?, password)
            .map_err(|_| anyhow::anyhow!("apple_login_secret_save_failed: grant Keychain access and run native reviewer-login --fresh"))?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum SecretReadFailure {
    KeychainUnavailable,
    #[cfg(any(target_os = "macos", test))]
    InvalidSecret,
}
impl std::fmt::Display for SecretReadFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::KeychainUnavailable => "apple_login_keychain_unavailable: unlock or authorize Keychain; automatic renewal will retry without a password prompt",
            #[cfg(any(target_os = "macos", test))]
            Self::InvalidSecret => "apple_login_secret_invalid: run native reviewer-login --fresh to replace the saved password",
        })
    }
}
impl std::error::Error for SecretReadFailure {}

/// Security.framework read boundary. SecBase.h defines errSecItemNotFound=-25300.
/// Other OSStatus values represent a failed Keychain read, not rejected Apple credentials.
#[cfg(any(target_os = "macos", test))]
fn decode_keychain_result(
    result: std::result::Result<Vec<u8>, i32>,
) -> Result<Option<Zeroizing<String>>> {
    let bytes = match result {
        Ok(bytes) => Zeroizing::new(bytes),
        Err(-25300) => return Ok(None),
        Err(_) => return Err(SecretReadFailure::KeychainUnavailable.into()),
    };
    if bytes.is_empty() || bytes.len() > 4096 {
        return Err(SecretReadFailure::InvalidSecret.into());
    }
    let value = std::str::from_utf8(&bytes).map_err(|_| SecretReadFailure::InvalidSecret)?;
    Ok(Some(Zeroizing::new(value.to_owned())))
}

#[cfg(target_os = "macos")]
impl SecretStore for NativeStore {
    fn supported(&self) -> bool {
        true
    }
    fn get(&self, account: &str, access: Access) -> Result<Option<Zeroizing<String>>> {
        use security_framework::{
            os::macos::keychain::SecKeychain, passwords::get_generic_password,
        };
        // Security.framework's user-interaction switch is process-global; serialize all our
        // Keychain operations so an interactive read cannot re-enable UI during a headless read.
        let _serial = KEYCHAIN_LOCK
            .lock()
            .map_err(|_| SecretReadFailure::KeychainUnavailable)?;
        let _no_ui = if matches!(access, Access::Unattended) {
            Some(
                SecKeychain::disable_user_interaction()
                    .map_err(|_| SecretReadFailure::KeychainUnavailable)?,
            )
        } else {
            None
        };
        decode_keychain_result(get_generic_password(SERVICE, account).map_err(|error| error.code()))
    }
    fn set(&self, account: &str, password: &str) -> Result<()> {
        let _serial = KEYCHAIN_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("apple_login_secret_save_failed"))?;
        security_framework::passwords::set_generic_password(SERVICE, account, password.as_bytes())
            .map_err(|_| anyhow::anyhow!("apple_login_secret_save_failed: grant Keychain access and run native reviewer-login --fresh"))
    }
}
#[cfg(target_os = "macos")]
static KEYCHAIN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(not(target_os = "macos"))]
impl SecretStore for NativeStore {
    fn supported(&self) -> bool {
        false
    }
    fn get(&self, _: &str, _: Access) -> Result<Option<Zeroizing<String>>> {
        bail!("apple_login_secret_store_unsupported")
    }
    fn set(&self, _: &str, _: &str) -> Result<()> {
        bail!("apple_login_secret_store_unsupported")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct FakeStore {
        supported: bool,
        missing: bool,
    }
    impl SecretStore for FakeStore {
        fn supported(&self) -> bool {
            self.supported
        }
        fn get(&self, account: &str, access: Access) -> Result<Option<Zeroizing<String>>> {
            assert_eq!(account, "other@example.invalid");
            assert!(matches!(access, Access::Unattended));
            Ok((!self.missing).then(|| Zeroizing::new("synthetic-secret".into())))
        }
        fn set(&self, account: &str, password: &str) -> Result<()> {
            assert_eq!(account, "other@example.invalid");
            assert_eq!(password, "synthetic-secret");
            Ok(())
        }
    }
    #[test]
    fn recovery_password_lookup_uses_exact_normalized_account() {
        let store = FakeStore {
            supported: true,
            missing: false,
        };
        let secret = load_password(&store, " Other@Example.invalid ", Access::Unattended)
            .unwrap()
            .unwrap();
        assert_eq!(&*secret, "synthetic-secret");
    }
    #[test]
    fn recovery_unattended_missing_password_reports_enrollment() {
        let store = FakeStore {
            supported: true,
            missing: true,
        };
        assert!(load_password(&store, "other@example.invalid", Access::Unattended).is_err());
    }
    #[test]
    fn recovery_unsupported_secret_store_keeps_manual_login_only() {
        let store = FakeStore {
            supported: false,
            missing: false,
        };
        assert!(
            load_password(&store, "other@example.invalid", Access::Interactive)
                .unwrap()
                .is_none()
        );
        assert!(load_password(&store, "other@example.invalid", Access::Unattended).is_err());
    }
    struct RefusingStore;
    impl SecretStore for RefusingStore {
        fn supported(&self) -> bool {
            true
        }
        fn get(&self, _: &str, _: Access) -> Result<Option<Zeroizing<String>>> {
            bail!("backend echoed secret-sentinel")
        }
        fn set(&self, _: &str, _: &str) -> Result<()> {
            bail!("backend echoed secret-sentinel")
        }
    }
    #[test]
    fn recovery_secret_store_errors_do_not_echo_backend_data() {
        let error = load_password(&RefusingStore, "a@x.y", Access::Unattended)
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("apple_login_keychain_unavailable"));
        assert!(!error.contains("secret-sentinel"));
        let error = save_password(&RefusingStore, "a@x.y", "secret-sentinel")
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("apple_login_secret_save_failed"));
        assert!(!error.contains("secret-sentinel"));
    }
    struct TemporarilyLockedStore;
    impl SecretStore for TemporarilyLockedStore {
        fn supported(&self) -> bool {
            true
        }
        fn get(&self, _: &str, _: Access) -> Result<Option<Zeroizing<String>>> {
            bail!("permission denied while Keychain locked: secret-sentinel")
        }
        fn set(&self, _: &str, _: &str) -> Result<()> {
            bail!("must not write during a failed read")
        }
    }
    #[test]
    fn recovery_keychain_read_failure_is_transient_without_echoing_backend_data() {
        let error = load_password(&TemporarilyLockedStore, "a@x.y", Access::Unattended)
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("apple_login_keychain_unavailable"),
            "{error}"
        );
        assert!(!error.contains("secret-sentinel"));
    }
    #[test]
    fn recovery_native_keychain_codes_preserve_missing_vs_transient_reads() {
        assert!(decode_keychain_result(Err(-25300)).unwrap().is_none());
        for code in [
            -128, -25291, -25293, -25294, -25295, -25307, -25308, -25315, -26275, -50,
        ] {
            let error = decode_keychain_result(Err(code)).unwrap_err().to_string();
            assert!(
                error.starts_with("apple_login_keychain_unavailable"),
                "OSStatus{code}: {error}"
            );
            assert!(!error.contains(&code.to_string()));
        }
    }
    #[test]
    fn recovery_native_keychain_invalid_returned_secret_is_terminal() {
        struct InvalidPayload;
        impl SecretStore for InvalidPayload {
            fn supported(&self) -> bool {
                true
            }
            fn get(&self, _: &str, _: Access) -> Result<Option<Zeroizing<String>>> {
                decode_keychain_result(Ok(vec![0xff, 0xfe]))
            }
            fn set(&self, _: &str, _: &str) -> Result<()> {
                bail!("unexpected write")
            }
        }
        let error = load_password(&InvalidPayload, "a@x.y", Access::Unattended)
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("apple_login_secret_invalid"), "{error}");
        for bytes in [Vec::new(), vec![b'a'; 4097]] {
            let error = decode_keychain_result(Ok(bytes)).unwrap_err().to_string();
            assert!(error.starts_with("apple_login_secret_invalid"), "{error}");
        }
    }
    #[test]
    fn recovery_remembered_account_store_rejects_symlinks_and_insecure_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("trust");
        remember_account(&root, "a@x.y").unwrap();
        let path = root.join("account.json");
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(resolve_from(&root, None).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let link = d.path().join("link");
        symlink(&root, &link).unwrap();
        assert!(resolve_from(&link, None).is_err());
        fs::remove_file(&path).unwrap();
        symlink(d.path().join("other"), &path).unwrap();
        assert!(resolve_from(&root, None).is_err());
        assert!(remember_account(&root, "a@x.y").is_err());
    }
    #[test]
    fn recovery_remembered_account_is_used_without_an_explicit_account() {
        let d = tempfile::tempdir().unwrap();
        crate::reviewer::atomic_json(
            &d.path().join("account.json"),
            &json!({"version":1,"account":"remembered@example.invalid"}),
        )
        .unwrap();
        assert_eq!(
            resolve_from(d.path(), None).unwrap().as_deref(),
            Some("remembered@example.invalid")
        );
    }
    #[test]
    fn recovery_explicit_account_overrides_memory_and_is_normalized() {
        let d = tempfile::tempdir().unwrap();
        crate::reviewer::atomic_json(
            &d.path().join("account.json"),
            &json!({"version":1,"account":"remembered@example.invalid"}),
        )
        .unwrap();
        assert_eq!(
            resolve_from(d.path(), Some(" Other@Example.invalid "))
                .unwrap()
                .as_deref(),
            Some("other@example.invalid")
        );
    }
    #[test]
    fn recovery_malformed_account_store_fails_closed() {
        let d = tempfile::tempdir().unwrap();
        crate::reviewer::atomic_json(
            &d.path().join("account.json"),
            &json!({"version":1,"account":"injected\n@x.y"}),
        )
        .unwrap();
        assert!(resolve_from(d.path(), None).is_err());
    }
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires an unlocked interactive macOS Keychain; uses a disposable fixture item"]
    fn native_keychain_fixture_round_trip_without_unattended_ui() {
        let account = format!("asc-fixture-{}@example.invalid", uuid::Uuid::new_v4());
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = security_framework::passwords::delete_generic_password(SERVICE, &self.0);
            }
        }
        let _cleanup = Cleanup(account.clone());
        NativeStore
            .set(&account, "synthetic-keychain-fixture")
            .unwrap();
        let secret = NativeStore
            .get(&account, Access::Unattended)
            .unwrap()
            .unwrap();
        assert_eq!(&*secret, "synthetic-keychain-fixture");
    }
}
