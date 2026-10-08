//! Behavior tests for the modern Iris reviewer reply tools (written before the implementation).
//!
//! Contract for the implementer (names chosen here; adjust tests only with the orchestrator):
//! * Tools `reply_app_review_message` (required: app_id, review_submission_id, thread_id,
//!   expected_apple_message_id, message) and `get_app_review_reply_status`
//!   (required: app_id, review_submission_id, thread_id).
//! * Preflight GETs, in any order, but all before any write:
//!   - `GET /iris/v1/apps/{app}/reviewSubmissions` -> `data[].id` must contain the submission.
//!   - `GET /iris/v1/resolutionCenterThreads?filter[reviewSubmission]={sid}` -> `data[]` with
//!     `attributes.canDeveloperAddNote`; thread must be a member.
//!   - `GET /iris/v1/resolutionCenterThreads/{tid}/resolutionCenterMessages` -> `data[]` with
//!     `id`, `attributes.createdDate`, `attributes.messageBody`, and
//!     `relationships.fromActor.data.id` of `apple` or `developer` (see `msg`).
//!     The newest Apple message id must equal `expected_apple_message_id`.
//!   - `GET /iris/v1/resolutionCenterThreads/{tid}/resolutionCenterDraftMessage` -> `{"data":null}`.
//! * Writes: `POST /iris/v1/resolutionCenterDraftMessages` (JSON:API body, `type`
//!   `resolutionCenterDraftMessages`), then `POST /iris/v1/resolutionCenterMessages` referencing
//!   the draft id. Both carry `Content-Type: application/vnd.api+json` and `X-Csrf-Itc: [asc-ui]`.
//!   Neither POST is ever retried.
//! * Success: Ok value with `"status":"delivered"` after a read-back finds a new developer message.
//!   Message POST 401/5xx or an unconfirmed read-back: Err containing `delivery_unknown`.
//! * Error code substrings: `reviewer_reply_target_mismatch`, `reviewer_reply_stale_context`,
//!   `reviewer_reply_draft_conflict`, `reviewer_reply_duplicate`. All fire before any POST.
//! * `get_app_review_reply_status` never POSTs; Ok value with `"status"` of `delivered` or
//!   `delivery_unknown`.
use super::*;
use std::collections::HashMap;
use std::io::Read as _;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
struct Req {
    method: String,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    body: String,
}

#[derive(Clone)]
struct Cfg {
    existing_draft: bool,
    /// Developer message already in the thread before the reply (for duplicate detection).
    prior_dev_body: Option<String>,
    message_post_status: u16,
    /// Whether a successful message POST becomes visible in later message GETs.
    post_visible: bool,
    /// Adds a message whose actor type is unrecognised.
    bad_actor: bool,
    missing_attachment_metadata: bool,
    submission_state: &'static str,
    thread_state: Option<&'static str>,
    draft_post_status: u16,
    developer_actor_type: &'static str,
    clock_offset_secs: i64,
    wrong_draft_author: bool,
    attached_draft: bool,
    /// Body Apple stores for a created draft instead of the requested one.
    draft_body_override: Option<&'static str>,
    /// Body Apple stores for a sent message instead of the draft body.
    sent_body_override: Option<&'static str>,
    /// Replace the authentication source (different account) when this path is first served.
    swap_source: Option<(&'static str, PathBuf)>,
    /// Rotate the session cookie (same source generation) on `GET /olympus/v1/session`.
    rotate_on_session: bool,
    /// Thread list pagination: `kept`, `changed` or `dropped` submission filter on page 2.
    thread_next: Option<&'static str>,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            existing_draft: false,
            prior_dev_body: None,
            message_post_status: 201,
            post_visible: true,
            bad_actor: false,
            missing_attachment_metadata: false,
            submission_state: "UNRESOLVED_ISSUES",
            thread_state: None,
            draft_post_status: 201,
            developer_actor_type: "USER",
            clock_offset_secs: 0,
            wrong_draft_author: false,
            attached_draft: false,
            draft_body_override: None,
            sent_body_override: None,
            swap_source: None,
            rotate_on_session: false,
            thread_next: None,
        }
    }
}

#[derive(Default)]
struct St {
    log: Vec<Req>,
    sent: Option<String>,
    /// Draft created by the scripted server: (body, created date). Removed once sent.
    draft: Option<(String, String)>,
    /// Creation date of the delivered message (real clock, never a hardcoded past date).
    sent_date: Option<String>,
    swapped: bool,
}

/// A different account's valid session, published as a replacement source.
fn account_b() -> Value {
    json!({"cookies":[{"name":"session","value":"account-b-sentinel","domain":"apple.com","path":"/"}]})
}

fn msg(id: &str, actor: &str, date: &str, body: &str) -> Value {
    json!({"type":"resolutionCenterMessages","id":id,
        "attributes":{"createdDate":date,"messageBody":body},
        "relationships":{"fromActor":{"data":{"type":"actors","id":actor}}}})
}

fn actors(kind: &str) -> Value {
    json!([
        {"type":"actors","id":"apple","attributes":{"actorType":"APPLE"}},
        {"type":"actors","id":"developer","attributes":{"actorType":kind}}
    ])
}

