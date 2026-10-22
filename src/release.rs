use crate::client::ApiClient;
use anyhow::{Result, anyhow, bail};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use state::{Outcome, Receipt, Store, Target};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

mod remote;
mod state;

pub const NAME: &str = "release_app_store_version";
const TOKEN_TTL_SECONDS: u64 = 900;
const WARNING: &str = "Releasing cannot be cancelled once requested. This tool does not attest that a human approved the release.";

pub fn tool() -> Value {
    json!({
        "name": NAME,
        "title": "Release manually held App Store version",
        "description": "Two-step release of one iOS App Store version in PENDING_DEVELOPER_RELEASE with manual release. Call with dry_run=true (default) to validate the exact target and obtain a single-use confirmation_token valid for 15 minutes, then call with dry_run=false and that token. At most one release request is ever sent per version; any unknown outcome blocks automatic retry. A release cannot be cancelled and human approval is not attested by this tool.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "app_id": {"type": "string", "minLength": 1, "maxLength": 128},
                "version_id": {"type": "string", "minLength": 1, "maxLength": 128},
                "expected_version_string": {"type": "string", "minLength": 1, "maxLength": 64},
                "expected_build_id": {"type": "string", "minLength": 1, "maxLength": 128},
                "dry_run": {"type": "boolean", "default": true},
                "confirmation_token": {"type": "string", "minLength": 64, "maxLength": 64}
            },
            "required": ["app_id", "version_id", "expected_version_string", "expected_build_id"],
            "additionalProperties": false
        },
        "annotations": {"readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": true}
    })
}

pub fn state_root() -> Result<PathBuf> {
    if let Some(root) = std::env::var_os("ASC_RELEASE_STATE_ROOT").filter(|value| !value.is_empty())
    {
        let root = PathBuf::from(root);
        if !root.is_absolute() {
            bail!("release_state_root_must_be_absolute");
        }
        return Ok(root);
    }
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("release_state_root_unavailable"))?;
    Ok(PathBuf::from(home).join("Library/Application Support/ASC MCP/Manual Releases"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    app_id: String,
    version_id: String,
    expected_version_string: String,
    expected_build_id: String,
    dry_run: Option<bool>,
    confirmation_token: Option<String>,
}

struct Args {
    target: Target,
    token: Option<String>,
}

fn identifier(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("release_invalid_identifier");
    }
    crate::api_tools::resource_segment(value)?;
    Ok(())
}

impl Args {
    fn parse(value: &Value) -> Result<Self> {
        let object = value
            .as_object()
            .ok_or_else(|| anyhow!("arguments must be an object"))?;
        if object.values().any(Value::is_null) {
            bail!("release_null_argument");
        }
        let raw: Raw = serde_json::from_value(value.clone())
            .map_err(|_| anyhow!("release_invalid_arguments"))?;
        identifier(&raw.app_id)?;
        identifier(&raw.version_id)?;
        identifier(&raw.expected_build_id)?;
        let version = &raw.expected_version_string;
        if version.is_empty() || version.len() > 64 || version.chars().any(char::is_control) {
            bail!("release_invalid_version_string");
        }
        let token = match (raw.dry_run.unwrap_or(true), raw.confirmation_token) {
            (true, None) => None,
            (false, Some(token))
                if token.len() == 64
                    && token
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) =>
            {
                Some(token)
            }
            _ => bail!("release_dry_run_and_token_mismatch"),
        };
        Ok(Self {
            target: Target {
                app_id: raw.app_id,
                version_id: raw.version_id,
                expected_version_string: raw.expected_version_string,
                expected_build_id: raw.expected_build_id,
            },
            token,
        })
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn recorded(receipt: &Receipt, observed: Option<Option<String>>) -> Result<Value> {
    let last = receipt.outcome.as_ref().map_or_else(
        || json!({"status": "outcome_unknown"}),
        |outcome| json!(outcome),
    );
    let mut value = json!({
        "status": "release_attempt_recorded",
        "target": serde_json::to_value(&receipt.target)?,
        "last_outcome": last,
        "automatic_retry": false,
        "message": "A release request was already attempted for this version. No further release request will be sent by this tool; inspect App Store Connect for the real state.",
    });
    if let Some(state) = observed {
        value["observed_state"] = json!(state);
    }
    Ok(value)
}

fn unknown(reason: &str) -> Value {
    json!({
        "status": "outcome_unknown",
        "reason": reason,
        "automatic_retry": false,
        "message": "The release request outcome is unknown. Do not retry automatically; inspect App Store Connect before any manual action.",
    })
}

async fn preview(client: &ApiClient, store: &Store, args: Args) -> Result<Value> {
    if let Some(receipt) = store.load()?.filter(|receipt| receipt.attempted) {
        let body = remote::fetch(client, &args.target.version_id).await?;
        return recorded(&receipt, Some(remote::observed_state(&body)));
    }
    let body = remote::fetch(client, &args.target.version_id).await?;
    let observed = remote::validate(&body, &args.target)?;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    let issued_at = now();
    let receipt = Receipt {
        schema: 1,
        target: args.target.clone(),
        token_sha256: token_hash(&token),
        issued_at,
        expires_at: issued_at + TOKEN_TTL_SECONDS,
        attempted: false,
        outcome: None,
    };
    store.save(&receipt)?;
    Ok(json!({
        "status": "prepared",
        "target": serde_json::to_value(&args.target)?,
        "observed_state": observed,
        "confirmation_token": token,
        "expires_at": receipt.expires_at,
        "release_cannot_be_cancelled": true,
        "human_approval_attested": false,
        "warning": WARNING,
    }))
}

fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

async fn confirm(client: &ApiClient, store: &Store, args: Args, token: &str) -> Result<Value> {
    let mut receipt = store
        .load()?
        .ok_or_else(|| anyhow!("release_not_prepared"))?;
    if receipt.attempted {
        return recorded(&receipt, None);
    }
    if receipt.target != args.target {
        bail!("release_target_mismatch");
    }
    if receipt.token_sha256 != token_hash(token) {
        bail!("release_token_invalid");
    }
    if now() >= receipt.expires_at {
        bail!("release_token_expired");
    }
    let body = remote::fetch(client, &args.target.version_id).await?;
    remote::validate(&body, &args.target)?;
    receipt.attempted = true;
    store.save(&receipt)?;
    let outcome = remote::send(client, &args.target.version_id).await;
    receipt.outcome = Some(outcome.clone());
    let persisted = store.save(&receipt).is_ok();
    match outcome {
        Outcome {
            status,
            request_id: Some(request_id),
            ..
        } if persisted && status == "release_request_created" => Ok(json!({
            "status": "release_request_created",
            "request_id": request_id,
            "target": serde_json::to_value(&args.target)?,
            "message": "Apple accepted the release request (HTTP 201). Acceptance is not proof of availability; check App Store Connect.",
        })),
        Outcome { reason, .. } if persisted => Ok(unknown(reason.as_deref().unwrap_or("unknown"))),
        _ => Ok(unknown("outcome_not_persisted")),
    }
}

pub async fn call(client: &ApiClient, root: &Path, args: &Value) -> Result<Value> {
    let args = Args::parse(args)?;
    let store = Store::open(root, &args.target.version_id)?;
    match args.token.clone() {
        None => preview(client, &store, args).await,
        Some(token) => confirm(client, &store, args, &token).await,
    }
}
