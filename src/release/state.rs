use crate::developer::fs_safe;
use anyhow::{Result, anyhow, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    path::{Path, PathBuf},
};

const MAX_RECEIPT: usize = 16 << 10;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub app_id: String,
    pub version_id: String,
    pub expected_version_string: String,
    pub expected_build_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    pub status: String,
    pub request_id: Option<String>,
    pub reason: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema: u8,
    pub target: Target,
    pub token_sha256: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub attempted: bool,
    pub outcome: Option<Outcome>,
}

pub struct Store {
    root: PathBuf,
    key: String,
    version_id: String,
    _lock: File,
}

impl Store {
    pub fn open(root: &Path, version_id: &str) -> Result<Self> {
        let key = hex::encode(Sha256::digest(version_id.as_bytes()));
        let lock = fs_safe::private_lock(root, &format!("release-{key}.lock"))?;
        lock.try_lock_exclusive()
            .map_err(|_| anyhow!("release_busy"))?;
        Ok(Self {
            root: root.to_path_buf(),
            key,
            version_id: version_id.to_string(),
            _lock: lock,
        })
    }

    fn path(&self) -> PathBuf {
        self.root.join(format!("release-{}.json", self.key))
    }

    pub fn load(&self) -> Result<Option<Receipt>> {
        let path = self.path();
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => bail!("release_state_unreadable"),
            Ok(_) => (),
        }
        let bytes = fs_safe::private_read(&path, MAX_RECEIPT)?;
        let receipt: Receipt =
            serde_json::from_slice(&bytes).map_err(|_| anyhow!("release_receipt_invalid"))?;
        let hash_valid = receipt.token_sha256.len() == 64
            && receipt
                .token_sha256
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        if receipt.schema != 1 || !hash_valid || receipt.target.version_id != self.version_id {
            bail!("release_receipt_invalid");
        }
        Ok(Some(receipt))
    }

    pub fn save(&self, receipt: &Receipt) -> Result<()> {
        let bytes = serde_json::to_vec(receipt)?;
        let path = self.path();
        fs_safe::atomic_write(&path, &bytes)?;
        if fs_safe::private_read(&path, MAX_RECEIPT)? != bytes {
            bail!("release_state_unwritable");
        }
        Ok(())
    }
}
