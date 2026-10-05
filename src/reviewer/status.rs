//! Session-only, read-only App Review status tools.
//!
//! Everything here is HTTP GET through `Reviewer::pages`/`request` (same-origin,
//! exact-path, max 20 pages, 8 MiB). Nothing reads or writes monitor state, takes
//! the monitor lock, logs in, switches teams or sends a POST.
use super::{Reviewer, identifier};
use anyhow::{Context, Result, bail};
use chrono::DateTime;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::Instant;

pub(super) const NAMES: &[&str] = &[
    "list_review_apps",
    "get_app_review_status",
    "get_all_app_review_statuses",
    "get_app_rejection_details",
    "get_review_connection_status",
];

/// Deadlines stay below the 180 s MCP tool timeout so partial batches can return.
#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub item: Duration,
    pub batch: Duration,
    pub output: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            item: Duration::from_secs(55),
            batch: Duration::from_secs(150),
            output: super::MAX_BODY,
        }
    }
}

pub(super) fn tools() -> Vec<Value> {
    let app_id = json!({"type":"string","pattern":"^[0-9]+$","description":"Numeric App Store Connect app ID"});
    let defs = [
        (
            "list_review_apps",
            "List apps visible to the Apple ID session through the undocumented Iris API. Follows all bounded pages.",
            false,
        ),
        (
            "get_app_review_status",
            "Read an app's App Store versions and review submissions with Apple's raw states. A COMPLETE submission is not approval.",
            true,
        ),
        (
            "get_all_app_review_statuses",
            "Read review status for every visible app. Partial results report complete=false, errors and not_checked; output is bounded and marked truncated.",
            false,
        ),
        (
            "get_app_rejection_details",
            "Read the current rejected versions' latest linked submission Apple messages and rejection details. Older history is labelled separately; unknown is never reported as no issues. Message HTML is untrusted.",
            true,
        ),
        (
            "get_review_connection_status",
            "Check the current Apple ID web session (Olympus) without exposing identity. Reports only the current provider; does not log in or switch teams.",
            false,
        ),
    ];
    defs.into_iter()
        .map(|(name, description, with_app)| {
            let (props, required) = if with_app {
                (json!({"app_id":app_id}), json!(["app_id"]))
            } else {
                (json!({}), json!([]))
            };
            json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":props,"required":required,"additionalProperties":false},"annotations":{"readOnlyHint":true,"openWorldHint":true}})
        })
        .collect()
}

