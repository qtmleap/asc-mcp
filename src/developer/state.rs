use super::groups::{Kind, Plan};
use super::{PortalCookie, cookies, fs_safe};
use anyhow::{Result, bail};
use fs2::FileExt;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Source {
    pub schema: u32,
    pub login_id: String,
    pub team_id: String,
    pub cookies: Vec<PortalCookie>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Overlay {
    schema: u32,
    generation: String,
    cookies: Vec<PortalCookie>,
}

pub(super) struct Transaction {
    pub _lock: File,
    pub root: PathBuf,
    pub source_path: PathBuf,
    pub source: Source,
    pub generation: String,
    pub cookies: Vec<PortalCookie>,
    pub csrf: Option<(String, String)>,
    pub secrets: std::collections::BTreeSet<String>,
}

pub(super) fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Delivery {
    Prepared,
    Attempting,
    Verified,
    Rejected,
    #[serde(rename = "delivery_unknown")]
    Unknown,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    pub schema: u32,
    pub operation_id: String,
    pub plan: Plan,
    pub token_hash: String,
    pub plan_digest: String,
    pub state: Delivery,
    pub created_at: i64,
    pub expires_at: i64,
    pub observation: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipts {
    schema: u32,
    pub records: Vec<Receipt>,
}

impl Receipt {
    fn valid(&self) -> bool {
        let hash =
            |value: &str| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
        self.schema == 1
            && uuid::Uuid::parse_str(&self.operation_id).is_ok()
            && self.plan.valid()
            && hash(&self.token_hash)
            && hash(&self.plan_digest)
            && serde_json::to_vec(&self.plan).is_ok_and(|bytes| digest(&bytes) == self.plan_digest)
            && self.created_at > 0
            && self.created_at.checked_add(900) == Some(self.expires_at)
            && matches!(
                self.observation.as_str(),
                "prepared"
                    | "attempting"
                    | "verified"
                    | "apple_rejected"
                    | "delivery_unknown"
                    | "interrupted"
                    | "desired_state_present"
                    | "desired_state_absent"
                    | "observation_unavailable"
            )
            && serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= 64 << 10)
    }

    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({"operation_id":self.operation_id,"state":self.state,"team_id":self.plan.team,"identifier":self.plan.identifier,
            "bundle_id":self.plan.bundle_id,"observation":self.observation,"causation_verified":self.state == Delivery::Verified,
            "write_blocked":matches!(self.state,Delivery::Attempting|Delivery::Unknown)})
    }
}

impl Receipts {
    pub fn load(portal: &super::DeveloperPortal) -> Result<Self> {
        let mut store = match optional_read(
            &portal.root.join("receipts.json"),
            2048 * (64 << 10) + (1 << 20),
        )? {
            Some(bytes) => serde_json::from_slice::<Self>(&bytes)
                .map_err(|_| anyhow::anyhow!("developer_receipt_invalid"))?,
            None => Self {
                schema: 1,
                records: vec![],
            },
        };
        let mut ids = std::collections::BTreeSet::new();
        if store.schema != 1
            || store.records.len() > 2048
            || store
                .records
                .iter()
                .any(|record| !record.valid() || !ids.insert(record.operation_id.clone()))
        {
            bail!("developer_receipt_invalid");
        }
        let mut changed = false;
        for record in &mut store.records {
            if record.state == Delivery::Attempting {
                record.state = Delivery::Unknown;
                record.observation = "interrupted".into();
                changed = true;
            }
        }
        let stamp = chrono::Utc::now().timestamp();
        let before = store.records.len();
        store.records.retain(|record| {
            !(record.state == Delivery::Prepared && record.expires_at <= stamp
                || matches!(record.state, Delivery::Verified | Delivery::Rejected)
                    && record.created_at < stamp - 30 * 86400)
        });
        if changed || before != store.records.len() {
            store.save(portal)?;
        }
        Ok(store)
    }

