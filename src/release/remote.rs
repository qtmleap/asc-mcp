use super::state::{Outcome, Target};
use crate::{api_tools::resource_segment, client::ApiClient};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

const PENDING: &str = "PENDING_DEVELOPER_RELEASE";

pub async fn fetch(client: &ApiClient, version_id: &str) -> Result<Value> {
    let path = format!("/v1/appStoreVersions/{}", resource_segment(version_id)?);
    client
        .request(
            "GET",
            &path,
            vec![("include".to_string(), "app,build".to_string())],
            None,
        )
        .await
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn relates(data: &Value, name: &str, kind: &str, id: &str) -> bool {
    let linkage = &data["relationships"][name]["data"];
    text(linkage, "type") == Some(kind) && text(linkage, "id") == Some(id)
}

fn state(attributes: &Value) -> Result<String> {
    let mut found = Vec::new();
    for key in ["appVersionState", "appStoreState"] {
        match attributes.get(key) {
            None => (),
            Some(Value::String(value)) => found.push(value.as_str()),
            Some(_) => bail!("release_state_invalid"),
        }
    }
    match found.as_slice() {
        [] => bail!("release_state_missing"),
        [one] => Ok((*one).to_string()),
        [first, second] if first == second => Ok((*first).to_string()),
        _ => bail!("release_state_conflict"),
    }
}

pub fn validate(body: &Value, target: &Target) -> Result<String> {
    let data = &body["data"];
    let attributes = &data["attributes"];
    if text(data, "type") != Some("appStoreVersions")
        || text(data, "id") != Some(&target.version_id)
        || text(attributes, "platform") != Some("IOS")
        || text(attributes, "releaseType") != Some("MANUAL")
        || text(attributes, "versionString") != Some(&target.expected_version_string)
    {
        bail!("release_target_mismatch");
    }
    if !relates(data, "app", "apps", &target.app_id)
        || !relates(data, "build", "builds", &target.expected_build_id)
    {
        bail!("release_relationship_mismatch");
    }
    let current = state(attributes)?;
    if current != PENDING {
        return Err(anyhow!("release_state_not_pending"));
    }
    Ok(current)
}

pub fn observed_state(body: &Value) -> Option<String> {
    let attributes = &body["data"]["attributes"];
    text(attributes, "appVersionState")
        .or_else(|| text(attributes, "appStoreState"))
        .map(str::to_string)
}

fn request_id(value: &Value) -> Option<String> {
    let data = &value["data"];
    let id = text(data, "id")?;
    let valid = !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    (text(data, "type") == Some("appStoreVersionReleaseRequests") && valid).then(|| id.to_string())
}

fn unknown(reason: &str) -> Outcome {
    Outcome {
        status: "outcome_unknown".to_string(),
        request_id: None,
        reason: Some(reason.to_string()),
    }
}

pub async fn send(client: &ApiClient, version_id: &str) -> Outcome {
    let body = json!({"data":{"type":"appStoreVersionReleaseRequests","relationships":{"appStoreVersion":{"data":{"type":"appStoreVersions","id":version_id}}}}});
    let response = client
        .raw_request(
            "POST",
            "/v1/appStoreVersionReleaseRequests",
            Vec::new(),
            Some(body),
        )
        .await;
    let Ok(response) = response else {
        return unknown("request_failed_or_rejected");
    };
    if response.status != 201 {
        return unknown("unexpected_status");
    }
    match serde_json::from_slice::<Value>(&response.body)
        .ok()
        .as_ref()
        .and_then(request_id)
    {
        Some(id) => Outcome {
            status: "release_request_created".to_string(),
            request_id: Some(id),
            reason: None,
        },
        None => unknown("invalid_response"),
    }
}