fn digits(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())
}
fn text<'a>(v: &'a Value, pointer: &str) -> &'a Value {
    v.pointer(pointer).unwrap_or(&Value::Null)
}
fn project_version(v: &Value) -> Result<Value> {
    let id = v["id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("reviewer_api_error_response")?;
    Ok(
        json!({"id":id,"platform":text(v,"/attributes/platform"),"version_string":text(v,"/attributes/versionString"),"app_store_state":text(v,"/attributes/appStoreState"),"app_version_state":text(v,"/attributes/appVersionState"),"created_date":text(v,"/attributes/createdDate")}),
    )
}
fn project_submission(s: &Value) -> Result<Value> {
    let id = s["id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("reviewer_api_error_response")?;
    Ok(
        json!({"id":id,"platform":text(s,"/attributes/platform"),"state":text(s,"/attributes/state"),"submitted_date":text(s,"/attributes/submittedDate"),"version_id":text(s,"/relationships/appStoreVersionForReview/data/id")}),
    )
}
/// Apple's reviewRejections reasons, whitelisted to the three documented fields.
fn rejection_attributes(attributes: &Value) -> Value {
    let reasons: Option<Vec<Value>> = attributes["reasons"]
        .as_array()
        .map(|r| {
            r.iter()
                .map(|r| {
                    json!({"reasonCode":r["reasonCode"],"reasonDescription":r["reasonDescription"],"reasonSection":r["reasonSection"]})
                })
                .collect()
        });
    json!({"reasons":reasons})
}
/// Current state string; appVersionState wins, appStoreState only when it is absent.
fn current_state(version: &Value) -> Option<&str> {
    match &version["app_version_state"] {
        Value::Null => version["app_store_state"].as_str(),
        v => v.as_str(),
    }
}
// Known non-rejection states from Apple's AppVersionState/AppStoreVersionState.
// New values remain unclassified until their meaning has been verified.
fn known_non_rejected_state(state: &str) -> bool {
    matches!(
        state,
        "ACCEPTED"
            | "DEVELOPER_REMOVED_FROM_SALE"
            | "DEVELOPER_REJECTED"
            | "IN_REVIEW"
            | "INVALID_BINARY"
            | "PENDING_APPLE_RELEASE"
            | "PENDING_CONTRACT"
            | "PENDING_DEVELOPER_RELEASE"
            | "PREPARE_FOR_SUBMISSION"
            | "PREORDER_READY_FOR_SALE"
            | "PROCESSING_FOR_APP_STORE"
            | "PROCESSING_FOR_DISTRIBUTION"
            | "READY_FOR_DISTRIBUTION"
            | "READY_FOR_REVIEW"
            | "READY_FOR_SALE"
            | "REMOVED_FROM_SALE"
            | "REPLACED_WITH_NEW_VERSION"
            | "WAITING_FOR_EXPORT_COMPLIANCE"
            | "WAITING_FOR_REVIEW"
            | "NOT_APPLICABLE"
    )
}
/// `Ok(Some(id))` when a newer same-platform version exists, `Ok(None)` when none does,
/// `Err` when chronology cannot be established reliably.
fn superseded_by(version: &Value, all: &[Value]) -> Result<Option<String>, String> {
    let vid = version["id"].as_str().unwrap_or_default();
    let Some(platform) = version["platform"].as_str() else {
        return Err(format!("unknown_chronology: version {vid} has no platform"));
    };
    let date = |v: &Value| {
        v["created_date"]
            .as_str()
            .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
    };
    let mine = date(version);
    let mut newest: Option<(DateTime<chrono::FixedOffset>, &str)> = None;
    for other in all.iter().filter(|o| o["id"] != version["id"]) {
        let oid = other["id"].as_str().unwrap_or_default();
        match other["platform"].as_str() {
            Some(p) if p != platform => continue,
            Some(_) => {}
            None => return Err(format!("unknown_chronology: version {oid} has no platform")),
        }
        let (Some(mine), Some(theirs)) = (mine, date(other)) else {
            return Err(format!(
                "unknown_chronology: versions {vid} and {oid} lack a valid createdDate"
            ));
        };
        if mine == theirs {
            return Err(format!(
                "unknown_chronology: versions {vid} and {oid} share a createdDate"
            ));
        }
        if theirs > mine && newest.is_none_or(|(d, _)| theirs > d) {
            newest = Some((theirs, oid));
        }
    }
    Ok(newest.map(|(_, id)| id.to_owned()))
}
fn classify(error: &str) -> &'static str {
    let code = error.split(':').next().unwrap_or(error).trim();
    if matches!(
        code,
        "reviewer_session_required"
            | "reviewer_session_invalid"
            | "reviewer_session_permissions"
            | "reviewer_private_file_required"
            | "reviewer_private_file_too_large"
            | "reviewer_private_file_unreadable"
            | "reviewer_private_file_changed"
    ) {
        "missing"
    } else if error.contains("reviewer_auth_required") {
        "rejected"
    } else if error.contains("reviewer_forbidden") {
        "forbidden"
    } else if error.contains("reviewer_unexpected_redirect") {
        "redirected"
    } else {
        "unreachable"
    }
}
fn authentication_error(error: &anyhow::Error) -> bool {
    matches!(classify(&error.to_string()), "missing" | "rejected")
}
fn find_included<'a>(response: &'a Value, reference: &Value) -> Option<&'a Value> {
    let id = reference.get("id")?;
    response["included"].as_array()?.iter().find(|i| {
        &i["id"] == id && (reference.get("type").is_none() || i["type"] == reference["type"])
    })
}