    pub fn save(&self, portal: &super::DeveloperPortal) -> Result<()> {
        #[cfg(test)]
        {
            let remaining = portal
                .fail_receipt
                .load(std::sync::atomic::Ordering::SeqCst);
            if remaining > 0
                && portal
                    .fail_receipt
                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst)
                    == 1
            {
                bail!("developer_state_unwritable");
            }
        }
        if self.records.len() > 2048 || self.records.iter().any(|record| !record.valid()) {
            bail!("developer_receipt_invalid");
        }
        let bytes =
            serde_json::to_vec(self).map_err(|_| anyhow::anyhow!("developer_receipt_invalid"))?;
        let path = portal.root.join("receipts.json");
        fs_safe::atomic_write(&path, &bytes)?;
        if fs_safe::private_read(&path, 2048 * (64 << 10) + (1 << 20))? != bytes {
            bail!("developer_receipt_unverified");
        }
        Ok(())
    }

    pub fn check_conflict(&self, plan: &Plan) -> Result<()> {
        if self.records.iter().any(|record| {
            matches!(record.state, Delivery::Attempting | Delivery::Unknown)
                && record.plan.key() == plan.key()
        }) {
            bail!("developer_target_delivery_unknown");
        }
        Ok(())
    }

    pub fn prepare(
        &mut self,
        portal: &super::DeveloperPortal,
        plan: Plan,
    ) -> Result<serde_json::Value> {
        self.check_conflict(&plan)?;
        if self.records.len() >= 2048 {
            bail!("developer_receipt_capacity");
        }
        let mut random = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut random);
        let token = hex::encode(random);
        let stamp = chrono::Utc::now().timestamp();
        let receipt = Receipt {
            schema: 1,
            operation_id: uuid::Uuid::new_v4().to_string(),
            plan_digest: digest(
                &serde_json::to_vec(&plan)
                    .map_err(|_| anyhow::anyhow!("developer_plan_invalid"))?,
            ),
            token_hash: digest(token.as_bytes()),
            plan,
            state: Delivery::Prepared,
            created_at: stamp,
            expires_at: stamp + 900,
            observation: "prepared".into(),
        };
        let output = serde_json::json!({"state":"prepared","operation_id":receipt.operation_id,"confirmation_token":token,
            "expires_at":receipt.expires_at,"team_id":receipt.plan.team,"identifier":receipt.plan.identifier,"name":receipt.plan.name,
            "bundle_id":receipt.plan.bundle_id,"existing_group_ids":receipt.plan.baseline,"proposed_group_ids":receipt.plan.expected,
            "warning":if receipt.plan.kind == Kind::Associate { "Private API replaces associations; an external Portal or Xcode writer can race between verification and replacement. Confirm the complete proposed set." } else { "Experimental undocumented Developer Portal write; confirmation token does not attest human approval." }});
        self.records.push(receipt);
        self.save(portal)?;
        Ok(output)
    }

    pub fn confirmed(&self, args: &super::groups::WriteArgs, generation: &str) -> Result<usize> {
        let index = self
            .records
            .iter()
            .position(|record| Some(&record.operation_id) == args.operation_id.as_ref())
            .ok_or_else(|| anyhow::anyhow!("developer_confirmation_invalid"))?;
        let record = &self.records[index];
        if record.state != Delivery::Prepared
            || record.expires_at <= chrono::Utc::now().timestamp()
            || record.plan.generation != generation
            || record.token_hash != digest(args.token.as_deref().unwrap_or_default().as_bytes())
            || record.plan.kind != args.plan.kind
            || record.plan.team != args.plan.team
            || record.plan.identifier != args.plan.identifier
            || record.plan.name != args.plan.name
            || record.plan.bundle_id != args.plan.bundle_id
        {
            bail!("developer_confirmation_invalid");
        }
        Ok(index)
    }
}

pub(super) fn acquire(root: &Path) -> Result<File> {
    let file = fs_safe::private_lock(root, ".developer.lock")?;
    file.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("developer_busy"))?;
    Ok(file)
}

pub(super) fn optional_read(path: &Path, max: usize) -> Result<Option<Vec<u8>>> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => bail!("developer_state_unreadable"),
        Ok(_) => fs_safe::private_read(path, max).map(Some),
    }
}

impl Transaction {
    pub fn load(root: &Path, source_path: &Path) -> Result<Self> {
        let lock = acquire(root)?;
        let bytes = fs_safe::private_read(source_path, 1 << 20)?;
        let source: Source = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("developer_session_invalid"))?;
        if source.schema != 1
            || uuid::Uuid::parse_str(&source.login_id).is_err()
            || super::validate_team(&source.team_id).is_err()
        {
            bail!("developer_session_invalid");
        }
        cookies::validate(&source.cookies)?;
        let generation = digest(&bytes);
        let mut effective = source.cookies.clone();
        if let Some(bytes) = optional_read(&root.join("overlay.json"), 1 << 20)? {
            let overlay: Overlay = serde_json::from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("developer_overlay_invalid"))?;
            if overlay.schema != 1 {
                bail!("developer_overlay_invalid");
            }
            if overlay.generation == generation {
                cookies::validate_jar(&overlay.cookies)?;
                effective = overlay.cookies;
            }
        }
        cookies::header(
            &effective,
            &url::Url::parse(
                "https://developer.apple.com/services-account/QH65B2/account/listTeams.action",
            )?,
        )?;
        let secrets = source
            .cookies
            .iter()
            .chain(&effective)
            .map(|cookie| cookie.value.clone())
            .collect();
        Ok(Self {
            _lock: lock,
            root: root.into(),
            source_path: source_path.into(),
            source,
            generation,
            cookies: effective,
            csrf: None,
            secrets,
        })
    }

    pub fn remember_secrets(&mut self, values: impl IntoIterator<Item = String>) -> Result<()> {
        self.secrets
            .extend(values.into_iter().filter(|value| !value.is_empty()));
        if self.secrets.len() > 4096
            || self.secrets.iter().map(String::len).sum::<usize>() > 8 << 20
        {
            bail!("developer_secret_history_limit");
        }
        Ok(())
    }

    pub fn reject_secret_echo(&self, value: &serde_json::Value) -> Result<()> {
        let contains = |text: &str| {
            self.secrets
                .iter()
                .any(|secret| text == secret || secret.len() >= 8 && text.contains(secret))
        };
        match value {
            serde_json::Value::String(text) if contains(text) => {
                bail!("developer_response_contains_secret")
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    self.reject_secret_echo(value)?;
                }
            }
            serde_json::Value::Object(values) => {
                for (key, value) in values {
                    if contains(key) {
                        bail!("developer_response_contains_secret");
                    }
                    self.reject_secret_echo(value)?;
                }
            }
            _ => (),
        }
        Ok(())
    }

    pub fn ensure_generation(&self) -> Result<()> {
        if digest(&fs_safe::private_read(&self.source_path, 1 << 20)?) != self.generation {
            bail!("developer_session_changed");
        }
        Ok(())
    }

    pub fn persist_overlay(&self) -> Result<()> {
        self.ensure_generation()?;
        let overlay = Overlay {
            schema: 1,
            generation: self.generation.clone(),
            cookies: self.cookies.clone(),
        };
        let bytes =
            serde_json::to_vec(&overlay).map_err(|_| anyhow::anyhow!("developer_state_invalid"))?;
        if bytes.len() > 1 << 20 {
            bail!("developer_state_too_large");
        }
        fs_safe::atomic_write(&self.root.join("overlay.json"), &bytes)
    }
}