fn route(cfg: &Cfg, st: &mut St, r: &Req) -> (u16, Value) {
    let p = r.path.as_str();
    if let Some((path, source)) = &cfg.swap_source
        && *path == p
        && !st.swapped
    {
        st.swapped = true;
        atomic_json(source, &account_b()).unwrap();
    }
    if let Some(mode) = cfg.thread_next
        && r.method == "GET"
        && p == "/iris/v1/resolutionCenterThreads"
    {
        let params: Vec<(String, String)> = url::form_urlencoded::parse(r.query.as_bytes())
            .into_owned()
            .collect();
        let filter = params
            .iter()
            .find(|(k, _)| k == "filter[reviewSubmission]")
            .map(|(_, v)| v.as_str());
        if !params.iter().any(|(k, _)| k == "cursor") {
            let scope = match mode {
                "kept" => "&filter%5BreviewSubmission%5D=sub1",
                "changed" => "&filter%5BreviewSubmission%5D=sub-other",
                _ => "",
            };
            let host = r.headers.get("host").cloned().unwrap_or_default();
            let next =
                format!("http://{host}/iris/v1/resolutionCenterThreads?cursor=2&limit=200{scope}");
            return (200, json!({"data":[],"links":{"next":next}}));
        }
        // Only an unscoped or foreign listing contains thr1; sub1's own second page is empty.
        let foreign = mode != "kept" && filter != Some("sub1");
        let data = if mode == "kept" || foreign {
            json!([{"type":"resolutionCenterThreads","id":"thr1",
                "attributes":{"canDeveloperAddNote":true,"state":cfg.thread_state}}])
        } else {
            json!([])
        };
        return (200, json!({"data":data}));
    }
    match (r.method.as_str(), p) {
        ("GET", "/olympus/v1/session") => (
            200,
            json!({"user":{"emailAddress":"developer@example.invalid"}}),
        ),
        ("GET", "/iris/v1/apps/123/reviewSubmissions") => (
            200,
            json!({"data":[{"type":"reviewSubmissions","id":"sub1","attributes":{"state":cfg.submission_state}}]}),
        ),
        ("GET", "/iris/v1/apps/999/reviewSubmissions") => (
            200,
            json!({"data":[{"type":"reviewSubmissions","id":"sub9"}]}),
        ),
        ("GET", "/iris/v1/resolutionCenterThreads") => (
            200,
            json!({"data":[{"type":"resolutionCenterThreads","id":"thr1",
                "attributes":{"canDeveloperAddNote":true,"state":cfg.thread_state}}]}),
        ),
        ("GET", "/iris/v1/resolutionCenterThreads/thr1/resolutionCenterMessages") => {
            let mut data = vec![msg(
                "m-apple-1",
                "apple",
                "2026-01-01T00:00:00Z",
                "Please fix",
            )];
            if cfg.bad_actor {
                data.push(msg("m-x", "robot", "2026-01-02T00:00:00Z", "?"));
            }
            if let Some(b) = &cfg.prior_dev_body {
                data.push(msg("m-dev-0", "developer", "2026-01-02T00:00:00Z", b));
            }
            if let (Some(b), Some(d), true) = (&st.sent, &st.sent_date, cfg.post_visible) {
                data.push(msg("m-dev-new", "developer", d, b));
            }
            let mut inc = actors(cfg.developer_actor_type);
            inc.as_array_mut()
                .unwrap()
                .push(json!({"type":"actors","id":"robot","attributes":{"actorType":"ROBOT"}}));
            (200, json!({"data":data,"included":inc}))
        }
        ("GET", "/iris/v1/resolutionCenterThreads/thr1/resolutionCenterDraftMessage") => {
            if cfg.existing_draft {
                (
                    200,
                    json!({"data":{"type":"resolutionCenterDraftMessages","id":"draft-old",
                    "attributes":{"messageBody":"someone else's draft"}}}),
                )
            } else if let Some((body, date)) = &st.draft {
                let mut response = json!({"data":{"type":"resolutionCenterDraftMessages","id":"draft-new",
                    "attributes":{"messageBody":body,"createdDate":date,"lastUpdatedDate":date},
                    "relationships":{"fromActor":{"data":{"type":"actors","id":"developer"}},
                        "resolutionCenterMessageAttachments":{"data":[]}}},
                    "included":actors(cfg.developer_actor_type)});
                response["included"][1]["attributes"]["actorType"] = json!("USER");
                response["included"][1]["attributes"]["userEmail"] =
                    json!(if cfg.wrong_draft_author {
                        "someone@example.invalid"
                    } else {
                        "developer@example.invalid"
                    });
                if cfg.attached_draft {
                    response["data"]["relationships"]["resolutionCenterMessageAttachments"]["data"] =
                        json!([{"type":"resolutionCenterMessageAttachments","id":"attachment-1"}]);
                }
                if cfg.missing_attachment_metadata {
                    response["data"]["relationships"]
                        .as_object_mut()
                        .unwrap()
                        .remove("resolutionCenterMessageAttachments");
                }
                (200, response)
            } else {
                (200, json!({"data":null}))
            }
        }
        ("POST", "/iris/v1/resolutionCenterDraftMessages") => {
            if !(200..300).contains(&cfg.draft_post_status) {
                return (
                    cfg.draft_post_status,
                    json!({"errors":[{"code":"REFUSED"}]}),
                );
            }
            let v: Value = serde_json::from_str(&r.body).unwrap_or(Value::Null);
            let body = v
                .pointer("/data/attributes/messageBody")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let body = cfg.draft_body_override.map(str::to_owned).unwrap_or(body);
            st.draft = Some((body, chrono::Utc::now().to_rfc3339()));
            // 204 deliberately has no usable representation; the created draft remains readable.
            (
                cfg.draft_post_status,
                json!({"data":{"type":"resolutionCenterDraftMessages","id":"draft-new"}}),
            )
        }
        ("POST", "/iris/v1/resolutionCenterMessages") => {
            if cfg.message_post_status < 300 {
                st.sent = st
                    .draft
                    .take()
                    .map(|(body, _)| cfg.sent_body_override.map(str::to_owned).unwrap_or(body));
                st.sent_date = Some(
                    (chrono::Utc::now() + chrono::Duration::seconds(cfg.clock_offset_secs))
                        .to_rfc3339(),
                );
            }
            (
                cfg.message_post_status,
                json!({"data":{"type":"resolutionCenterMessages","id":"m-dev-new"}}),
            )
        }
        _ => (404, json!({"errors":[{"code":"NOT_FOUND"}]})),
    }
}