impl Reviewer {
    pub(super) async fn status_call(
        &self,
        name: &str,
        args: &Value,
        api_configured: Option<bool>,
    ) -> Result<Value> {
        let app = || -> Result<&str> {
            let id = super::arg(args, "app_id")?;
            if !digits(id) {
                bail!("invalid app_id: digits only");
            }
            Ok(id)
        };
        match name {
            "list_review_apps" => {
                let apps = self.within(self.limits.item, self.list_apps()).await?;
                Ok(json!({"apps":apps,"complete":true}))
            }
            "get_app_review_status" => {
                let id = app()?;
                self.within(self.limits.item, self.app_status(id)).await
            }
            "get_all_app_review_statuses" => self.all_statuses().await,
            "get_app_rejection_details" => {
                let id = app()?;
                self.within(self.limits.item, self.rejection_details(id))
                    .await
            }
            "get_review_connection_status" => Ok(self.connection_status(api_configured).await),
            _ => bail!("unknown reviewer tool"),
        }
    }

    async fn within<T>(&self, limit: Duration, work: impl Future<Output = Result<T>>) -> Result<T> {
        tokio::time::timeout(limit, work)
            .await
            .map_err(|_| anyhow::anyhow!("reviewer_item_timeout: Apple did not respond in time"))?
    }

    async fn connection_status(&self, api_configured: Option<bool>) -> Value {
        let session = match self.url("/olympus/v1/session", &[]) {
            Ok(url) => {
                self.within(
                    self.limits.item,
                    self.request(reqwest::Method::GET, url, None, None),
                )
                .await
            }
            Err(e) => Err(e),
        };
        let (authentication, provider) = match session {
            // Same shape check as apple_login: a user or provider object, else not a session.
            Ok((body, _)) if body["user"].is_object() || body["provider"].is_object() => {
                let provider = &body["provider"];
                let known = provider["providerId"].is_u64() || provider["providerId"].is_i64();
                let provider = if known && provider["name"].is_string() {
                    json!({"providerId":provider["providerId"],"name":provider["name"]})
                } else {
                    json!("unknown")
                };
                ("ok", provider)
            }
            Ok(_) => ("unknown", json!("unknown")),
            Err(e) => (classify(&e.to_string()), json!("unknown")),
        };
        json!({
            "authentication":authentication,
            "provider":provider,
            "scope":"current_provider_session",
            "capability":{"app_review_reads":"A valid session proves Apple accepted the cookies for the current provider only; access to each app is verified when that app is read.","other_providers":"not checked; no team switching is performed"},
            "public_api_configured":api_configured,
        })
    }

    async fn list_apps(&self) -> Result<Vec<Value>> {
        let apps = self.pages("/iris/v1/apps", &[("limit", "200")]).await?;
        apps["data"]
            .as_array()
            .context("reviewer_api_error_response")?
            .iter()
            .map(|a| {
                let id = a["id"].as_str().filter(|s| !s.is_empty()).context("reviewer_api_error_response")?;
                Ok(json!({"id":id,"name":text(a,"/attributes/name"),"bundle_id":text(a,"/attributes/bundleId"),"sku":text(a,"/attributes/sku")}))
            })
            .collect()
    }

    /// Both resources are required; either failure fails the whole read.
    async fn fetch_app(&self, id: &str) -> Result<(Vec<Value>, Vec<Value>)> {
        let versions_path = format!("/iris/v1/apps/{id}/appStoreVersions");
        let submissions_path = format!("/iris/v1/apps/{id}/reviewSubmissions");
        let versions_query = [("limit", "200")];
        let submissions_query = [("limit", "200"), ("include", "appStoreVersionForReview")];
        let versions_read = self.pages(&versions_path, &versions_query);
        let submissions_read = self.pages(&submissions_path, &submissions_query);
        tokio::pin!(versions_read, submissions_read);
        // Cancel a slow sibling on auth failure. For other failures, read the
        // sibling so its authentication error cannot be hidden by the first.
        let (versions, submissions) = tokio::select! {
            versions = &mut versions_read => {
                if versions.as_ref().err().is_some_and(authentication_error) {
                    return Err(versions.unwrap_err());
                }
                (versions, submissions_read.await)
            }
            submissions = &mut submissions_read => {
                if submissions.as_ref().err().is_some_and(authentication_error) {
                    return Err(submissions.unwrap_err());
                }
                (versions_read.await, submissions)
            }
        };
        if versions.as_ref().err().is_some_and(authentication_error) {
            return Err(versions.unwrap_err());
        }
        if submissions.as_ref().err().is_some_and(authentication_error) {
            return Err(submissions.unwrap_err());
        }
        let (versions, submissions) = (versions?, submissions?);
        Ok((
            versions["data"]
                .as_array()
                .context("reviewer_api_error_response")?
                .iter()
                .map(project_version)
                .collect::<Result<_>>()?,
            submissions["data"]
                .as_array()
                .context("reviewer_api_error_response")?
                .iter()
                .map(project_submission)
                .collect::<Result<_>>()?,
        ))
    }

