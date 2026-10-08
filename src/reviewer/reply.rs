//! Modern two-step (draft, then send) App Review reply through the Iris JSON:API.
//!
//! Safety model: every thread has a private advisory lock and a private durable record. The
//! record is written before each POST, so a crash or ambiguous result leaves an unfinished
//! record that blocks later sends on that thread until a person inspects it. POSTs are never
//! replayed. Nothing is ever deleted or patched automatically.
//!
//! Every operation is bound to one authentication source generation (`Pinned`): the ownership
//! proof, preflights and POSTs use that generation only, a replaced source fails closed before
//! any further request, and these requests are never recovered or replayed. Response bodies
//! stay unredacted in memory so integrity hashes and creator checks compare what Apple
//! returned; only fixed codes and identifiers that are not credentials leave this module.
use super::*;

const RECORD_MAX: usize = 64 * 1024;
const BODY_UNITS: usize = 4000;
const ID_MAX: usize = 128;

/// Persisted per-thread state. Holds a body hash only, never the body or any credential.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Record {
    app_id: String,
    submission_id: String,
    thread_id: String,
    expected_apple_message_id: String,
    body_hash: String,
    /// Whole seconds since the epoch, taken before the first POST.
    intent_at: i64,
    prior_message_ids: Vec<String>,
    draft_id: Option<String>,
    response_message_id: Option<String>,
    /// intent, draft_created, send_attempted, accepted, delivered
    state: String,
    #[serde(default)]
    http_status: Option<u16>,
    #[serde(default)]
    error_kind: Option<String>,
}

pub(super) fn record_path(root: &Path, thread_id: &str) -> PathBuf {
    root.join(format!("reply-{thread_id}.json"))
}

struct ThreadLock(fs::File);
impl ThreadLock {
    fn acquire(root: &Path, thread_id: &str) -> Result<Self> {
        let file = open_private_lock(root, &format!(".reply-{thread_id}.lock"))?;
        fs2::FileExt::try_lock_exclusive(&file).map_err(|_| {
            anyhow::anyhow!(
                "reviewer_reply_busy: another reply or status check is active on this thread"
            )
        })?;
        Ok(Self(file))
    }
}
impl Drop for ThreadLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Actor {
    Apple,
    Developer,
}

struct Msg {
    id: String,
    created: DateTime<Utc>,
    actor: Actor,
    hash: String,
}

struct Ctx {
    msgs: Vec<Msg>,
    latest_apple: Option<String>,
}
impl Ctx {
    fn ids(&self) -> Vec<String> {
        self.msgs.iter().map(|m| m.id.clone()).collect()
    }
}

fn normalize(body: &str) -> String {
    body.replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_owned()
}
fn body_hash(body: &str) -> String {
    digest(normalize(body).as_bytes())
}
fn valid_id(s: &str) -> bool {
    identifier(s) && s.len() <= ID_MAX
}
fn validate_body(body: &str) -> Result<()> {
    if body.trim().is_empty()
        || body.encode_utf16().count() > BODY_UNITS
        || body
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        bail!(
            "invalid reply message: must be non-blank, at most 4000 UTF-16 units, no control characters"
        );
    }
    Ok(())
}
/// Only a fixed machine code from an internal error; never response text.
fn code(error: &anyhow::Error) -> String {
    let text = error.to_string();
    let head = text.split(':').next().unwrap_or("");
    if head.starts_with("reviewer_") || head.starts_with("delivery_unknown") {
        head.chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect()
    } else {
        "reviewer_request_failed".into()
    }
}

fn record_failure(record: &mut Record, error: &anyhow::Error) {
    let text = error.to_string();
    record.http_status = text
        .split("HTTP ")
        .nth(1)
        .and_then(|v| v.split_ascii_whitespace().next())
        .and_then(|v| v.trim_end_matches(';').parse().ok())
        .filter(|s| (100..600).contains(s));
    record.error_kind = Some(
        if record.http_status.is_some() {
            "http_status"
        } else if text.contains("invalid JSON") {
            "non_json"
        } else if text.contains("interrupted response") {
            "interrupted_response"
        } else if text.contains("oversized response") {
            "oversized_response"
        } else if text.contains("request interrupted") {
            "transport"
        } else if text.starts_with("reviewer_session_changed") {
            "session_changed"
        } else {
            "client"
        }
        .into(),
    );
}

