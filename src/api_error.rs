//! Sanitized public App Store Connect errors. Only status plus a bounded subset
//! of the documented JSON:API error fields ever leave the client; raw bodies,
//! headers, request bodies and credentials never do.
use serde_json::{Value, json};
use std::fmt;

pub const MAX_ERROR_BODY: usize = 64 * 1024;
pub const MAX_ERRORS: usize = 5;
const REDACTED: &str = "[REDACTED]";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApiErrorDetail {
    pub code: Option<String>,
    pub title: Option<String>,
    pub detail: Option<String>,
    pub source_pointer: Option<String>,
    pub source_parameter: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: u16,
    pub errors: Vec<ApiErrorDetail>,
}

impl ApiErrorDetail {
    fn is_empty(&self) -> bool {
        self.code.is_none()
            && self.title.is_none()
            && self.detail.is_none()
            && self.source_pointer.is_none()
            && self.source_parameter.is_none()
    }
    pub fn to_json(&self) -> Value {
        let mut out = serde_json::Map::new();
        for (key, value) in [
            ("code", &self.code),
            ("title", &self.title),
            ("detail", &self.detail),
        ] {
            if let Some(v) = value {
                out.insert(key.into(), json!(v));
            }
        }
        let mut source = serde_json::Map::new();
        if let Some(v) = &self.source_pointer {
            source.insert("pointer".into(), json!(v));
        }
        if let Some(v) = &self.source_parameter {
            source.insert("parameter".into(), json!(v));
        }
        if !source.is_empty() {
            out.insert("source".into(), Value::Object(source));
        }
        Value::Object(out)
    }
}
impl fmt::Display for ApiErrorDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if let Some(v) = &self.code {
            parts.push(format!("[{v}]"));
        }
        if let Some(v) = &self.title {
            parts.push(v.clone());
        }
        if let Some(v) = &self.detail {
            parts.push(format!("- {v}"));
        }
        if let Some(v) = &self.source_pointer {
            parts.push(format!("(pointer: {v})"));
        }
        if let Some(v) = &self.source_parameter {
            parts.push(format!("(parameter: {v})"));
        }
        write!(f, "{}", parts.join(" "))
    }
}
impl ApiError {
    /// Build from an already size-bounded JSON error body. Unparseable bodies
    /// yield an error with a status only.
    pub fn from_body(status: u16, body: &[u8], secrets: &[String]) -> Self {
        if body.len() > MAX_ERROR_BODY {
            return Self::status_only(status);
        }
        let errors = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|v| {
                v.get("errors").and_then(Value::as_array).map(|items| {
                    let mut errors = Vec::new();
                    let mut visited = 0;
                    for item in items {
                        if errors.len() == MAX_ERRORS {
                            break;
                        }
                        if let Some(error) = detail(item, secrets) {
                            errors.push(error);
                        }
                        if let Some(associated) = item.pointer("/meta/associatedErrors") {
                            associated_details(associated, secrets, &mut errors, 0, &mut visited);
                        }
                    }
                    errors
                })
            })
            .unwrap_or_default();
        Self { status, errors }
    }
    pub fn status_only(status: u16) -> Self {
        Self {
            status,
            errors: Vec::new(),
        }
    }
    pub fn to_json(&self) -> Value {
        json!({"status":self.status,"errors":self.errors.iter().map(ApiErrorDetail::to_json).collect::<Vec<_>>()})
    }
}
impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "App Store Connect returned HTTP {}", self.status)?;
        for (i, e) in self.errors.iter().enumerate() {
            write!(f, "{} {e}", if i == 0 { ":" } else { ";" })?;
        }
        Ok(())
    }
}
impl std::error::Error for ApiError {}

// Apple can return per-resource validation failures inside this specific meta
// field. Resource-map keys and unrelated meta never leave the sanitizer.
fn associated_details(
    value: &Value,
    secrets: &[String],
    out: &mut Vec<ApiErrorDetail>,
    depth: usize,
    visited: &mut usize,
) {
    if depth > 8 || *visited >= 256 || out.len() >= MAX_ERRORS {
        return;
    }
    *visited += 1;
    match value {
        Value::Array(values) => {
            for value in values {
                associated_details(value, secrets, out, depth + 1, visited);
            }
        }
        Value::Object(values) => {
            if let Some(error) = detail(value, secrets) {
                out.push(error);
                if let Some(nested) = value.pointer("/meta/associatedErrors") {
                    associated_details(nested, secrets, out, depth + 1, visited);
                }
            } else {
                for (key, value) in values {
                    if key != "meta" && key != "links" {
                        associated_details(value, secrets, out, depth + 1, visited);
                    }
                }
            }
        }
        _ => {}
    }
}