    async fn app_status(&self, id: &str) -> Result<Value> {
        let (versions, submissions) = self.fetch_app(id).await?;
        Ok(
            json!({"app_id":id,"versions":versions,"review_submissions":submissions,"complete":true,
            "notes":["States are Apple's raw values; unknown values are preserved.","A COMPLETE review submission is not approval; read the version state."]}),
        )
    }

    async fn all_statuses(&self) -> Result<Value> {
        let started = Instant::now();
        let apps = self.within(self.limits.item, self.list_apps()).await?;
        let (mut results, mut errors, mut not_checked) = (Vec::new(), Vec::new(), Vec::new());
        let mut stopped = false;
        for app in &apps {
            let id = app["id"].as_str().unwrap_or_default();
            let remaining = self.limits.batch.saturating_sub(started.elapsed());
            if stopped || remaining.is_zero() {
                not_checked.push(json!(id));
                continue;
            }
            let outcome = if digits(id) {
                let limit = remaining.min(self.limits.item);
                match tokio::time::timeout(limit, self.app_status(id)).await {
                    Ok(r) => r,
                    Err(_) if remaining < self.limits.item => Err(anyhow::anyhow!(
                        "reviewer_batch_deadline_timeout: batch time budget exhausted"
                    )),
                    Err(_) => Err(anyhow::anyhow!(
                        "reviewer_item_timeout: Apple did not respond in time"
                    )),
                }
            } else {
                Err(anyhow::anyhow!(
                    "invalid_app_id: Apple returned a non-numeric app ID"
                ))
            };
            match outcome {
                Ok(status) => results.push(json!({"app_id":id,"name":app["name"],"bundle_id":app["bundle_id"],"status":status})),
                Err(e) => {
                    let message = e.to_string();
                    stopped = authentication_error(&e);
                    errors.push(json!({"app_id":id,"error":if message.is_empty() {"unknown_error".to_owned()} else {message}}));
                }
            }
        }
        let mut out = json!({"apps":results,"errors":errors,"not_checked":not_checked,"complete":true,"truncated":false,"omitted_count":0});
        let mut omitted = 0;
        while serde_json::to_vec(&out)?.len() > self.limits.output {
            let apps = out["apps"].as_array_mut().unwrap();
            if apps.pop().is_none() {
                // Even the failures don't fit: keep the flag, drop the detail.
                out["not_checked"] = json!([]);
                out["errors"] = json!([]);
                out["error_detail_omitted"] = json!(true);
                if serde_json::to_vec(&out)?.len() > self.limits.output {
                    bail!("reviewer_response_invalid: output limit too small");
                }
                break;
            }
            omitted += 1;
            out["truncated"] = json!(true);
            out["omitted_count"] = json!(omitted);
        }
        let partial = out["truncated"] == true
            || out["error_detail_omitted"] == true
            || !out["errors"].as_array().unwrap().is_empty()
            || !out["not_checked"].as_array().unwrap().is_empty();
        out["complete"] = json!(!partial);
        Ok(out)
    }