/// Minimal scripted HTTP/1.1 server (test-only; no production hooks).
fn serve(cfg: Cfg) -> (String, Arc<Mutex<St>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let st = Arc::new(Mutex::new(St::default()));
    let shared = st.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            let (head_end, len) = loop {
                let n = s.read(&mut tmp).unwrap_or(0);
                if n == 0 {
                    break (0, 0);
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                    let len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    break (i + 4, len);
                }
            };
            if head_end == 0 {
                continue;
            }
            while buf.len() < head_end + len {
                let n = s.read(&mut tmp).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
            let mut lines = head.lines();
            let mut first = lines.next().unwrap_or("").split_whitespace();
            let method = first.next().unwrap_or("").to_string();
            let target = first.next().unwrap_or("").to_string();
            let (path, query) = target.split_once('?').unwrap_or((&target, ""));
            let headers = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
                .collect();
            let req = Req {
                method,
                path: path.to_string(),
                query: query.to_string(),
                headers,
                body: String::from_utf8_lossy(&buf[head_end..]).to_string(),
            };
            let (status, body) = {
                let mut g = shared.lock().unwrap();
                g.log.push(req.clone());
                route(&cfg, &mut g, &req)
            };
            let body = body.to_string();
            let rotate = if cfg.rotate_on_session && req.path == "/olympus/v1/session" {
                "Set-Cookie: session=rotated-sentinel; Domain=apple.com; Path=/\r\n"
            } else {
                ""
            };
            let _ = write!(
                s,
                "HTTP/1.1 {status} X\r\nContent-Type: application/vnd.api+json\r\nContent-Length: {}\r\nConnection: close\r\ncsrf: safe\r\ncsrf_ts: 123\r\n{rotate}\r\n{body}",
                body.len()
            );
        }
    });
    (origin, st)
}

fn args() -> Value {
    json!({"app_id":"123","review_submission_id":"sub1","thread_id":"thr1",
        "expected_apple_message_id":"m-apple-1","message":"Thanks, fixed"})
}

fn posts(st: &Arc<Mutex<St>>) -> Vec<Req> {
    st.lock()
        .unwrap()
        .log
        .iter()
        .filter(|r| r.method == "POST")
        .cloned()
        .collect()
}

async fn setup(cfg: Cfg) -> (tempfile::TempDir, Reviewer, Arc<Mutex<St>>) {
    let (origin, st) = serve(cfg);
    let dir = tempfile::tempdir().unwrap();
    let r = Reviewer::for_test(&origin, dir.path());
    (dir, r, st)
}

fn has_tools(r: &Reviewer) {
    let names: Vec<String> = r
        .tools()
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_owned))
        .collect();
    for n in ["reply_app_review_message", "get_app_review_reply_status"] {
        assert!(names.iter().any(|x| x == n), "tool {n} must be registered");
    }
}

async fn expect_rejected(cfg: Cfg, a: Value, code: &str) {
    let (_d, r, st) = setup(cfg).await;
    has_tools(&r);
    let err = r
        .call("reply_app_review_message", &a)
        .await
        .expect_err("reply must be rejected")
        .to_string();
    assert!(err.contains(code), "expected {code}, got: {err}");
    assert!(posts(&st).is_empty(), "no POST may happen before {code}");
}