impl Reviewer {
    fn load_record(&self, thread_id: &str) -> Result<Option<Record>> {
        let path = record_path(&self.root, thread_id);
        match fs::symlink_metadata(&path) {
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => bail!("reviewer_reply_outbox_unreadable"),
        }
        let bytes = private_read(&path, RECORD_MAX)
            .map_err(|_| anyhow::anyhow!("reviewer_reply_outbox_unreadable"))?;
        let record: Record = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("reviewer_reply_outbox_invalid"))?;
        if record.thread_id != thread_id {
            bail!("reviewer_reply_outbox_invalid");
        }
        Ok(Some(record))
    }

    fn save_record(&self, record: &Record) -> Result<()> {
        atomic_json(
            &record_path(&self.root, &record.thread_id),
            &serde_json::to_value(record)?,
        )
        .map(|_| ())
        .map_err(|_| {
            anyhow::anyhow!(
                "reviewer_reply_outbox_unwritable: persistence failed; no further request was made"
            )
        })
    }

    async fn verify_membership(&self, pin: &Pinned, app: &str, sid: &str) -> Result<bool> {
        let subs = self
            .pages_with(
                &format!("/iris/v1/apps/{app}/reviewSubmissions"),
                &[("limit", "200")],
                Some(pin),
            )
            .await?;
        let submission = subs["data"]
            .as_array()
            .and_then(|d| d.iter().find(|s| s["id"] == sid))
            .context("reviewer_reply_target_mismatch: submission does not belong to the app")?;
        Ok(submission
            .pointer("/attributes/state")
            .and_then(Value::as_str)
            == Some("UNRESOLVED_ISSUES"))
    }

    async fn verify_thread(
        &self,
        pin: &Pinned,
        sid: &str,
        tid: &str,
        unresolved: bool,
    ) -> Result<()> {
        let threads = self
            .pages_with(
                "/iris/v1/resolutionCenterThreads",
                &[("filter[reviewSubmission]", sid), ("limit", "200")],
                Some(pin),
            )
            .await?;
        let thread = threads["data"]
            .as_array()
            .and_then(|d| d.iter().find(|t| t["id"] == tid))
            .context("reviewer_reply_target_mismatch: thread does not belong to the submission")?;
        if thread.pointer("/attributes/canDeveloperAddNote") != Some(&json!(true))
            || !(unresolved
                || thread.pointer("/attributes/state").and_then(Value::as_str) == Some("ACTIVE"))
        {
            bail!("reviewer_reply_stale_context: thread no longer accepts developer notes");
        }
        Ok(())
    }

    async fn thread_messages(&self, pin: &Pinned, tid: &str) -> Result<Ctx> {
        let response = self
            .pages_with(
                &format!("/iris/v1/resolutionCenterThreads/{tid}/resolutionCenterMessages"),
                &[
                    (
                        "include",
                        "fromActor,rejections,resolutionCenterMessageAttachments",
                    ),
                    ("limit", "200"),
                ],
                Some(pin),
            )
            .await?;
        let schema = || {
            anyhow::anyhow!(
                "reviewer_actor_schema_unknown: cannot establish message actors or dates"
            )
        };
        let included = response["included"].as_array().cloned().unwrap_or_default();
        let mut msgs = Vec::new();
        for m in response["data"]
            .as_array()
            .context("reviewer_api_error_response")?
        {
            let id = m["id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(schema)?;
            // IDs are saved and returned; one that carries a credential value is refused.
            if !pin.public(id) {
                bail!("reviewer_response_invalid: unusable message identifier");
            }
            let created = m
                .pointer("/attributes/createdDate")
                .and_then(Value::as_str)
                .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
                .ok_or_else(schema)?
                .with_timezone(&Utc);
            let actor_id = m
                .pointer("/relationships/fromActor/data/id")
                .and_then(Value::as_str)
                .ok_or_else(schema)?;
            let attrs = included
                .iter()
                .find(|a| a["type"] == "actors" && a["id"] == actor_id)
                .map(|a| &a["attributes"])
                .ok_or_else(schema)?;
            let actor = match attrs["actorType"].as_str() {
                Some("APPLE") => Actor::Apple,
                // Actor constants verified in the Apple frontend bundle (module 59640).
                Some("USER" | "API_KEY" | "XCODE_CLOUD") => Actor::Developer,
                _ => return Err(schema()),
            };
            let body = match &m["attributes"]["messageBody"] {
                Value::String(s) => s.as_str(),
                Value::Null => "",
                _ => return Err(schema()),
            };
            msgs.push(Msg {
                id: id.to_owned(),
                created,
                actor,
                hash: body_hash(body),
            });
        }
        let newest = msgs
            .iter()
            .filter(|m| m.actor == Actor::Apple)
            .map(|m| m.created)
            .max();
        let latest_apple = match newest {
            None => None,
            Some(date) => {
                let mut ties = msgs
                    .iter()
                    .filter(|m| m.actor == Actor::Apple && m.created == date);
                let first = ties.next().map(|m| m.id.clone());
                if ties.next().is_some() {
                    bail!("reviewer_actor_schema_unknown: latest Apple message is ambiguous");
                }
                first
            }
        };
        Ok(Ctx { msgs, latest_apple })
    }

    /// `data` of the thread draft endpoint: `Null` only when Apple explicitly says no draft.
    async fn draft_response(&self, pin: &Pinned, tid: &str) -> Result<Value> {
        let unverifiable = |e: anyhow::Error| {
            if code(&e) == "reviewer_session_changed" {
                return e;
            }
            anyhow::anyhow!(
                "reviewer_reply_draft_unverifiable: {}; an existing draft cannot be ruled out",
                code(&e)
            )
        };
        let (response, _) = self
            .pinned(
                pin,
                Method::GET,
                self.url(
                    &format!("/iris/v1/resolutionCenterThreads/{tid}/resolutionCenterDraftMessage"),
                    &[
                        ("include", "resolutionCenterMessageAttachments,fromActor"),
                        ("limit[resolutionCenterMessageAttachments]", "1000"),
                    ],
                )?,
                None,
                false,
            )
            .await
            .map_err(unverifiable)?;
        match response.get("data") {
            Some(Value::Null) => Ok(response),
            Some(d) if d.is_object() => Ok(response),
            _ => Err(unverifiable(anyhow::anyhow!("reviewer_response_invalid"))),
        }
    }

    async fn draft_data(&self, pin: &Pinned, tid: &str) -> Result<Value> {
        Ok(self.draft_response(pin, tid).await?["data"].clone())
    }

    async fn validate_owned_pending_draft(
        &self,
        pin: &Pinned,
        record: &Record,
        response: &Value,
        draft_id: &str,
    ) -> Result<()> {
        let refuse = || {
            anyhow::anyhow!(
                "reviewer_reply_resume_refused: draft identity, body, attachments, creator or time could not be verified"
            )
        };
        let draft = &response["data"];
        let created = draft
            .pointer("/attributes/createdDate")
            .and_then(Value::as_str)
            .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
            .ok_or_else(refuse)?
            .timestamp();
        let now = Utc::now().timestamp();
        if draft["id"] != draft_id
            || draft["type"] != "resolutionCenterDraftMessages"
            || !draft
                .pointer("/attributes/messageBody")
                .and_then(Value::as_str)
                .is_some_and(|v| body_hash(v) == record.body_hash)
            || !draft
                .pointer("/relationships/resolutionCenterMessageAttachments/data")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
            || record.intent_at < now - 86400
            || record.intent_at > now + 5
            || created < record.intent_at - 5
            || created > now + 5
        {
            return Err(refuse());
        }
        let actor_id = draft
            .pointer("/relationships/fromActor/data/id")
            .and_then(Value::as_str)
            .ok_or_else(refuse)?;
        let actor = response["included"]
            .as_array()
            .and_then(|a| {
                a.iter()
                    .find(|v| v["type"] == "actors" && v["id"] == actor_id)
            })
            .ok_or_else(refuse)?;
        if actor
            .pointer("/attributes/actorType")
            .and_then(Value::as_str)
            != Some("USER")
        {
            return Err(refuse());
        }
        let email = actor
            .pointer("/attributes/userEmail")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(refuse)?;
        // Same pinned generation as every later preflight and the send.
        let session = self
            .pinned(
                pin,
                Method::GET,
                self.url("/olympus/v1/session", &[])?,
                None,
                false,
            )
            .await?
            .0;
        let current_email = session
            .pointer("/user/emailAddress")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(refuse)?;
        if !email.eq_ignore_ascii_case(current_email) {
            return Err(refuse());
        }
        Ok(())
    }

    async fn pending_draft_summary(&self, pin: &Pinned, record: &Record) -> Value {
        let response = match self.draft_response(pin, &record.thread_id).await {
            Ok(v) => v,
            Err(e) => return json!({"error":code(&e)}),
        };
        if response["data"].is_null() {
            return json!({"exists":false});
        }
        let Some(id) = response
            .pointer("/data/id")
            .and_then(Value::as_str)
            .filter(|v| valid_id(v) && pin.public(v))
        else {
            return json!({"exists":true,"resumable":false});
        };
        let phase_ok = matches!(
            record.state.as_str(),
            "intent" | "draft_created" | "draft_pending_validation"
        );
        let id_ok = record.draft_id.as_ref().is_none_or(|known| known == id);
        let verified = phase_ok
            && id_ok
            && self
                .validate_owned_pending_draft(pin, record, &response, id)
                .await
                .is_ok();
        json!({"exists":true,"draft_id":id,"resumable":verified})
    }

    fn delivered_message(ctx: &Ctx, record: &Record) -> Option<String> {
        ctx.msgs
            .iter()
            .find(|m| {
                m.actor == Actor::Developer
                    && !record.prior_message_ids.contains(&m.id)
                    && m.hash == record.body_hash
                    && record
                        .response_message_id
                        .as_ref()
                        .is_none_or(|id| *id == m.id)
            })
            .map(|m| m.id.clone())
    }

    pub(super) async fn reply_modern(&self, args: &Value) -> Result<Value> {
        let (app, sid, tid) = (
            arg(args, "app_id")?,
            arg(args, "review_submission_id")?,
            arg(args, "thread_id")?,
        );
        let expected = arg(args, "expected_apple_message_id")?;
        let body = arg(args, "message")?;
        if ![app, sid, tid, expected].into_iter().all(valid_id) {
            bail!("invalid identifier argument");
        }
        validate_body(body)?;
        let hash = body_hash(body);

        let _lock = ThreadLock::acquire(&self.root, tid)?;
        if let Some(previous) = self.load_record(tid)? {
            if previous.state != "delivered" && previous.state != "reset" {
                bail!(
                    "reviewer_reply_outbox_blocked: an earlier reply attempt is unfinished (phase {}); inspect the thread with get_app_review_reply_status and App Store Connect before sending",
                    previous.state
                );
            }
            if previous.state == "delivered" && previous.body_hash == hash {
                bail!("reviewer_reply_duplicate: identical body was already delivered; not sent");
            }
        }

        let pin = self.pin().await?;
        let unresolved = self.verify_membership(&pin, app, sid).await?;
        self.verify_thread(&pin, sid, tid, unresolved).await?;
        let ctx = self.thread_messages(&pin, tid).await?;
        if ctx.latest_apple.as_deref() != Some(expected) {
            bail!("reviewer_reply_stale_context: the newest Apple message is not the expected one");
        }
        if ctx
            .msgs
            .iter()
            .any(|m| m.actor == Actor::Developer && m.hash == hash)
        {
            bail!("reviewer_reply_duplicate: identical developer body already present; not sent");
        }
        if !self.draft_data(&pin, tid).await?.is_null() {
            bail!(
                "reviewer_reply_draft_conflict: a draft already exists on this thread; it was not touched"
            );
        }
        self.check_pin(&pin).await?;

        let mut record = Record {
            app_id: app.into(),
            submission_id: sid.into(),
            thread_id: tid.into(),
            expected_apple_message_id: expected.into(),
            body_hash: hash,
            intent_at: Utc::now().timestamp(),
            prior_message_ids: ctx.ids(),
            draft_id: None,
            response_message_id: None,
            state: "intent".into(),
            http_status: None,
            error_kind: None,
        };
        self.save_record(&record)?;

        let draft_payload = json!({"data":{"type":"resolutionCenterDraftMessages",
            "attributes":{"messageBody":body},
            "relationships":{"resolutionCenterThread":{"data":{"type":"resolutionCenterThreads","id":tid}}}}});
        let created = self
            .pinned(
                &pin,
                Method::POST,
                self.url("/iris/v1/resolutionCenterDraftMessages", &[])?,
                Some(draft_payload),
                true,
            )
            .await;
        let (draft, draft_headers) = match created {
            Ok(result) => result,
            Err(error) => {
                record_failure(&mut record, &error);
                let text = error.to_string();
                // A changed source is detected locally before the POST is sent.
                let rejected = text.starts_with("reviewer_session_changed")
                    || text.starts_with("reviewer_auth_required: HTTP 401")
                    || text.starts_with("reviewer_forbidden: HTTP 403")
                    || text
                        .strip_prefix("reviewer_api_unavailable: HTTP ")
                        .and_then(|v| v.parse::<u16>().ok())
                        .is_some_and(|status| (400..500).contains(&status) && status != 408);
                if rejected {
                    record.state = "draft_rejected".into();
                    self.save_record(&record)?;
                    bail!(
                        "reviewer_reply_draft_rejected: {}; no send was attempted; inspect with get_app_review_reply_status",
                        code(&error)
                    );
                }
                self.save_record(&record)?;
                bail!(
                    "reviewer_reply_draft_unknown: {}; a draft may exist and this thread stays blocked until inspected",
                    code(&error)
                );
            }
        };
        let draft_id = draft
            .pointer("/data/id")
            .and_then(Value::as_str)
            .filter(|id| {
                valid_id(id)
                    && pin.public(id)
                    && draft.pointer("/data/type") == Some(&json!("resolutionCenterDraftMessages"))
            })
            .map(str::to_owned);
        let Some(draft_id) = draft_id else {
            record.state = "draft_pending_validation".into();
            record.http_status = draft_headers
                .get("x-asc-mcp-status")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok());
            record.error_kind = Some(
                if draft["data"].is_null() {
                    "empty_response"
                } else {
                    "draft_schema"
                }
                .into(),
            );
            self.save_record(&record)?;
            bail!(
                "reviewer_reply_draft_pending_validation: create succeeded without a usable ID; read status and explicitly resume the verified draft; no send was attempted"
            );
        };
        record.draft_id = Some(draft_id.clone());
        record.state = "draft_created".into();
        self.save_record(&record)?;

        self.send_created_draft(&pin, &mut record).await
    }

    async fn send_created_draft(&self, pin: &Pinned, record: &mut Record) -> Result<Value> {
        let app = record.app_id.clone();
        let sid = record.submission_id.clone();
        let tid = record.thread_id.clone();
        let expected = record.expected_apple_message_id.clone();
        let draft_id = record
            .draft_id
            .clone()
            .context("reviewer_reply_resume_refused: missing draft identity")?;
        let unsent = |reason: String| {
            anyhow::anyhow!(
                "{reason}; the draft was left unsent and this thread stays blocked until inspected"
            )
        };
        let current = self
            .draft_data(pin, &tid)
            .await
            .map_err(|e| unsent(code(&e)))?;
        let attachments = current.pointer("/relationships/resolutionCenterMessageAttachments/data");
        let clean = current["id"] == draft_id.as_str()
            && current
                .pointer("/attributes/messageBody")
                .and_then(Value::as_str)
                .is_some_and(|b| body_hash(b) == record.body_hash)
            && attachments.is_some_and(|a| a.as_array().is_some_and(Vec::is_empty));
        if !clean {
            return Err(unsent(
                "reviewer_reply_draft_conflict: draft read-back differs from the request".into(),
            ));
        }
        let again = async {
            let unresolved = self.verify_membership(pin, &app, &sid).await?;
            self.verify_thread(pin, &sid, &tid, unresolved).await?;
            self.thread_messages(pin, &tid).await
        }
        .await
        .map_err(|e| unsent(code(&e)))?;
        if again.latest_apple.as_deref() != Some(expected.as_str())
            || again
                .ids()
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                != record.prior_message_ids.iter().cloned().collect()
        {
            return Err(unsent(
                "reviewer_reply_stale_context: the thread changed after the draft was created"
                    .into(),
            ));
        }

        self.check_pin(pin).await.map_err(|e| unsent(code(&e)))?;
        record.state = "send_attempted".into();
        self.save_record(record)?;
        let send_payload = json!({"data":{"type":"resolutionCenterMessages",
            "relationships":{"createFromDraftMessage":{"data":{"type":"resolutionCenterDraftMessages","id":draft_id}}}}});
        let sent = self
            .pinned(
                pin,
                Method::POST,
                self.url("/iris/v1/resolutionCenterMessages", &[])?,
                Some(send_payload),
                true,
            )
            .await;
        let (response, _) = match sent {
            Ok(ok) => ok,
            // The generation check runs locally before any byte is sent: definitely unsent.
            Err(e) if e.to_string().starts_with("reviewer_session_changed") => {
                record_failure(record, &e);
                record.state = "draft_created".into();
                self.save_record(record)?;
                return Err(unsent(code(&e)));
            }
            Err(e) => {
                record_failure(record, &e);
                let _ = self.save_record(record);
                bail!(
                    "delivery_unknown: send outcome is unknown ({}); do not resend; inspect with get_app_review_reply_status",
                    code(&e)
                );
            }
        };
        record.state = "accepted".into();
        record.response_message_id = response
            .pointer("/data/id")
            .and_then(Value::as_str)
            .filter(|id| {
                valid_id(id)
                    && pin.public(id)
                    && response.pointer("/data/type") == Some(&json!("resolutionCenterMessages"))
            })
            .map(str::to_owned);
        let _ = self.save_record(record);

        match self.thread_messages(pin, &tid).await {
            Ok(after) => match Self::delivered_message(&after, record) {
                Some(message_id) => {
                    record.state = "delivered".into();
                    record.response_message_id = Some(message_id.clone());
                    let _ = self.save_record(record);
                    Ok(json!({"status":"delivered","thread_id":tid,"message_id":message_id}))
                }
                None => bail!(
                    "delivery_unknown: Apple accepted the send but the read-back did not confirm it; do not resend; inspect with get_app_review_reply_status"
                ),
            },
            Err(e) => bail!(
                "delivery_unknown: Apple accepted the send but the read-back failed ({}); do not resend; inspect with get_app_review_reply_status",
                code(&e)
            ),
        }
    }

    pub(super) async fn reply_status(&self, args: &Value) -> Result<Value> {
        let (app, sid, tid) = (
            arg(args, "app_id")?,
            arg(args, "review_submission_id")?,
            arg(args, "thread_id")?,
        );
        if ![app, sid, tid].into_iter().all(valid_id) {
            bail!("invalid identifier argument");
        }
        let _lock = ThreadLock::acquire(&self.root, tid)?;
        let Some(mut record) = self.load_record(tid)? else {
            return Ok(json!({"status":"not_attempted","thread_id":tid}));
        };
        if record.app_id != app || record.submission_id != sid {
            bail!(
                "reviewer_reply_target_mismatch: the recorded attempt targets a different app or submission"
            );
        }
        if record.state == "reset" {
            return Ok(json!({"status":"not_attempted","thread_id":tid}));
        }
        if record.state == "delivered" {
            return Ok(
                json!({"status":"delivered","thread_id":tid,"message_id":record.response_message_id}),
            );
        }
        let phase = record.state.clone();
        let unsent_phase = matches!(
            phase.as_str(),
            "intent" | "draft_created" | "draft_rejected" | "draft_pending_validation"
        );
        let pin = self.pin().await;
        let draft_summary = match (&pin, unsent_phase) {
            (Ok(pin), true) => self.pending_draft_summary(pin, &record).await,
            (Err(e), true) => json!({"error":code(e)}),
            _ => Value::Null,
        };
        let unknown = |error: Option<String>| {
            let status = if matches!(
                phase.as_str(),
                "intent" | "draft_created" | "draft_rejected" | "draft_pending_validation"
            ) {
                "not_sent"
            } else {
                "delivery_unknown"
            };
            json!({"status":status,"thread_id":tid,"phase":phase,
                "blocked":true,"read_back_error":error,"draft":draft_summary,"http_status":record.http_status,"error_kind":record.error_kind,
                "next":"inspect the thread in App Store Connect; the reply record is never removed automatically"})
        };
        let read = match &pin {
            Ok(pin) => self.thread_messages(pin, tid).await,
            Err(e) => Err(anyhow::anyhow!("{}", code(e))),
        };
        match read {
            Ok(ctx) => match Self::delivered_message(&ctx, &record) {
                Some(message_id) => {
                    record.state = "delivered".into();
                    record.response_message_id = Some(message_id.clone());
                    self.save_record(&record)?;
                    Ok(json!({"status":"delivered","thread_id":tid,"message_id":message_id}))
                }
                None => Ok(unknown(None)),
            },
            Err(e) => Ok(unknown(Some(code(&e)))),
        }
    }

    pub(super) async fn resume_unsent_reply(&self, args: &Value) -> Result<Value> {
        let (app, sid, tid, expected, body, draft_id) = (
            arg(args, "app_id")?,
            arg(args, "review_submission_id")?,
            arg(args, "thread_id")?,
            arg(args, "expected_apple_message_id")?,
            arg(args, "message")?,
            arg(args, "draft_id")?,
        );
        if ![app, sid, tid, expected, draft_id]
            .into_iter()
            .all(valid_id)
        {
            bail!("invalid identifier argument");
        }
        validate_body(body)?;
        let _lock = ThreadLock::acquire(&self.root, tid)?;
        let mut record = self
            .load_record(tid)?
            .context("reviewer_reply_resume_refused: no pending attempt exists")?;
        if !matches!(
            record.state.as_str(),
            "intent" | "draft_created" | "draft_pending_validation"
        ) || record.response_message_id.is_some()
            || (record.state == "draft_created" && record.draft_id.is_none())
            || record.app_id != app
            || record.submission_id != sid
            || record.expected_apple_message_id != expected
            || record.body_hash != body_hash(body)
            || record.draft_id.as_ref().is_some_and(|id| id != draft_id)
        {
            bail!(
                "reviewer_reply_resume_refused: only the unchanged original request with no previous send attempt can resume"
            );
        }
        let pin = self.pin().await?;
        let unresolved = self.verify_membership(&pin, app, sid).await?;
        self.verify_thread(&pin, sid, tid, unresolved).await?;
        let ctx = self.thread_messages(&pin, tid).await?;
        if ctx.latest_apple.as_deref() != Some(expected)
            || ctx
                .ids()
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                != record.prior_message_ids.iter().cloned().collect()
        {
            bail!("reviewer_reply_stale_context");
        }
        let draft = self.draft_response(&pin, tid).await?;
        self.validate_owned_pending_draft(&pin, &record, &draft, draft_id)
            .await?;
        record.draft_id = Some(draft_id.into());
        record.state = "draft_created".into();
        self.save_record(&record)?;
        self.send_created_draft(&pin, &mut record).await
    }

    /// Explicit local recovery only for a definitive draft rejection, never for an unknown send.
    pub(super) async fn reset_rejected_reply(&self, args: &Value) -> Result<Value> {
        let (app, sid, tid) = (
            arg(args, "app_id")?,
            arg(args, "review_submission_id")?,
            arg(args, "thread_id")?,
        );
        if ![app, sid, tid].into_iter().all(valid_id) {
            bail!("invalid identifier argument");
        }
        let _lock = ThreadLock::acquire(&self.root, tid)?;
        let mut record = self
            .load_record(tid)?
            .context("reviewer_reply_reset_refused: no rejected draft attempt exists")?;
        if record.app_id != app || record.submission_id != sid {
            bail!("reviewer_reply_target_mismatch");
        }
        if record.state != "draft_rejected"
            || record.draft_id.is_some()
            || record.response_message_id.is_some()
        {
            bail!(
                "reviewer_reply_reset_refused: only a definitively rejected draft creation can be reset; unknown outcomes remain blocked"
            );
        }
        let pin = self.pin().await?;
        let unresolved = self.verify_membership(&pin, app, sid).await?;
        self.verify_thread(&pin, sid, tid, unresolved).await?;
        if !self.draft_data(&pin, tid).await?.is_null() {
            bail!("reviewer_reply_draft_conflict: remote draft exists; nothing was changed");
        }
        let messages = self.thread_messages(&pin, tid).await?;
        if messages
            .msgs
            .iter()
            .any(|m| m.actor == Actor::Developer && m.hash == record.body_hash)
        {
            bail!(
                "reviewer_reply_duplicate: matching developer message already exists; nothing was changed"
            );
        }
        record.state = "reset".into();
        self.save_record(&record)?;
        Ok(json!({"status":"reset","thread_id":tid,"sent":false}))
    }
}