    async fn submission_messages(&self, submission: &str) -> Result<(Vec<Value>, Vec<String>)> {
        let threads = self
            .pages(
                "/iris/v1/resolutionCenterThreads",
                &[("filter[reviewSubmission]", submission), ("limit", "200")],
            )
            .await?;
        let (mut out, mut reasons) = (Vec::new(), Vec::new());
        for thread in threads["data"]
            .as_array()
            .context("reviewer_api_error_response")?
        {
            let tid = thread["id"]
                .as_str()
                .filter(|s| identifier(s))
                .context("reviewer_api_error_response")?;
            let response = self
                .pages(
                    &format!("/iris/v1/resolutionCenterThreads/{tid}/resolutionCenterMessages"),
                    &[
                        (
                            "include",
                            "fromActor,rejections,resolutionCenterMessageAttachments",
                        ),
                        ("limit", "200"),
                    ],
                )
                .await?;
            for m in response["data"]
                .as_array()
                .context("reviewer_api_error_response")?
            {
                let mid = m["id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .context("reviewer_api_error_response")?;
                let actor = m
                    .pointer("/relationships/fromActor/data")
                    .and_then(|r| find_included(&response, r))
                    .map(|a| &a["attributes"]);
                let kind = actor.and_then(|a| a["actorType"].as_str());
                let user = actor.is_some_and(|a| {
                    ["userEmail", "userFirstName", "apiKeyId"]
                        .iter()
                        .any(|k| a[*k].is_string())
                });
                let (actor_type, name) = match (kind, actor) {
                    (Some(k @ ("APPLE" | "DEVELOPER")), Some(a)) => (k, a["name"].clone()),
                    (_, Some(a)) if user => ("USER", a["name"].clone()),
                    _ => ("UNKNOWN", Value::Null),
                };
                if actor_type == "UNKNOWN" {
                    reasons.push(format!("unknown_actor: message {mid} sender cannot be identified; treated as not Apple"));
                }
                if actor_type == "APPLE" {
                    if !text(m, "/attributes/messageBody").is_string() {
                        reasons.push(format!(
                            "missing_message_body: Apple message {mid} has no readable body"
                        ));
                    }
                    if let Some(references) = m.pointer("/relationships/rejections/data") {
                        match references {
                            Value::Null => {},
                            Value::Array(references) => {
                                for reference in references {
                                    let included = find_included(&response, reference);
                                    let valid = included
                                        .and_then(|item| item.pointer("/attributes/reasons"))
                                        .and_then(Value::as_array)
                                        .is_some_and(|items| items.iter().all(|reason| {
                                            ["reasonCode", "reasonDescription", "reasonSection"]
                                                .iter().all(|key| reason[*key].is_string())
                                        }));
                                    if !valid {
                                        reasons.push(format!(
                                            "missing_rejection_evidence: Apple message {mid} has an unresolved or malformed linked rejection"
                                        ));
                                    }
                                }
                            },
                            _ => reasons.push(format!(
                                "missing_rejection_evidence: Apple message {mid} has malformed rejection references"
                            )),
                        }
                    }
                }
                let related = |key: &str| -> Vec<Value> {
                    m.pointer(&format!("/relationships/{key}/data"))
                        .and_then(Value::as_array)
                        .map(|refs| refs.iter().map(|r| (r, find_included(&response, r))).map(|(r, inc)| (r.clone(), inc.cloned())).map(|(r, inc)| match key {
                            "rejections" => json!({"id":r["id"],"attributes":inc.map(|i| rejection_attributes(&i["attributes"])).unwrap_or(Value::Null)}),
                            _ => json!({"id":r["id"],"file_name":inc.as_ref().map(|i| i["attributes"]["fileName"].clone()),"file_size":inc.as_ref().map(|i| i["attributes"]["fileSize"].clone())}),
                        }).collect())
                        .unwrap_or_default()
                };
                out.push(json!({"id":mid,"thread_id":tid,"submission_id":submission,"created_date":text(m,"/attributes/createdDate"),"body_html":text(m,"/attributes/messageBody"),"actor_type":actor_type,"actor_name":name,"is_apple":actor_type == "APPLE","rejections":related("rejections"),"attachments":related("resolutionCenterMessageAttachments")}));
            }
        }
        Ok((out, reasons))
    }

    async fn rejection_details(&self, id: &str) -> Result<Value> {
        let (versions, submissions) = self.fetch_app(id).await?;
        let (mut rejected_out, mut reasons) = (Vec::new(), Vec::<String>::new());
        let (mut historical, mut unclassified) = (Vec::new(), Vec::new());
        for version in &versions {
            let vid = version["id"].as_str().unwrap_or_default();
            let state = match current_state(version) {
                Some(s @ ("REJECTED" | "METADATA_REJECTED")) => s,
                Some(state) if known_non_rejected_state(state) => continue,
                _ => {
                    reasons.push(format!(
                        "unknown_current_state: version {vid} has a missing or unrecognized state"
                    ));
                    unclassified.push(version.clone());
                    continue;
                }
            };
            let chronology = superseded_by(version, &versions);
            if let Ok(Some(newer)) = &chronology {
                historical.push(
                    json!({"version":version,"rejection_state":state,"superseded_by":newer,
                    "note":"Historical rejection; a newer version exists on the same platform"}),
                );
                continue;
            }
            if let Err(reason) = chronology {
                reasons.push(reason);
            }
            let linked: Vec<&Value> = submissions
                .iter()
                .filter(|s| s["version_id"] == vid)
                .collect();
            let dated: Vec<_> = linked
                .iter()
                .map(|s| {
                    (
                        *s,
                        s["submitted_date"]
                            .as_str()
                            .and_then(|d| DateTime::parse_from_rfc3339(d).ok()),
                    )
                })
                .collect();
            let mut entry = json!({"version":version,"rejection_state":state,"latest_submission":null,"current_messages":[],
                "older_message_history":{"label":"Historical messages from older linked submissions for this version; not the current rejection","submissions":[]}});
            let latest = if linked.is_empty() {
                Err("unknown_no_linked_submission")
            } else if dated.iter().any(|(_, d)| d.is_none()) {
                Err("unknown_unparseable_submitted_date")
            } else {
                let max = dated.iter().filter_map(|(_, d)| *d).max().unwrap();
                let top: Vec<_> = dated.iter().filter(|(_, d)| *d == Some(max)).collect();
                if top.len() == 1 {
                    Ok(top[0].0)
                } else {
                    Err("unknown_ambiguous_latest_submission")
                }
            };
            match latest {
                Err(status) => {
                    entry["latest_status"] = json!(status);
                    reasons.push(format!(
                        "{status}: version {vid}; current rejection details could not be determined"
                    ));
                }
                Ok(latest) => {
                    entry["latest_status"] = json!("latest_submission_identified");
                    entry["latest_submission"] = latest.clone();
                    let (messages, unknown) = self
                        .submission_messages(latest["id"].as_str().unwrap_or_default())
                        .await?;
                    if messages.is_empty() {
                        reasons.push(format!("no_messages_found: version {vid} latest submission has no readable messages"));
                    } else if !messages.iter().any(|message| message["is_apple"] == true) {
                        reasons.push(format!(
                            "no_apple_messages_found: version {vid} latest submission has no identifiable Apple message"
                        ));
                    }
                    reasons.extend(unknown);
                    entry["current_messages"] = json!(messages);
                    for older in linked.iter().filter(|s| s["id"] != latest["id"]) {
                        let (messages, unknown) = self
                            .submission_messages(older["id"].as_str().unwrap_or_default())
                            .await?;
                        reasons.extend(unknown);
                        entry["older_message_history"]["submissions"].as_array_mut().unwrap().push(json!({"submission_id":older["id"],"state":older["state"],"submitted_date":older["submitted_date"],"messages":messages}));
                    }
                }
            }
            rejected_out.push(entry);
        }
        let out = json!({"app_id":id,
            "status":if !rejected_out.is_empty() {"rejected_versions_found"} else if !unclassified.is_empty() {"unknown_current_state"} else {"no_current_rejection"},
            "rejected_versions":rejected_out,"historical_rejected_versions":historical,"unclassified_versions":unclassified,"complete":reasons.is_empty(),"incomplete_reasons":reasons,
            "notes":["Only versions whose current state is REJECTED or METADATA_REJECTED are examined.","Only submissions linked to the version are used; no dates are matched across versions."]});
        if serde_json::to_vec(&out)?.len() > self.limits.output {
            bail!("reviewer_response_invalid: output too large");
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests;