#[tokio::test]
async fn tools_are_registered_with_required_arguments() {
    let (_d, r, _s) = setup(Cfg::default()).await;
    has_tools(&r);
    let tools = r.tools();
    let req = |n: &str| -> Vec<String> {
        let t = tools.iter().find(|t| t["name"] == n).expect(n);
        t["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect()
    };
    for k in [
        "app_id",
        "review_submission_id",
        "thread_id",
        "expected_apple_message_id",
        "message",
    ] {
        assert!(
            req("reply_app_review_message").iter().any(|x| x == k),
            "{k}"
        );
    }
    for k in ["app_id", "review_submission_id", "thread_id"] {
        assert!(
            req("get_app_review_reply_status").iter().any(|x| x == k),
            "{k}"
        );
    }
}

#[tokio::test]
async fn happy_path_sends_draft_then_message_and_reads_back_delivered() {
    let (_d, r, st) = setup(Cfg::default()).await;
    has_tools(&r);
    let out = r
        .call("reply_app_review_message", &args())
        .await
        .expect("reply should succeed");
    assert_eq!(out["status"], "delivered", "{out}");
    let p = posts(&st);
    assert_eq!(p.len(), 2, "exactly one draft POST then one message POST");
    assert_eq!(p[0].path, "/iris/v1/resolutionCenterDraftMessages");
    assert_eq!(p[1].path, "/iris/v1/resolutionCenterMessages");
    for q in &p {
        assert_eq!(
            q.headers.get("origin").map(String::as_str),
            Some(r.origin.origin().ascii_serialization().as_str())
        );
        assert_eq!(
            q.headers.get("referer").map(String::as_str),
            Some(r.origin.as_str())
        );
        assert_eq!(
            q.headers.get("content-type").map(String::as_str),
            Some("application/vnd.api+json")
        );
        assert_eq!(
            q.headers.get("x-csrf-itc").map(String::as_str),
            Some("[asc-ui]")
        );
        let v: Value = serde_json::from_str(&q.body).expect("JSON:API body");
        assert!(v["data"]["type"].is_string(), "{v}");
    }
    assert!(p[0].body.contains("Thanks, fixed"));
    assert!(
        p[1].body.contains("draft-new"),
        "message must reference the created draft"
    );
    let log = st.lock().unwrap().log.clone();
    for request in log
        .iter()
        .filter(|q| q.path.ends_with("/resolutionCenterDraftMessage"))
    {
        let params: HashMap<_, _> = url::form_urlencoded::parse(request.query.as_bytes())
            .into_owned()
            .collect();
        assert_eq!(
            params.get("include").map(String::as_str),
            Some("resolutionCenterMessageAttachments,fromActor"),
            "must request draft attachment metadata"
        );
    }

    let first_post = log.iter().position(|r| r.method == "POST").unwrap();
    assert!(
        log[..first_post]
            .iter()
            .any(|r| r.query.contains("reviewSubmission") && r.query.contains("sub1")),
        "thread membership preflight must filter by reviewSubmission"
    );
    let last_post = log.iter().rposition(|r| r.method == "POST").unwrap();
    assert!(
        log[last_post + 1..]
            .iter()
            .any(|r| r.path.ends_with("/resolutionCenterMessages")),
        "read-back GET of messages must follow the message POST"
    );
}

#[tokio::test]
async fn wrong_app_submission_membership_is_target_mismatch() {
    let mut a = args();
    a["app_id"] = json!("999");
    expect_rejected(Cfg::default(), a, "reviewer_reply_target_mismatch").await;
}

#[tokio::test]
async fn wrong_thread_is_target_mismatch() {
    let mut a = args();
    a["thread_id"] = json!("thr2");
    expect_rejected(Cfg::default(), a, "reviewer_reply_target_mismatch").await;
}

#[tokio::test]
async fn stale_apple_message_id_is_stale_context() {
    let mut a = args();
    a["expected_apple_message_id"] = json!("m-apple-0");
    expect_rejected(Cfg::default(), a, "reviewer_reply_stale_context").await;
}

#[tokio::test]
async fn existing_draft_is_conflict_and_never_posts() {
    let cfg = Cfg {
        existing_draft: true,
        ..Cfg::default()
    };
    expect_rejected(cfg, args(), "reviewer_reply_draft_conflict").await;
}

#[tokio::test]
async fn duplicate_developer_body_is_rejected_before_posting() {
    let cfg = Cfg {
        prior_dev_body: Some("Thanks, fixed".into()),
        ..Cfg::default()
    };
    expect_rejected(cfg, args(), "reviewer_reply_duplicate").await;
}

#[tokio::test]
async fn invalid_message_is_rejected_before_any_request() {
    let (_d, r, st) = setup(Cfg::default()).await;
    has_tools(&r);
    let long = "x".repeat(4001);
    for m in ["", "   ", long.as_str()] {
        let mut a = args();
        a["message"] = json!(m);
        assert!(r.call("reply_app_review_message", &a).await.is_err());
    }
    assert!(
        st.lock().unwrap().log.is_empty(),
        "no HTTP for invalid input"
    );
}

#[tokio::test]
async fn message_post_5xx_is_delivery_unknown_and_not_retried() {
    let cfg = Cfg {
        message_post_status: 503,
        ..Cfg::default()
    };
    let (_d, r, st) = setup(cfg).await;
    has_tools(&r);
    let err = r
        .call("reply_app_review_message", &args())
        .await
        .expect_err("5xx must not claim success")
        .to_string();
    assert!(err.contains("delivery_unknown"), "{err}");
    assert!(!err.contains("secret-sentinel"));
    let msg_posts = posts(&st)
        .into_iter()
        .filter(|r| r.path == "/iris/v1/resolutionCenterMessages")
        .count();
    assert_eq!(msg_posts, 1, "message POST must never be retried");
}

#[tokio::test]
async fn message_post_401_is_delivery_unknown_and_not_retried() {
    let cfg = Cfg {
        message_post_status: 401,
        ..Cfg::default()
    };
    let (_d, r, st) = setup(cfg).await;
    has_tools(&r);
    let err = r
        .call("reply_app_review_message", &args())
        .await
        .expect_err("401 must not claim success")
        .to_string();
    assert!(err.contains("delivery_unknown"), "{err}");
    let msg_posts = posts(&st)
        .into_iter()
        .filter(|r| r.path == "/iris/v1/resolutionCenterMessages")
        .count();
    assert_eq!(
        msg_posts, 1,
        "401 on the message POST must not trigger replay"
    );
}

#[tokio::test]
async fn accepted_post_without_readback_is_delivery_unknown() {
    let cfg = Cfg {
        post_visible: false,
        ..Cfg::default()
    };
    let (_d, r, _st) = setup(cfg).await;
    has_tools(&r);
    let res = r.call("reply_app_review_message", &args()).await;
    let text = match &res {
        Ok(v) => v.to_string(),
        Err(e) => e.to_string(),
    };
    assert!(text.contains("delivery_unknown"), "{text}");
    assert!(!text.contains("\"delivered\""), "{text}");
}

#[tokio::test]
async fn status_reports_not_attempted_without_an_outbox() {
    let (_d, r, st) = setup(Cfg::default()).await;
    has_tools(&r);
    let out = r
        .call(
            "get_app_review_reply_status",
            &json!({"app_id":"123","review_submission_id":"sub1","thread_id":"thr1"}),
        )
        .await
        .expect("status should succeed");
    assert_eq!(out["status"], "not_attempted", "{out}");
    assert!(posts(&st).is_empty(), "status must never POST");
}

#[tokio::test]
async fn status_reports_delivered_after_a_reply_and_never_posts() {
    let (_d, r, st) = setup(Cfg::default()).await;
    has_tools(&r);
    r.call("reply_app_review_message", &args())
        .await
        .expect("reply");
    let before = posts(&st).len();
    let out = r
        .call(
            "get_app_review_reply_status",
            &json!({"app_id":"123","review_submission_id":"sub1","thread_id":"thr1"}),
        )
        .await
        .expect("status should succeed");
    assert_eq!(out["status"], "delivered", "{out}");
    assert_eq!(posts(&st).len(), before, "status must never POST");
}

fn status_args() -> Value {
    json!({"app_id":"123","review_submission_id":"sub1","thread_id":"thr1"})
}

fn write_record(dir: &Path, state: &str, body: &str) {
    let rec = json!({"app_id":"123","submission_id":"sub1","thread_id":"thr1",
        "expected_apple_message_id":"m-apple-1","body_hash":digest(body.as_bytes()),
        "intent_at":0,"prior_message_ids":["m-apple-1"],"draft_id":null,
        "response_message_id":null,"state":state});
    atomic_json(&reply::record_path(dir, "thr1"), &rec).unwrap();
}

#[tokio::test]
async fn unfinished_record_blocks_a_different_body_after_restart() {
    for state in ["intent", "draft_created", "send_attempted", "accepted"] {
        let (d, r, st) = setup(Cfg::default()).await;
        write_record(d.path(), state, "earlier text");
        let mut a = args();
        a["message"] = json!("A different reply");
        let err = r
            .call("reply_app_review_message", &a)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("reviewer_reply_outbox_blocked"),
            "{state}: {err}"
        );
        assert!(
            st.lock().unwrap().log.is_empty(),
            "{state}: blocked before any request"
        );
        // A fresh Reviewer over the same root (restart) is blocked too, and status stays GET-only.
        let again = Reviewer::for_test(r.origin.as_str(), d.path());
        let out = again
            .call("get_app_review_reply_status", &status_args())
            .await
            .unwrap();
        assert_eq!(
            out["status"],
            if matches!(state, "intent" | "draft_created") {
                "not_sent"
            } else {
                "delivery_unknown"
            },
            "{out}"
        );
        assert!(posts(&st).is_empty());
        assert!(!out.to_string().contains("secret-sentinel"));
    }
}