fn field(value: &Value, key: &str, limit: usize, secrets: &[String]) -> Option<String> {
    let s = sanitize(value.get(key)?.as_str()?, secrets, limit);
    (!s.is_empty()).then_some(s)
}
fn detail(value: &Value, secrets: &[String]) -> Option<ApiErrorDetail> {
    value.as_object()?;
    let source = value.get("source").unwrap_or(&Value::Null);
    let d = ApiErrorDetail {
        code: field(value, "code", 100, secrets),
        title: field(value, "title", 200, secrets),
        detail: field(value, "detail", 500, secrets),
        source_pointer: field(source, "pointer", 200, secrets),
        source_parameter: field(source, "parameter", 200, secrets),
    };
    (!d.is_empty()).then_some(d)
}

fn invisible(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
}
fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
}
fn replace_secrets(text: &str, secrets: &[String]) -> String {
    let mut ordered: Vec<&String> = secrets.iter().filter(|s| !s.is_empty()).collect();
    ordered.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let mut out = text.to_owned();
    for secret in ordered {
        out = out.replace(secret.as_str(), REDACTED);
    }
    out
}
/// Replace JWT-looking runs (`eyJ...`) with a marker.
fn redact_jwts(text: &str) -> String {
    fn flush(run: &mut String, out: &mut String) {
        if run.starts_with("eyJ") && run.len() >= 16 {
            out.push_str(REDACTED)
        } else {
            out.push_str(run)
        }
        run.clear();
    }
    let mut out = String::new();
    let mut run = String::new();
    for c in text.chars() {
        if token_char(c) {
            run.push(c)
        } else {
            flush(&mut run, &mut out);
            out.push(c)
        }
    }
    flush(&mut run, &mut out);
    out
}
fn sensitive_name(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "token",
        "secret",
        "cookie",
        "private",
        "authorization",
        "credential",
        "apikey",
        "api_key",
    ]
    .iter()
    .any(|k| key.contains(k))
}
/// Bounded, control-free, redacted, Unicode-safe text for public errors.
pub fn sanitize(input: &str, secrets: &[String], limit: usize) -> String {
    let text = replace_secrets(input, secrets);
    let upper = text.to_ascii_uppercase();
    if upper.contains("-----BEGIN")
        || upper.contains("PRIVATE KEY")
        || upper.contains("AUTHORIZATION:")
        || upper.contains("COOKIE:")
        || upper.contains("BEARER")
    {
        return REDACTED.into();
    }
    let text: String = text.chars().filter(|c| !invisible(*c)).collect();
    let text = redact_jwts(&replace_secrets(&text, secrets));
    let mut previous_bearer = false;
    let words: Vec<String> = text
        .split(' ')
        .map(|word| {
            let mut word = word.to_owned();
            if word.contains("://") && word.contains('@') {
                return REDACTED.to_owned();
            }
            if previous_bearer && !word.is_empty() {
                word = REDACTED.into();
            }
            previous_bearer = word.eq_ignore_ascii_case("bearer");
            if let Some((key, _)) = word.split_once('=')
                && sensitive_name(key)
            {
                word = format!("{key}={REDACTED}");
            }
            if let Some(at) = word.find('?')
                && (word.contains('/') || word.contains("://"))
                && at + 1 < word.len()
            {
                word.truncate(at);
                word.push_str("?[REDACTED]");
            }
            word
        })
        .collect();
    let text = words.join(" ");
    let text = text.trim();
    if text.chars().count() > limit {
        format!("{}…", text.chars().take(limit).collect::<String>())
    } else {
        text.to_owned()
    }
}
/// String values stored under credential-like keys in a request body. They are
/// used only locally to scrub error text and are never stored or logged.
pub fn sensitive_values(body: &Value) -> Vec<String> {
    fn walk(v: &Value, sensitive: bool, depth: usize, out: &mut Vec<String>) {
        if depth > 128 {
            return;
        }
        match v {
            Value::String(s) if sensitive && !s.is_empty() => out.push(s.clone()),
            Value::Array(a) => a.iter().for_each(|x| walk(x, sensitive, depth + 1, out)),
            Value::Object(m) => m
                .iter()
                .for_each(|(k, x)| walk(x, sensitive || sensitive_name(k), depth + 1, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(body, false, 0, &mut out);
    out
}

/// A composite tool failed after zero or more steps. Keeps the cause as a real
/// `source()` so it can be downcast.
#[derive(Debug)]
pub struct SubmissionError {
    pub submission_id: Option<String>,
    pub completed_steps: Vec<&'static str>,
    pub failed_step: &'static str,
    message: String,
    cause: anyhow::Error,
}
impl SubmissionError {
    pub fn new(
        submission_id: Option<String>,
        completed_steps: Vec<&'static str>,
        failed_step: &'static str,
        message: String,
        cause: anyhow::Error,
    ) -> Self {
        Self {
            submission_id,
            completed_steps,
            failed_step,
            message,
            cause,
        }
    }
    pub fn api_cause(&self) -> Option<&ApiError> {
        self.cause
            .chain()
            .find_map(|c| c.downcast_ref::<ApiError>())
    }
    /// True when the failed write may still have taken effect.
    pub fn outcome_unknown(&self) -> bool {
        self.api_cause().is_none_or(|a| a.status >= 500)
    }
    fn cause_text(&self) -> String {
        self.api_cause()
            .map(ToString::to_string)
            .unwrap_or_else(|| self.cause.to_string())
    }
    pub fn to_json(&self) -> Value {
        let cause = match self.api_cause() {
            Some(a) => a.to_json(),
            None => json!({"message":self.cause.to_string()}),
        };
        json!({
            "submission_id":self.submission_id,
            "completed_steps":self.completed_steps,
            "failed_step":self.failed_step,
            "message":self.message,
            "cause":cause,
            "outcome_unknown":self.outcome_unknown(),
        })
    }
}
impl fmt::Display for SubmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.message, self.cause_text())
    }
}
impl std::error::Error for SubmissionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.cause as &(dyn std::error::Error + 'static))
    }
}