#[tokio::test]
async fn delivered_record_allows_a_new_body_but_blocks_the_same_one() {
    let (d, r, st) = setup(Cfg::default()).await;
    write_record(d.path(), "delivered", "Thanks, fixed");
    let err = r
        .call("reply_app_review_message", &args())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("reviewer_reply_duplicate"), "{err}");
    assert!(posts(&st).is_empty());
    let mut a = args();
    a["message"] = json!("Another note");
    assert_eq!(
        r.call("reply_app_review_message", &a).await.unwrap()["status"],
        "delivered"
    );
}

#[tokio::test]
async fn same_thread_lock_contention_is_busy_and_sends_nothing() {
    let (d, r, st) = setup(Cfg::default()).await;
    let held = open_private_lock(d.path(), ".reply-thr1.lock").unwrap();
    fs2::FileExt::try_lock_exclusive(&held).unwrap();
    let err = r
        .call("reply_app_review_message", &args())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("reviewer_reply_busy"), "{err}");
    assert!(st.lock().unwrap().log.is_empty());
    // A different thread's lock is independent of the held one.
    let other = open_private_lock(d.path(), ".reply-thr2.lock").unwrap();
    assert!(fs2::FileExt::try_lock_exclusive(&other).is_ok());
}

#[tokio::test]
async fn message_length_is_counted_in_utf16_units_and_crlf_is_accepted() {
    let (_d, r, st) = setup(Cfg::default()).await;
    for (m, ok) in [
        ("😀".repeat(2000), true),
        ("😀".repeat(2001), false),
        ("a\u{0}b".into(), false),
        ("a\u{1b}b".into(), false),
    ] {
        let mut a = args();
        a["message"] = json!(m);
        if !ok {
            assert!(r.call("reply_app_review_message", &a).await.is_err());
        }
    }
    assert!(
        st.lock().unwrap().log.is_empty(),
        "rejected input makes no request"
    );
    let mut a = args();
    a["message"] = json!("line one\r\nline two");
    assert_eq!(
        r.call("reply_app_review_message", &a).await.unwrap()["status"],
        "delivered"
    );
}

#[tokio::test]
async fn duplicate_detection_folds_crlf_and_outer_whitespace() {
    let cfg = Cfg {
        prior_dev_body: Some("a\nb".into()),
        ..Cfg::default()
    };
    let mut a = args();
    a["message"] = json!("  a\r\nb \r\n");
    expect_rejected(cfg, a, "reviewer_reply_duplicate").await;
}

#[tokio::test]
async fn unknown_actor_fails_closed_before_any_post() {
    let cfg = Cfg {
        bad_actor: true,
        ..Cfg::default()
    };
    expect_rejected(cfg, args(), "reviewer_actor_schema_unknown").await;
}

#[cfg(unix)]
#[tokio::test]
async fn symlinked_record_is_refused_without_following_it() {
    let (d, r, st) = setup(Cfg::default()).await;
    let outside = d.path().join("outside.json");
    std::fs::write(&outside, b"{}").unwrap();
    std::os::unix::fs::symlink(&outside, reply::record_path(d.path(), "thr1")).unwrap();
    assert!(r.call("reply_app_review_message", &args()).await.is_err());
    assert!(
        r.call("get_app_review_reply_status", &status_args())
            .await
            .is_err()
    );
    assert!(posts(&st).is_empty());
    assert_eq!(std::fs::read(&outside).unwrap(), b"{}");
}

#[cfg(unix)]
#[tokio::test]
async fn reply_record_is_private_and_holds_no_body_or_secret() {
    use std::os::unix::fs::PermissionsExt as _;
    let (d, r, _st) = setup(Cfg::default()).await;
    r.call("reply_app_review_message", &args()).await.unwrap();
    let path = reply::record_path(d.path(), "thr1");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        !text.contains("Thanks, fixed") && !text.contains("secret-sentinel"),
        "{text}"
    );
}

#[tokio::test]
async fn status_confirms_a_recorded_attempt_only_from_a_matching_new_developer_message() {
    // Record says send_attempted; server shows no matching message => unknown, no POST.
    let (d, r, st) = setup(Cfg::default()).await;
    write_record(d.path(), "send_attempted", "Thanks, fixed");
    let out = r
        .call("get_app_review_reply_status", &status_args())
        .await
        .unwrap();
    assert_eq!(out["status"], "delivery_unknown", "{out}");
    assert!(posts(&st).is_empty());
}

#[tokio::test]
async fn unverifiable_attachment_metadata_never_sends_a_created_draft() {
    let cfg = Cfg {
        missing_attachment_metadata: true,
        post_visible: false,
        ..Cfg::default()
    };
    let (_d, r, st) = setup(cfg).await;
    let _ = r.call("reply_app_review_message", &args()).await;
    assert!(
        !posts(&st)
            .iter()
            .any(|p| p.path == "/iris/v1/resolutionCenterMessages"),
        "unknown attachment metadata must block sending"
    );
    assert_eq!(
        posts(&st).len(),
        1,
        "only the owned draft may have been created"
    );
}

#[tokio::test]
async fn closed_thread_with_can_reply_flag_still_makes_no_post() {
    let cfg = Cfg {
        submission_state: "COMPLETE",
        thread_state: Some("CLOSED"),
        ..Cfg::default()
    };
    expect_rejected(cfg, args(), "reviewer_reply_stale_context").await;
}

#[tokio::test]
async fn an_active_thread_can_reply_after_submission_completion() {
    let cfg = Cfg {
        submission_state: "COMPLETE",
        thread_state: Some("ACTIVE"),
        ..Cfg::default()
    };
    let (_d, r, _st) = setup(cfg).await;
    assert_eq!(
        r.call("reply_app_review_message", &args()).await.unwrap()["status"],
        "delivered"
    );
}

#[tokio::test]
async fn actual_non_apple_actor_types_confirm_delivery_despite_clock_skew() {
    for kind in ["USER", "API_KEY", "XCODE_CLOUD"] {
        let cfg = Cfg {
            developer_actor_type: kind,
            clock_offset_secs: -86400,
            ..Cfg::default()
        };
        let (_d, r, _st) = setup(cfg).await;
        assert_eq!(
            r.call("reply_app_review_message", &args()).await.unwrap()["status"],
            "delivered",
            "{kind}"
        );
    }
}

#[tokio::test]
async fn rejected_draft_is_not_sent_and_can_be_reset_without_any_post() {
    let cfg = Cfg {
        draft_post_status: 400,
        ..Cfg::default()
    };
    let (_d, r, st) = setup(cfg).await;
    assert!(r.call("reply_app_review_message", &args()).await.is_err());
    let status = r
        .call("get_app_review_reply_status", &status_args())
        .await
        .unwrap();
    assert_eq!(status["status"], "not_sent");
    assert_eq!(status["phase"], "draft_rejected");
    let before = posts(&st).len();
    let result = r
        .call("reset_rejected_app_review_reply", &status_args())
        .await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(result.unwrap()["status"], "reset");
    assert_eq!(posts(&st).len(), before, "reset must never mutate Apple");
    assert_eq!(
        r.call("get_app_review_reply_status", &status_args())
            .await
            .unwrap()["status"],
        "not_attempted"
    );
}

#[tokio::test]
async fn reset_cannot_clear_unknown_sends_or_remote_drafts() {
    for phase in [
        "intent",
        "draft_created",
        "send_attempted",
        "accepted",
        "delivered",
    ] {
        let (d, r, st) = setup(Cfg::default()).await;
        write_record(d.path(), phase, "Thanks, fixed");
        assert!(
            r.call("reset_rejected_app_review_reply", &status_args())
                .await
                .is_err()
        );
        assert!(posts(&st).is_empty());
        let saved: Value =
            serde_json::from_slice(&std::fs::read(reply::record_path(d.path(), "thr1")).unwrap())
                .unwrap();
        assert_eq!(saved["state"], phase);
    }
    let (d, r, st) = setup(Cfg {
        existing_draft: true,
        ..Cfg::default()
    })
    .await;
    write_record(d.path(), "draft_rejected", "Thanks, fixed");
    assert!(
        r.call("reset_rejected_app_review_reply", &status_args())
            .await
            .is_err()
    );
    assert!(posts(&st).is_empty());
}

fn resume_args() -> Value {
    let mut a = args();
    a["draft_id"] = json!("draft-new");
    a
}

fn pending_record_and_draft(d: &tempfile::TempDir, st: &Arc<Mutex<St>>, phase: &str) {
    write_record(d.path(), phase, "Thanks, fixed");
    let path = reply::record_path(d.path(), "thr1");
    let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["intent_at"] = json!(Utc::now().timestamp() - 2);
    if phase == "draft_created" {
        record["draft_id"] = json!("draft-new");
    }
    atomic_json(&path, &record).unwrap();
    st.lock().unwrap().draft = Some(("Thanks, fixed".into(), Utc::now().to_rfc3339()));
}

#[tokio::test]
async fn explicit_resume_sends_the_verified_existing_draft_only_once() {
    for phase in ["intent", "draft_created", "draft_pending_validation"] {
        let (d, r, st) = setup(Cfg::default()).await;
        pending_record_and_draft(&d, &st, phase);
        let status = r
            .call("get_app_review_reply_status", &status_args())
            .await
            .unwrap();
        assert_eq!(status["draft"]["draft_id"], "draft-new");
        assert_eq!(status["draft"]["resumable"], true);
        let result = r
            .call("resume_unsent_app_review_reply", &resume_args())
            .await
            .unwrap();
        assert_eq!(result["status"], "delivered");
        assert_eq!(posts(&st).len(), 1);
        assert_eq!(posts(&st)[0].path, "/iris/v1/resolutionCenterMessages");
        assert!(
            r.call("resume_unsent_app_review_reply", &resume_args())
                .await
                .is_err()
        );
        assert_eq!(posts(&st).len(), 1, "resume never sends twice");
        assert!(
            !std::fs::read_to_string(reply::record_path(d.path(), "thr1"))
                .unwrap()
                .contains("developer@example.invalid")
        );
    }
}