/// Public text and optional structured content for a tool failure. Only the
/// sanitized `ApiError`/`SubmissionError` types are surfaced from the chain;
/// other errors keep their previous top-level message only.
pub fn public_error(e: &anyhow::Error) -> (String, Option<Value>) {
    if let Some(s) = e.downcast_ref::<SubmissionError>() {
        return (s.to_string(), Some(s.to_json()));
    }
    if let Some(api) = e.chain().find_map(|c| c.downcast_ref::<ApiError>()) {
        let top = e.to_string();
        let api_text = api.to_string();
        let text = if top == api_text {
            api_text
        } else {
            format!("{top}: {api_text}")
        };
        return (text, Some(api.to_json()));
    }
    (e.to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn associated_errors_are_extracted_without_echoing_meta_or_resource_keys() {
        let e = ApiError::from_body(409, json!({"errors":[{
            "code":"STATE_ERROR.ENTITY_STATE_INVALID","detail":"check associated errors",
            "meta":{"associatedErrors":{"private-resource-url":[
                {"code":"ENTITY_ERROR.MISSING","detail":"Support URL is required","source":{"pointer":"/data/attributes/supportUrl"}},
                {"code":"ENTITY_ERROR.CONTACT","detail":"bad fixture-password"}
            ]},"unrelated":{"detail":"private-extra-content"}}
        }]}).to_string().as_bytes(), &["fixture-password".into()]);
        let text = e.to_string();
        assert!(text.contains("Support URL is required"), "{text}");
        assert!(text.contains("ENTITY_ERROR.CONTACT"), "{text}");
        for secret in [
            "private-resource-url",
            "private-extra-content",
            "fixture-password",
        ] {
            assert!(!text.contains(secret));
        }
    }
    fn parse(body: Value, secrets: &[String]) -> ApiError {
        ApiError::from_body(409, body.to_string().as_bytes(), secrets)
    }
    #[test]
    fn keeps_only_allowed_fields_and_caps_five() {
        let items: Vec<Value> = (0..8)
            .map(|i| json!({"code":format!("C{i}"),"title":"t","detail":"d","source":{"pointer":"/data/id","parameter":"p","extra":"no"},"meta":{"x":1},"id":"zz"}))
            .collect();
        let e = parse(json!({"errors":items,"meta":"no"}), &[]);
        assert_eq!(e.errors.len(), 5);
        let rendered = e.to_json().to_string();
        assert!(rendered.contains("/data/id") && rendered.contains("\"parameter\""));
        assert!(!rendered.contains("zz") && !rendered.contains("extra"));
        assert!(
            e.to_string()
                .starts_with("App Store Connect returned HTTP 409: [C0] t - d")
        );
    }
    #[test]
    fn non_object_and_wrong_typed_entries_are_ignored() {
        let e = parse(
            json!({"errors":["x",1,{"code":5,"detail":["a"]},{"title":"ok"}]}),
            &[],
        );
        assert_eq!(e.errors.len(), 1);
        assert_eq!(e.errors[0].title.as_deref(), Some("ok"));
        for body in [
            &b""[..],
            b"<html>secret</html>",
            b"[]",
            b"{\"errors\":{}}",
            b"\xff\xfe",
        ] {
            assert!(ApiError::from_body(500, body, &[]).errors.is_empty());
        }
        assert_eq!(
            ApiError::status_only(500).to_string(),
            "App Store Connect returned HTTP 500"
        );
    }
    #[test]
    fn strips_controls_and_bounds_unicode_safely() {
        let long = "界".repeat(900);
        let e = parse(
            json!({"errors":[{"detail":format!("a\u{0}b\nc\u{202E}d{long}"),"code":"x\u{7}y"}]}),
            &[],
        );
        let d = e.errors[0].detail.as_deref().unwrap();
        assert!(d.starts_with("abcd界") && d.ends_with('…'));
        assert_eq!(d.chars().count(), 501);
        assert!(!d.chars().any(|c| c.is_control()));
        assert_eq!(e.errors[0].code.as_deref(), Some("xy"));
    }
    #[test]
    fn redacts_in_every_field() {
        let jwt = "eyJhbGciOiJFUzI1NiJ9.eyJpc3MiOiJ4In0.c2ln-_";
        let secrets = vec!["hunter2-value".to_owned()];
        let e = parse(
            json!({"errors":[{
                "code":format!("bad {jwt}"),
                "title":"Authorization: Bearer abc.def",
                "detail":"password hunter2-value at https://x.test/v1/a?token=1&b=2 failed",
                "source":{"pointer":"/hunter2-value","parameter":"-----BEGIN EC PRIVATE KEY-----abc"}
            }]}),
            &secrets,
        );
        let all = e.to_json().to_string();
        for leaked in [jwt, "hunter2-value", "abc.def", "token=1", "BEGIN"] {
            assert!(!all.contains(leaked), "{leaked} leaked in {all}");
        }
        assert!(all.contains("https://x.test/v1/a?[REDACTED]"));
        assert!(all.contains("failed"));
    }
    #[test]
    fn collects_sensitive_body_values_only() {
        let v = sensitive_values(
            &json!({"data":{"attributes":{"password":"p4ss","name":"keep","nested":{"clientSecret":["s3"]},"x":{"pushToken":"tk"}}}}),
        );
        assert_eq!(v.len(), 3);
        for s in ["p4ss", "s3", "tk"] {
            assert!(v.iter().any(|x| x == s));
        }
        assert!(!v.iter().any(|x| x == "keep"));
    }
    #[test]
    fn public_error_surfaces_only_safe_chain_members() {
        let api = anyhow::Error::new(ApiError::from_body(
            409,
            br#"{"errors":[{"code":"STATE","detail":"nope"}]}"#,
            &[],
        ));
        let wrapped = api.context("upload failed");
        let (text, structured) = public_error(&wrapped);
        assert!(text.starts_with("upload failed: App Store Connect returned HTTP 409"));
        assert_eq!(structured.unwrap()["errors"][0]["code"], "STATE");
        let plain = anyhow::anyhow!("inner secret").context("outer");
        assert_eq!(public_error(&plain), ("outer".to_owned(), None));
    }
    #[test]
    fn submission_error_keeps_source_and_downcasts() {
        let cause = anyhow::Error::new(ApiError::status_only(503));
        let e = anyhow::Error::new(SubmissionError::new(
            Some("s1".into()),
            vec!["create_review_submission"],
            "attach_version",
            "review submission s1 created; failed to attach version".into(),
            cause,
        ));
        let s = e.downcast_ref::<SubmissionError>().unwrap();
        assert!(s.outcome_unknown());
        assert!(
            std::error::Error::source(s)
                .unwrap()
                .downcast_ref::<ApiError>()
                .is_some()
        );
        let j = s.to_json();
        assert_eq!(j["submission_id"], "s1");
        assert_eq!(j["cause"]["status"], 503);
    }
}