#[tokio::test]
async fn resume_refuses_wrong_draft_author_attachments_date_or_message() {
    for (cfg, bad) in [
        (
            Cfg {
                wrong_draft_author: true,
                ..Cfg::default()
            },
            "author",
        ),
        (
            Cfg {
                attached_draft: true,
                ..Cfg::default()
            },
            "attachment",
        ),
        (Cfg::default(), "old"),
        (Cfg::default(), "id"),
        (Cfg::default(), "body"),
    ] {
        let (d, r, st) = setup(cfg).await;
        pending_record_and_draft(&d, &st, "intent");
        assert!(
            r.tools()
                .iter()
                .any(|t| t["name"] == "resume_unsent_app_review_reply")
        );
        let mut a = resume_args();
        match bad {
            "old" => {
                st.lock().unwrap().draft = Some((
                    "Thanks, fixed".into(),
                    (Utc::now() - chrono::Duration::days(1)).to_rfc3339(),
                ))
            }
            "id" => a["draft_id"] = json!("other-draft"),
            "body" => a["message"] = json!("changed body"),
            _ => (),
        }
        let error = r
            .call("resume_unsent_app_review_reply", &a)
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.contains("@example.invalid"));
        assert!(posts(&st).is_empty(), "{bad}");
    }
}

#[tokio::test]
async fn unknown_send_outcome_is_never_resumable() {
    for phase in [
        "send_attempted",
        "accepted",
        "delivered",
        "draft_rejected",
        "reset",
    ] {
        let (d, r, st) = setup(Cfg::default()).await;
        pending_record_and_draft(&d, &st, phase);
        assert!(
            r.tools()
                .iter()
                .any(|t| t["name"] == "resume_unsent_app_review_reply")
        );
        assert!(
            r.call("resume_unsent_app_review_reply", &resume_args())
                .await
                .is_err()
        );
        assert!(st.lock().unwrap().log.is_empty(), "{phase}");
    }
}

#[tokio::test]
async fn empty_successful_create_waits_for_explicit_validated_resume() {
    let (d, r, st) = setup(Cfg {
        draft_post_status: 204,
        ..Cfg::default()
    })
    .await;
    assert!(r.call("reply_app_review_message", &args()).await.is_err());
    let record: Value =
        serde_json::from_slice(&std::fs::read(reply::record_path(d.path(), "thr1")).unwrap())
            .unwrap();
    assert_eq!(record["state"], "draft_pending_validation");
    assert_eq!(record["http_status"], 204);
    assert_eq!(record["error_kind"], "empty_response");
    assert_eq!(posts(&st).len(), 1);
    assert_eq!(
        r.call("resume_unsent_app_review_reply", &resume_args())
            .await
            .unwrap()["status"],
        "delivered"
    );
    assert_eq!(
        posts(&st).len(),
        2,
        "one create and one send, no create retry"
    );
}

#[tokio::test]
async fn empty_successful_send_is_confirmed_only_by_readback() {
    let (_d, r, st) = setup(Cfg {
        message_post_status: 204,
        ..Cfg::default()
    })
    .await;
    assert_eq!(
        r.call("reply_app_review_message", &args()).await.unwrap()["status"],
        "delivered"
    );
    assert_eq!(posts(&st).len(), 2);
}

#[tokio::test]
async fn failed_create_keeps_only_safe_numeric_diagnostics() {
    let (d, r, st) = setup(Cfg {
        draft_post_status: 503,
        ..Cfg::default()
    })
    .await;
    assert!(r.call("reply_app_review_message", &args()).await.is_err());
    let rec: Value =
        serde_json::from_slice(&std::fs::read(reply::record_path(d.path(), "thr1")).unwrap())
            .unwrap();
    assert_eq!(rec["http_status"], 503);
    assert_eq!(rec["error_kind"], "http_status");
    assert_eq!(rec["state"], "intent");
    assert_eq!(posts(&st).len(), 1);
}

/// Like `setup`, but the scripted server may reference the private session source path.
async fn setup_with(
    cfg: impl FnOnce(&Path) -> Cfg,
) -> (tempfile::TempDir, Reviewer, Arc<Mutex<St>>) {
    let dir = tempfile::tempdir().unwrap();
    let (origin, st) = serve(cfg(&dir.path().join("session.cookie")));
    let r = Reviewer::for_test(&origin, dir.path());
    (dir, r, st)
}

fn message_posts(st: &Arc<Mutex<St>>) -> usize {
    posts(st)
        .iter()
        .filter(|p| p.path == "/iris/v1/resolutionCenterMessages")
        .count()
}

fn redacted_record_and_secret_draft(d: &tempfile::TempDir, st: &Arc<Mutex<St>>) {
    pending_record_and_draft(d, st, "intent");
    let path = reply::record_path(d.path(), "thr1");
    let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["body_hash"] = json!(digest(b"[redacted]"));
    atomic_json(&path, &record).unwrap();
    // Apple holds the cookie value as the draft body; redaction would turn it into "[redacted]".
    st.lock().unwrap().draft = Some(("secret-sentinel".into(), Utc::now().to_rfc3339()));
}

#[tokio::test]
async fn resume_compares_the_raw_draft_body_not_its_redaction() {
    let (d, r, st) = setup(Cfg::default()).await;
    redacted_record_and_secret_draft(&d, &st);
    let status = r
        .call("get_app_review_reply_status", &status_args())
        .await
        .unwrap();
    assert_eq!(status["draft"]["resumable"], false, "{status}");
    assert!(!status.to_string().contains("secret-sentinel"));
    let mut a = resume_args();
    a["message"] = json!("[redacted]");
    let error = r
        .call("resume_unsent_app_review_reply", &a)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("reviewer_reply_resume_refused"), "{error}");
    assert!(!error.contains("secret-sentinel"));
    assert!(
        posts(&st).is_empty(),
        "a colliding draft must never be sent"
    );
}

#[tokio::test]
async fn draft_readback_compares_the_raw_body_before_sending() {
    let (_d, r, st) = setup(Cfg {
        draft_body_override: Some("secret-sentinel"),
        ..Cfg::default()
    })
    .await;
    let mut a = args();
    a["message"] = json!("[redacted]");
    let error = r
        .call("reply_app_review_message", &a)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("reviewer_reply_draft_conflict"), "{error}");
    assert!(!error.contains("secret-sentinel"));
    assert_eq!(posts(&st).len(), 1, "only the draft was created");
    assert_eq!(message_posts(&st), 0);
}

#[tokio::test]
async fn delivery_readback_compares_the_raw_message_body() {
    let (_d, r, st) = setup(Cfg {
        sent_body_override: Some("secret-sentinel"),
        ..Cfg::default()
    })
    .await;
    let mut a = args();
    a["message"] = json!("[redacted]");
    let error = r
        .call("reply_app_review_message", &a)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("delivery_unknown"), "{error}");
    assert!(!error.contains("secret-sentinel"));
    let status = r
        .call("get_app_review_reply_status", &status_args())
        .await
        .unwrap();
    assert_eq!(status["status"], "delivery_unknown", "{status}");
    assert!(!status.to_string().contains("secret-sentinel"));
    assert_eq!(message_posts(&st), 1, "never resent");
}

#[tokio::test]
async fn source_replaced_after_ownership_proof_sends_nothing() {
    let (d, r, st) = setup_with(|source| Cfg {
        swap_source: Some(("/olympus/v1/session", source.to_owned())),
        ..Cfg::default()
    })
    .await;
    pending_record_and_draft(&d, &st, "intent");
    let error = r
        .call("resume_unsent_app_review_reply", &resume_args())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("reviewer_session_changed"), "{error}");
    assert!(
        posts(&st).is_empty(),
        "account A's draft must not be sent as B"
    );
    let log = st.lock().unwrap().log.clone();
    assert!(st.lock().unwrap().swapped);
    assert!(
        log.iter().all(|q| !q
            .headers
            .get("cookie")
            .is_some_and(|c| c.contains("account-b-sentinel"))),
        "no request may use the replacement session within the operation"
    );
    let record: Value =
        serde_json::from_slice(&std::fs::read(reply::record_path(d.path(), "thr1")).unwrap())
            .unwrap();
    assert_ne!(record["state"], "send_attempted");
}

#[tokio::test]
async fn source_replaced_before_draft_create_sends_nothing() {
    let (d, r, st) = setup_with(|source| Cfg {
        swap_source: Some((
            "/iris/v1/resolutionCenterThreads/thr1/resolutionCenterDraftMessage",
            source.to_owned(),
        )),
        ..Cfg::default()
    })
    .await;
    let error = r
        .call("reply_app_review_message", &args())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("reviewer_session_changed"), "{error}");
    assert!(posts(&st).is_empty());
    assert!(!reply::record_path(d.path(), "thr1").exists());
}

#[tokio::test]
async fn cookie_rotation_within_the_same_source_still_sends_once() {
    let (d, r, st) = setup(Cfg {
        rotate_on_session: true,
        ..Cfg::default()
    })
    .await;
    pending_record_and_draft(&d, &st, "intent");
    let out = r
        .call("resume_unsent_app_review_reply", &resume_args())
        .await
        .unwrap();
    assert_eq!(out["status"], "delivered", "{out}");
    let sent = posts(&st);
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].headers.get("cookie").map(String::as_str),
        Some("session=rotated-sentinel")
    );
}

#[tokio::test]
async fn thread_pagination_keeps_the_submission_scope() {
    let (_d, r, _st) = setup(Cfg {
        thread_next: Some("kept"),
        ..Cfg::default()
    })
    .await;
    assert_eq!(
        r.call("reply_app_review_message", &args()).await.unwrap()["status"],
        "delivered"
    );
}

#[tokio::test]
async fn changed_pagination_scope_is_refused_before_any_post() {
    expect_rejected(
        Cfg {
            thread_next: Some("changed"),
            ..Cfg::default()
        },
        args(),
        "reviewer_unsafe_pagination_url",
    )
    .await;
}

#[tokio::test]
async fn dropped_pagination_scope_is_restored_and_foreign_thread_rejected() {
    expect_rejected(
        Cfg {
            thread_next: Some("dropped"),
            ..Cfg::default()
        },
        args(),
        "reviewer_reply_target_mismatch",
    )
    .await;
}

#[test]
fn pagination_scope_rules() {
    let first = Url::parse("https://x.invalid/p?filter%5Ba%5D=1&limit=200").unwrap();
    let ok = |next: &str| scoped_next(Url::parse(next).unwrap(), &first);
    let kept = ok("https://x.invalid/p?cursor=2&filter%5Ba%5D=1").unwrap();
    assert_eq!(
        kept.query_pairs().filter(|(k, _)| k == "filter[a]").count(),
        1
    );
    let restored = ok("https://x.invalid/p?cursor=2").unwrap();
    assert!(
        restored
            .query_pairs()
            .any(|(k, v)| k == "filter[a]" && v == "1")
    );
    for bad in [
        "https://x.invalid/p?filter%5Ba%5D=2",
        "https://x.invalid/p?filter%5Ba%5D=1&filter%5Ba%5D=2",
        "https://x.invalid/p?filter%5Ba%5D=1&filter%5Ba%5D=1",
        "https://x.invalid/p?filter%5Ba%5D=1&filter%5Bb%5D=1",
    ] {
        assert!(ok(bad).is_err(), "{bad}");
    }
}
