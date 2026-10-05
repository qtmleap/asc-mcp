//! Apple ID session-backed, undocumented App Review endpoints. No subprocesses.
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use reqwest::{Client, Method, header::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use url::Url;

#[cfg(test)]
mod refresh_tests;
mod session;
mod status;

const MAX_BODY: usize = 8 * 1024 * 1024;
const MAX_SESSION: usize = 1024 * 1024;
const ORIGIN: &str = "https://appstoreconnect.apple.com";

pub struct Reviewer {
    client: Client,
    cookie_path: PathBuf,
    root: PathBuf,
    origin: Url,
    app_id: String,
    legacy_reply: bool,
    limits: status::Limits,
}

#[derive(Clone, Serialize, Deserialize)]
struct Cookie {
    name: String,
    value: String,
    domain: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    expires: Option<String>,
}

fn private_root() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .context("reviewer_home_required")?,
    )
    .join("Library/Application Support/InterceptorReview"))
}

fn private_read(path: &Path, max: usize) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path).map_err(|_| {
        anyhow::anyhow!("reviewer_session_required: import a valid Apple ID session")
    })?;
    if !meta.is_file() {
        bail!("reviewer_private_file_required");
    }
    #[cfg(unix)]
    if meta.permissions().mode() & 0o077 != 0 {
        bail!("reviewer_session_permissions: file must have mode 600");
    }
    if meta.len() > max as u64 {
        bail!("reviewer_private_file_too_large");
    }
    let mut bytes = Vec::new();
    let file =
        fs::File::open(path).map_err(|_| anyhow::anyhow!("reviewer_private_file_unreadable"))?;
    #[cfg(unix)]
    {
        let opened = file.metadata()?;
        if opened.ino() != meta.ino()
            || opened.dev() != meta.dev()
            || opened.permissions().mode() & 0o077 != 0
        {
            bail!("reviewer_private_file_changed");
        }
    }
    file.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("reviewer_private_file_unreadable"))?;
    if bytes.len() > max {
        bail!("reviewer_private_file_too_large");
    }
    Ok(bytes)
}

fn ensure_root(root: &Path) -> Result<()> {
    if !root.exists() {
        fs::create_dir_all(root).map_err(|_| anyhow::anyhow!("reviewer_state_unwritable"))?;
    }
    if !fs::symlink_metadata(root)?.is_dir() {
        bail!("reviewer_private_directory_required");
    }
    #[cfg(unix)]
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn atomic_json(path: &Path, value: &Value) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec_pretty(value)?;
    if bytes.len() > MAX_BODY {
        bail!("reviewer_private_file_too_large");
    }
    let parent = path.parent().context("reviewer_state_path_invalid")?;
    ensure_root(parent)?;
    let mut attempt = 0;
    let (temporary, mut file) = loop {
        let tmp = parent.join(format!(
            ".reviewer-{}-{}-{}.tmp",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default(),
            attempt
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&tmp) {
            Ok(f) => break (tmp, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 10 => attempt += 1,
            Err(_) => bail!("reviewer_state_unwritable"),
        }
    };
    let result = (|| -> Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        bail!("reviewer_state_unwritable");
    }
    Ok(bytes)
}

// serde_yaml treats Ruby object tags as data; never instantiate tagged objects.
fn untag(value: serde_yaml::Value) -> serde_yaml::Value {
    use serde_yaml::Value as Y;
    match value {
        Y::Tagged(t) => untag(t.value),
        Y::Sequence(v) => Y::Sequence(v.into_iter().map(untag).collect()),
        Y::Mapping(v) => Y::Mapping(v.into_iter().map(|(k, v)| (untag(k), untag(v))).collect()),
        v => v,
    }
}
fn parse_cookies(bytes: &[u8]) -> Result<Vec<Cookie>> {
    let parsed: serde_yaml::Value =
        serde_yaml::from_slice(bytes).map_err(|_| anyhow::anyhow!("reviewer_session_invalid"))?;
    let mut data: Value = serde_yaml::from_value(untag(parsed))
        .map_err(|_| anyhow::anyhow!("reviewer_session_invalid"))?;
    if data.is_object() {
        data = data
            .get("cookies")
            .cloned()
            .context("reviewer_session_invalid")?;
    }
    serde_json::from_value(data).map_err(|_| anyhow::anyhow!("reviewer_session_invalid"))
}
fn valid_expiry(expires: &Option<String>) -> bool {
    let Some(s) = expires.as_ref().filter(|v| !v.is_empty()) else {
        return true;
    };
    DateTime::parse_from_rfc3339(s)
        .or_else(|_| DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f %z"))
        .or_else(|_| DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f %:z"))
        .or_else(|_| {
            DateTime::parse_from_str(&s.replace(" Z", " +0000"), "%Y-%m-%d %H:%M:%S%.f %z")
        })
        .map(|date| date.with_timezone(&Utc) > Utc::now())
        .unwrap_or(false)
}
fn cookie_header(cookies: &[Cookie]) -> Result<String> {
    let entries: Vec<_> = cookies
        .iter()
        .filter(|c| {
            matches!(
                c.domain.trim_start_matches('.'),
                "apple.com" | "appstoreconnect.apple.com"
            ) && (c.path.is_empty() || c.path == "/")
                && valid_expiry(&c.expires)
                && !c.name.is_empty()
                && c.name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
                && !c.value.is_empty()
                && c.value
                    .bytes()
                    .all(|b| (0x21..=0x7e).contains(&b) && !b"\";,\\".contains(&b))
        })
        .map(|c| format!("{}={}", c.name, c.value))
        .collect();
    if entries.is_empty() {
        bail!("reviewer_session_required: no unexpired App Store Connect cookies");
    }
    Ok(entries.join("; "))
}

/// Convert an existing session into owner-only JSON. Does not log in or validate access.
pub fn import_session(source: &Path) -> Result<PathBuf> {
    import_into(source, &private_root()?)
}
fn import_into(source: &Path, root: &Path) -> Result<PathBuf> {
    let cookies = parse_cookies(&private_read(source, MAX_SESSION)?)?;
    cookie_header(&cookies)?;
    let path = root.join("session.cookie");
    atomic_json(&path, &json!({"cookies":cookies}))?;
    Ok(path)
}

/// Save a native login's web cookies without persisting the account identifier.
pub fn save_login_session(_email: &str, cookies: Value) -> Result<PathBuf> {
    let bytes =
        serde_json::to_vec(&cookies).map_err(|_| anyhow::anyhow!("reviewer_session_invalid"))?;
    if bytes.len() > MAX_SESSION {
        bail!("reviewer_private_file_too_large");
    }
    let cookies = parse_cookies(&bytes)?;
    cookie_header(&cookies)?;
    let path = private_root()?.join("session.cookie");
    atomic_json(&path, &json!({"cookies":cookies}))?;
    Ok(path)
}

fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn arg<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .with_context(|| format!("{name} is required"))
}
fn redact(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, v) in map {
                if matches!(
                    key.to_ascii_lowercase().as_str(),
                    "cookie"
                        | "cookies"
                        | "set-cookie"
                        | "password"
                        | "authorization"
                        | "csrf"
                        | "csrf_ts"
                ) {
                    *v = json!("[redacted]");
                } else {
                    redact(v);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact),
        _ => (),
    }
}
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn redact_cookie_values(value: &mut Value, cookies: &[Cookie]) {
    match value {
        Value::Object(map) => map
            .values_mut()
            .for_each(|v| redact_cookie_values(v, cookies)),
        Value::Array(values) => values
            .iter_mut()
            .for_each(|v| redact_cookie_values(v, cookies)),
        Value::String(text) => {
            for cookie in cookies.iter().filter(|c| !c.value.is_empty()) {
                if *text == cookie.value {
                    *text = "[redacted]".into();
                } else if cookie.value.len() >= 8 {
                    *text = text.replace(&cookie.value, "[redacted]");
                }
            }
        }
        _ => (),
    }
}

struct MonitorLock(fs::File);
impl MonitorLock {
    fn acquire(root: &Path) -> Result<Self> {
        ensure_root(root)?;
        let path = root.join(".monitor.lock");
        if path.exists() && !fs::symlink_metadata(&path)?.is_file() {
            bail!("reviewer_private_file_required")
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options
            .open(path)
            .map_err(|_| anyhow::anyhow!("reviewer_state_unwritable"))?;
        fs2::FileExt::try_lock_exclusive(&file).map_err(|_| {
            anyhow::anyhow!("reviewer_monitor_busy: another check or acknowledgement is active")
        })?;
        Ok(Self(file))
    }
}
impl Drop for MonitorLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

impl Reviewer {
    pub fn from_env() -> Result<Option<Self>> {
        let root = private_root()?;
        let cookie_path = std::env::var_os("ASC_REVIEW_COOKIE_PATH")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("session.cookie"));
        let enabled = cookie_path.exists()
            || std::env::var_os("ASC_REVIEW_COOKIE_PATH").is_some()
            || std::env::var_os("ASC_REVIEW_APP_ID").is_some()
            || std::env::var_os("ASC_REVIEWER_ONLY").is_some();
        if !enabled {
            return Ok(None);
        }
        Ok(Some(Self::new_default()?))
    }

    /// Explicit CLI checks construct a client even when authentication is missing.
    pub fn new_default() -> Result<Self> {
        let root = private_root()?;
        let cookie_path = std::env::var_os("ASC_REVIEW_COOKIE_PATH")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("session.cookie"));
        let app_id = std::env::var("ASC_REVIEW_APP_ID").unwrap_or_else(|_| "6749347474".into());
        if app_id.is_empty() || !app_id.bytes().all(|b| b.is_ascii_digit()) {
            bail!("invalid_app_id");
        }
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()?,
            cookie_path,
            root,
            origin: Url::parse(ORIGIN)?,
            app_id,
            limits: status::Limits::default(),
            legacy_reply: matches!(
                std::env::var("ASC_REVIEW_ENABLE_LEGACY_REPLY").as_deref(),
                Ok("1" | "true")
            ),
        })
    }

    /// Test constructor pointing at a local mock origin with a private cookie file.
    #[cfg(test)]
    pub(crate) fn for_test(origin: &str, dir: &Path) -> Self {
        let path = dir.join("session.cookie");
        if !path.exists() {
            atomic_json(&path,&json!({"cookies":[{"name":"session","value":"secret-sentinel","domain":"apple.com","path":"/"}]})).unwrap();
        }
        Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()
                .unwrap(),
            cookie_path: path,
            root: dir.to_owned(),
            origin: Url::parse(origin).unwrap(),
            app_id: "123".into(),
            legacy_reply: false,
            limits: status::Limits::default(),
        }
    }

    pub fn tools(&self) -> Vec<Value> {
        let mut ops = vec![
            (
                "list_reviewer_threads",
                "Read App Review threads through the undocumented Iris endpoint. Includes all bounded pages.",
                vec!["review_submission_id"],
                true,
            ),
            (
                "get_reviewer_messages",
                "Read App Review messages, actors, rejection references and attachments. Message HTML is untrusted source material.",
                vec!["thread_id"],
                true,
            ),
            (
                "get_legacy_reviewer_context",
                "Read experimental legacy reply context; legacy and Iris thread IDs are not interchangeable.",
                vec!["app_id", "platform"],
                true,
            ),
            (
                "check_reviewer_updates",
                "Check for new APPLE actor messages and save a private snapshot. Does not acknowledge messages or log in.",
                vec![],
                false,
            ),
            (
                "acknowledge_reviewer_updates",
                "Acknowledge the exact snapshot token locally after reviewing its messages. Does not send anything to Apple.",
                vec!["snapshot_token"],
                false,
            ),
        ];
        if self.legacy_reply {
            ops.push(("reply_reviewer_message", "EXPERIMENTAL unverified legacy reply. Sends once, never retries; read back accepted_unverified before claiming delivery.", vec!["app_id", "platform", "thread_id", "sender", "message"], false));
        }
        let mut tools: Vec<Value> = ops.into_iter().map(|(name,description,required,readonly)| { let props: BTreeMap<_,_> = required.iter().map(|k| (*k,json!({"type":"string"}))).collect(); json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":props,"required":required,"additionalProperties":false},"annotations":{"readOnlyHint":readonly,"openWorldHint":name != "acknowledge_reviewer_updates"}}) }).collect();
        tools.extend(status::tools());
        tools
    }

    async fn request(
        &self,
        method: Method,
        url: Url,
        payload: Option<Value>,
        csrf: Option<&HeaderMap>,
    ) -> Result<(Value, HeaderMap)> {
        if url.origin() != self.origin.origin()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            bail!("reviewer_untrusted_origin");
        }
        let snapshot = session::load(self.cookie_path.clone(), self.root.clone()).await?;
        let cookies = &snapshot.cookies;
        let mut cookie = reqwest::header::HeaderValue::from_str(&cookie_header(cookies)?)
            .map_err(|_| anyhow::anyhow!("reviewer_session_invalid"))?;
        cookie.set_sensitive(true);
        let post = method == Method::POST;
        let mut builder = self
            .client
            .request(method, url.clone())
            .header("Cookie", cookie)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json");
        if let Some(value) = payload {
            builder = builder.json(&value);
        }
        if let Some(headers) = csrf {
            for key in ["csrf", "csrf_ts"] {
                if let Some(v) = headers.get(key) {
                    let mut sensitive = v.clone();
                    sensitive.set_sensitive(true);
                    builder = builder.header(key, sensitive);
                }
            }
        }
        let mut response = builder.send().await.map_err(|_| {
            anyhow::anyhow!(if post {
                "delivery_unknown: request interrupted; read the thread before retrying"
            } else {
                "reviewer_request_failed: HTTP connection failed"
            })
        })?;
        let updates = session::updates(response.headers(), &url, cookies);
        let redaction_cookies: Vec<Cookie> = cookies
            .iter()
            .cloned()
            .chain(updates.iter().filter_map(|u| u.cookie().cloned()))
            .collect();
        if !updates.is_empty()
            && session::persist(
                self.cookie_path.clone(),
                self.root.clone(),
                snapshot.generation,
                updates,
            )
            .await
            .is_err()
        {
            eprintln!("reviewer_session_refresh_unwritable: updated cookies could not be saved");
        }
        let status = response.status();
        if status.as_u16() == 401 {
            bail!("reviewer_auth_required: HTTP 401 unauthenticated");
        }
        if status.as_u16() == 403 {
            bail!("reviewer_forbidden: HTTP 403 access denied; session expiry is not established");
        }
        if status.is_redirection() {
            let login = response
                .headers()
                .get("location")
                .and_then(|h| h.to_str().ok())
                .and_then(|s| url.join(s).ok())
                .is_some_and(|target| {
                    target.username().is_empty()
                        && target.password().is_none()
                        && ((target.scheme() == "https"
                            && matches!(
                                target.host_str(),
                                Some("idmsa.apple.com" | "appleid.apple.com")
                            ))
                            || (target.origin() == self.origin.origin()
                                && (target.path() == "/login"
                                    || target.path().starts_with("/login/"))))
                });
            if login {
                bail!("reviewer_auth_required: login_redirect");
            }
            bail!("reviewer_unexpected_redirect: redirect not followed");
        }
        if !status.is_success() {
            if post && status.is_server_error() {
                bail!(
                    "delivery_unknown: Apple returned HTTP {}; read the thread before retrying",
                    status.as_u16()
                );
            }
            bail!("reviewer_api_unavailable: HTTP {}", status.as_u16());
        }
        let mut headers = response.headers().clone();
        headers.remove(reqwest::header::SET_COOKIE);
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| {
            anyhow::anyhow!(if post {
                "delivery_unknown: interrupted response"
            } else {
                "reviewer_response_invalid"
            })
        })? {
            if chunk.len() > MAX_BODY - bytes.len() {
                bail!(
                    "{}",
                    if post {
                        "delivery_unknown: oversized response"
                    } else {
                        "reviewer_response_invalid: oversized response"
                    }
                );
            }
            bytes.extend_from_slice(&chunk);
        }
        let mut data = serde_json::from_slice(&bytes).map_err(|_| {
            anyhow::anyhow!(if post {
                "delivery_unknown: invalid JSON response"
            } else {
                "reviewer_response_invalid: expected JSON"
            })
        })?;
        redact(&mut data);
        redact_cookie_values(&mut data, &redaction_cookies);
        Ok((data, headers))
    }
    fn url(&self, path: &str, query: &[(&str, &str)]) -> Result<Url> {
        let mut url = self.origin.join(path)?;
        url.query_pairs_mut().extend_pairs(query.iter().copied());
        Ok(url)
    }
    async fn pages(&self, path: &str, query: &[(&str, &str)]) -> Result<Value> {
        let mut url = self.url(path, query)?;
        let mut visited = HashSet::new();
        let mut data = Vec::new();
        let mut included = Vec::new();
        for _ in 0..20 {
            if !visited.insert(url.as_str().to_owned()) {
                bail!("reviewer_pagination_cycle");
            }
            let (response, _) = self.request(Method::GET, url.clone(), None, None).await?;
            data.extend(
                response
                    .get("data")
                    .and_then(Value::as_array)
                    .context("reviewer_api_error_response")?
                    .iter()
                    .cloned(),
            );
            if let Some(items) = response.get("included") {
                included.extend(
                    items
                        .as_array()
                        .context("reviewer_api_error_response")?
                        .iter()
                        .cloned(),
                );
            }
            if serde_json::to_vec(&data)?.len() + serde_json::to_vec(&included)?.len() > MAX_BODY {
                bail!("reviewer_response_invalid: aggregate too large");
            }
            match response.pointer("/links/next") {
                None | Some(Value::Null) => return Ok(json!({"data":data,"included":included})),
                Some(Value::String(next)) if next.is_empty() => {
                    return Ok(json!({"data":data,"included":included}));
                }
                Some(Value::String(next)) => {
                    let candidate = Url::parse(next)
                        .map_err(|_| anyhow::anyhow!("reviewer_unsafe_pagination_url"))?;
                    if candidate.origin() != self.origin.origin()
                        || candidate.path() != path
                        || !candidate.username().is_empty()
                        || candidate.password().is_some()
                        || candidate.fragment().is_some()
                    {
                        bail!("reviewer_unsafe_pagination_url");
                    }
                    url = candidate;
                }
                _ => bail!("reviewer_unsafe_pagination_url"),
            }
        }
        bail!("reviewer_pagination_incomplete")
    }
    fn legacy_path(app: &str, platform: &str) -> Result<String> {
        if !identifier(app) || !matches!(platform, "ios" | "osx" | "appletvos" | "xros") {
            bail!("invalid app_id or platform");
        }
        Ok(format!(
            "/WebObjects/iTunesConnect.woa/ra/apps/{app}/platforms/{platform}/resolutionCenter"
        ))
    }
    pub async fn call(&self, name: &str, args: &Value) -> Result<Value> {
        self.call_with_api(name, args, None).await
    }
    /// `api_configured` is the server's actual public-API client state, if known.
    pub async fn call_with_api(
        &self,
        name: &str,
        args: &Value,
        api_configured: Option<bool>,
    ) -> Result<Value> {
        let tool = self
            .tools()
            .into_iter()
            .find(|t| t["name"] == name)
            .context("unknown reviewer tool")?;
        let object = args.as_object().context("invalid arguments")?;
        let required = tool["inputSchema"]["required"].as_array().unwrap();
        if object.len() != required.len() {
            bail!("invalid arguments");
        }
        for key in required {
            arg(args, key.as_str().unwrap())?;
        }
        match name {
            n if status::NAMES.contains(&n) => self.status_call(n, args, api_configured).await,
            "check_reviewer_updates" => self.check_updates().await,
            "acknowledge_reviewer_updates" => self.acknowledge(arg(args, "snapshot_token")?),
            "list_reviewer_threads" => {
                let id = arg(args, "review_submission_id")?;
                if !identifier(id) {
                    bail!("invalid review_submission_id");
                }
                self.pages(
                    "/iris/v1/resolutionCenterThreads",
                    &[("filter[reviewSubmission]", id), ("limit", "200")],
                )
                .await
            }
            "get_reviewer_messages" => {
                let id = arg(args, "thread_id")?;
                if !identifier(id) {
                    bail!("invalid thread_id");
                }
                self.pages(
                    &format!("/iris/v1/resolutionCenterThreads/{id}/resolutionCenterMessages"),
                    &[
                        (
                            "include",
                            "fromActor,rejections,resolutionCenterMessageAttachments",
                        ),
                        ("limit", "200"),
                    ],
                )
                .await
            }
            "get_legacy_reviewer_context" => {
                let path = Self::legacy_path(arg(args, "app_id")?, arg(args, "platform")?)?;
                let value = self
                    .request(
                        Method::GET,
                        self.url(&path, &[("v", "latest")])?,
                        None,
                        None,
                    )
                    .await?
                    .0;
                if value["statusCode"] != "SUCCESS" {
                    bail!("reviewer_legacy_api_error: Apple did not return SUCCESS")
                }
                Ok(value)
            }
            "reply_reviewer_message" if self.legacy_reply => self.reply(args).await,
            _ => bail!("unknown reviewer tool"),
        }
    }
    async fn reply(&self, args: &Value) -> Result<Value> {
        let body = arg(args, "message")?;
        let sender = arg(args, "sender")?;
        let thread_id = arg(args, "thread_id")?;
        if body.trim().is_empty()
            || body.encode_utf16().count() > 4000
            || sender.trim().is_empty()
            || !identifier(thread_id)
        {
            bail!("invalid reply arguments");
        }
        let path = Self::legacy_path(arg(args, "app_id")?, arg(args, "platform")?)?;
        let (current, headers) = self
            .request(
                Method::GET,
                self.url(&path, &[("v", "latest")])?,
                None,
                None,
            )
            .await?;
        if current["statusCode"] != "SUCCESS" {
            bail!("reviewer_reply_preflight_failed");
        }
        let thread = current
            .pointer("/data/appNotes/threads")
            .and_then(Value::as_array)
            .context("reviewer_reply_preflight_failed")?
            .iter()
            .find(|t| t["id"] == thread_id)
            .context("reviewer_thread_not_found")?;
        if thread["active"] != true
            || thread["canDeveloperAddNote"] != true
            || thread["versionId"].as_str().unwrap_or("").is_empty()
            || thread["version"].as_str().unwrap_or("").is_empty()
        {
            bail!("reviewer_thread_not_replyable");
        }
        if thread["messages"]
            .as_array()
            .map(|v| v.iter().any(|m| m["body"] == body))
            .unwrap_or(false)
        {
            bail!("reviewer_duplicate_reply: identical body already present; not sent");
        }
        if ["csrf", "csrf_ts"]
            .iter()
            .any(|k| headers.get(*k).map(|v| v.is_empty()).unwrap_or(true))
        {
            bail!("reviewer_csrf_required: not sent");
        }
        let payload = json!({"appNotes":{"threads":[{"id":thread_id,"versionId":thread["versionId"],"version":thread["version"],"messages":[{"from":sender,"date":Utc::now().timestamp_millis().to_string(),"body":body,"tokens":[]}]}]}});
        let (result, _) = self
            .request(
                Method::POST,
                self.url(&path, &[])?,
                Some(payload),
                Some(&headers),
            )
            .await?;
        if result["statusCode"] != "SUCCESS" {
            bail!(
                "delivery_unknown: Apple did not return SUCCESS; read the thread before retrying"
            );
        }
        Ok(
            json!({"status":"accepted_unverified","message":"Apple returned SUCCESS; read reviewer messages to confirm delivery. Do not automatically resend."}),
        )
    }
    fn state(&self) -> Result<Value> {
        let path = self.root.join("state.json");
        if !path.exists() {
            return Ok(json!({"seen":{}}));
        }
        let state: Value = serde_json::from_slice(&private_read(&path, MAX_BODY)?)
            .map_err(|_| anyhow::anyhow!("reviewer_state_invalid"))?;
        if !state["seen"].is_object() {
            bail!("reviewer_state_invalid");
        }
        Ok(state)
    }
    pub async fn check_updates(&self) -> Result<Value> {
        tokio::time::timeout(Duration::from_secs(55), self.check_inner())
            .await
            .map_err(|_| anyhow::anyhow!("reviewer_check_timeout"))?
    }
    async fn check_inner(&self) -> Result<Value> {
        let _lock = MonitorLock::acquire(&self.root)?;
        let state = self.state()?;
        let submissions = self
            .pages(
                &format!("/iris/v1/apps/{}/reviewSubmissions", self.app_id),
                &[("limit", "200")],
            )
            .await?;
        let mut messages = Vec::new();
        for submission in submissions["data"].as_array().unwrap() {
            let sid = submission["id"]
                .as_str()
                .filter(|s| identifier(s))
                .context("reviewer_api_error_response")?;
            let threads = self
                .pages(
                    "/iris/v1/resolutionCenterThreads",
                    &[("filter[reviewSubmission]", sid), ("limit", "200")],
                )
                .await?;
            for thread in threads["data"].as_array().unwrap() {
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
                for message in response["data"].as_array().unwrap() {
                    let id = message["id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .context("reviewer_api_error_response")?;
                    let actor_id = message.pointer("/relationships/fromActor/data/id");
                    let actor = response["included"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|a| {
                            a["type"] == "actors"
                                && actor_id.is_some()
                                && Some(&a["id"]) == actor_id
                        })
                        .map(|a| &a["attributes"])
                        .unwrap_or(&Value::Null);
                    let actor_type = actor["actorType"].as_str().unwrap_or("");
                    let known_user = actor["userEmail"].is_string()
                        || actor["userFirstName"].is_string()
                        || actor["apiKeyId"].is_string();
                    if actor_type != "APPLE" && actor_type != "DEVELOPER" && !known_user {
                        bail!(
                            "reviewer_actor_schema_unknown: cannot determine whether a message is from Apple"
                        )
                    }
                    messages.push(json!({"id":id,"thread_id":tid,"submission_id":sid,"created_date":message["attributes"]["createdDate"],"actor_type":actor["actorType"],"actor_name":actor["name"],"body_html":message["attributes"]["messageBody"],"rejection_references":message.pointer("/relationships/rejections/data")}));
                    if serde_json::to_vec(&messages)?.len() > MAX_BODY {
                        bail!("reviewer_response_invalid: aggregate too large");
                    }
                }
            }
        }
        let changes: Vec<Value> = messages
            .iter()
            .filter_map(|m| {
                if m["actor_type"] != "APPLE" {
                    return None;
                }
                let fingerprint = digest(&serde_json::to_vec(m).ok()?);
                if state["seen"][m["id"].as_str()?] == fingerprint {
                    return None;
                }
                let mut pending = m.clone();
                pending["fingerprint"] = json!(fingerprint);
                Some(pending)
            })
            .collect();
        let path = self.root.join("messages.json");
        let bytes = atomic_json(
            &path,
            &json!({"app_id":self.app_id,"messages":messages,"checked_at":Utc::now().to_rfc3339()}),
        )?;
        Ok(
            json!({"status":if changes.is_empty() {"unchanged"} else {"new_messages"},"app_id":self.app_id,"snapshot_path":path,"snapshot_token":digest(&bytes),"new_messages":changes}),
        )
    }
    pub fn acknowledge(&self, token: &str) -> Result<Value> {
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("valid snapshot_token required");
        }
        let _lock = MonitorLock::acquire(&self.root)?;
        if !self.root.join("messages.json").exists() {
            bail!("no_snapshot_to_acknowledge");
        }
        let bytes = private_read(&self.root.join("messages.json"), MAX_BODY)?;
        if digest(&bytes) != token {
            bail!("snapshot_changed_or_missing_token");
        }
        let snapshot: Value = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("reviewer_snapshot_invalid"))?;
        if snapshot["app_id"] != self.app_id {
            bail!("reviewer_snapshot_app_mismatch");
        }
        let mut state = self.state()?;
        for m in snapshot["messages"]
            .as_array()
            .context("reviewer_snapshot_invalid")?
        {
            if m["actor_type"] == "APPLE" {
                let id = m["id"].as_str().context("reviewer_snapshot_invalid")?;
                state["seen"][id] = json!(digest(&serde_json::to_vec(m)?));
            }
        }
        // Reject a check that replaced the snapshot during acknowledgement.
        if digest(&private_read(&self.root.join("messages.json"), MAX_BODY)?) != token {
            bail!("snapshot_changed_or_missing_token");
        }
        atomic_json(&self.root.join("state.json"), &state)?;
        Ok(json!({"status":"acknowledged"}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::{Method::GET, Method::POST, MockServer};
    #[test]
    fn imports_yaml_to_private_canonical_json() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        options.open(&source).unwrap().write_all(b"- !ruby/object:HTTP::Cookie\n  name: session\n  value: secret-sentinel\n  domain: apple.com\n  path: /\n").unwrap();
        let imported = import_into(&source, &dir.path().join("private")).unwrap();
        let bytes = private_read(&imported, MAX_SESSION).unwrap();
        let canonical: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(canonical["cookies"][0]["name"], "session");
        assert_eq!(
            cookie_header(&parse_cookies(&bytes).unwrap()).unwrap(),
            "session=secret-sentinel"
        );
        assert!(valid_expiry(&Some("2035-01-01 00:00:00 Z".into())));
    }
    fn fixture(server: &MockServer) -> (tempfile::TempDir, Reviewer) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.cookie");
        atomic_json(&path,&json!({"cookies":[{"name":"session","value":"secret-sentinel","domain":"apple.com","path":"/"}]})).unwrap();
        let reviewer = Reviewer {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()
                .unwrap(),
            cookie_path: path,
            root: dir.path().to_owned(),
            origin: Url::parse(&server.base_url()).unwrap(),
            app_id: "123".into(),
            legacy_reply: false,
            limits: status::Limits::default(),
        };
        (dir, reviewer)
    }
    #[test]
    fn safely_parses_tagged_yaml_and_filters_expiry() {
        let bytes = b"---\n- !ruby/object:HTTP::Cookie\n  name: session\n  value: safe\n  domain: .apple.com\n  path: /\n  expires: \n";
        assert_eq!(
            cookie_header(&parse_cookies(bytes).unwrap()).unwrap(),
            "session=safe"
        );
        for value in [
            json!([{"name":"s","value":"secret","domain":"evil.apple.com"}]),
            json!([{"name":"s","value":"secret; injected=yes","domain":"apple.com"}]),
            json!([{"name":"s","value":"secret","domain":"apple.com","expires":"2000-01-01T00:00:00Z"}]),
            json!([{"name":"s","value":"secret","domain":"apple.com","expires":"garbage"}]),
        ] {
            assert!(
                cookie_header(&parse_cookies(&serde_json::to_vec(&value).unwrap()).unwrap())
                    .is_err()
            );
        }
    }
    #[test]
    #[cfg(unix)]
    fn rejects_insecure_and_symlink_session_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cookie");
        fs::write(&file, "secret-sentinel").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        let error = private_read(&file, MAX_SESSION).unwrap_err().to_string();
        assert!(error.contains("permissions"));
        assert!(!error.contains("secret-sentinel"));
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(private_read(&link, MAX_SESSION).is_err());
    }
    #[tokio::test]
    async fn rejects_redirect_auth_and_invalid_json_without_leaking_body() {
        for (status, body) in [
            (302, "secret-sentinel"),
            (401, "secret-sentinel"),
            (403, "secret-sentinel"),
            (500, "secret-sentinel"),
            (200, "secret-sentinel"),
        ] {
            let server = MockServer::start();
            let (_dir, r) = fixture(&server);
            let mock = server.mock(|when, then| {
                when.method(GET);
                then.status(status)
                    .header("Location", "https://evil.example")
                    .body(body);
            });
            let error = r
                .pages("/iris/v1/threads", &[])
                .await
                .unwrap_err()
                .to_string();
            assert!(!error.contains("secret-sentinel"));
            mock.assert_calls(1);
        }
    }
    #[tokio::test]
    async fn bounded_pagination_and_redaction() {
        let server = MockServer::start();
        let (_dir, r) = fixture(&server);
        let next = format!("{}/iris/v1/threads?page=2", server.base_url());
        server.mock(|when, then| {
            when.method(GET)
                .path("/iris/v1/threads")
                .query_param("page", "1");
            then.json_body(
                json!({"data":[{"id":"one","password":"secret-sentinel"}],"links":{"next":next}}),
            );
        });
        server.mock(|when, then| {
            when.method(GET)
                .path("/iris/v1/threads")
                .query_param("page", "2");
            then.json_body(json!({"data":[{"id":"two","cookies":{"session":"secret-sentinel"},"echo":"cookie=secret-sentinel"}]}));
        });
        let response = r.pages("/iris/v1/threads", &[("page", "1")]).await.unwrap();
        assert_eq!(response["data"].as_array().unwrap().len(), 2);
        assert!(!response.to_string().contains("secret-sentinel"));
    }
    #[tokio::test]
    async fn rejects_cross_origin_and_cyclic_pagination() {
        for cross_origin in [true, false] {
            let server = MockServer::start();
            let (_dir, r) = fixture(&server);
            let next = if cross_origin {
                "https://evil.example/iris/v1/threads".into()
            } else {
                format!("{}/iris/v1/threads?page=1", server.base_url())
            };
            server.mock(|when, then| {
                when.method(GET);
                then.json_body(json!({"data":[],"links":{"next":next}}));
            });
            let error = r
                .pages("/iris/v1/threads", &[("page", "1")])
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(if cross_origin {
                "unsafe_pagination"
            } else {
                "pagination_cycle"
            }));
        }
    }
    #[tokio::test]
    async fn monitor_repeats_until_exact_snapshot_is_acknowledged() {
        let server = MockServer::start();
        let (_dir, r) = fixture(&server);
        server.mock(|when, then| {
            when.method(GET).path("/iris/v1/apps/123/reviewSubmissions");
            then.json_body(json!({"data":[{"id":"sub"}]}));
        });
        server.mock(|when, then| {
            when.method(GET).path("/iris/v1/resolutionCenterThreads");
            then.json_body(json!({"data":[{"id":"thread"}]}));
        });
        server.mock(|when,then| { when.method(GET).path("/iris/v1/resolutionCenterThreads/thread/resolutionCenterMessages"); then.json_body(json!({"data":[
            {"id":"apple","attributes":{"messageBody":"Please explain","createdDate":"2026-01-01"},"relationships":{"fromActor":{"data":{"id":"a"}}}},
            {"id":"developer","attributes":{"messageBody":"My reply"},"relationships":{"fromActor":{"data":{"id":"d"}}}},
            {"id":"other","attributes":{"messageBody":"Other actor"},"relationships":{"fromActor":{"data":{"id":"o"}}}}
        ],"included":[{"type":"actors","id":"a","attributes":{"actorType":"APPLE","name":"App Review"}},{"type":"actors","id":"d","attributes":{"actorType":"DEVELOPER"}},{"type":"actors","id":"o","attributes":{"actorType":"USER","userFirstName":"Test developer"}}]})); });
        let first = r.check_updates().await.unwrap();
        let second = r.check_updates().await.unwrap();
        assert_eq!(first["status"], "new_messages");
        assert_eq!(second["new_messages"].as_array().unwrap().len(), 1);
        assert_eq!(second["new_messages"][0]["id"], "apple");
        assert!(
            r.acknowledge(first["snapshot_token"].as_str().unwrap())
                .unwrap_err()
                .to_string()
                .contains("snapshot_changed")
        );
        r.acknowledge(second["snapshot_token"].as_str().unwrap())
            .unwrap();
        assert_eq!(r.check_updates().await.unwrap()["status"], "unchanged");
        let lock = MonitorLock::acquire(&r.root).unwrap();
        assert!(
            r.acknowledge(second["snapshot_token"].as_str().unwrap())
                .is_err()
        );
        drop(lock);
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(r.root.join("messages.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&r.root).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }
    #[tokio::test]
    async fn reloads_session_on_each_call() {
        let server = MockServer::start();
        let (_dir, r) = fixture(&server);
        let mock = server.mock(|when, then| {
            when.method(GET).header("Cookie", "session=changed");
            then.json_body(json!({"data":[]}));
        });
        atomic_json(
            &r.cookie_path,
            &json!({"cookies":[{"name":"session","value":"changed","domain":"apple.com"}]}),
        )
        .unwrap();
        r.pages("/iris/v1/threads", &[]).await.unwrap();
        mock.assert_calls(1);
    }
    #[tokio::test]
    async fn legacy_reply_disabled_and_post_is_never_retried() {
        let server = MockServer::start();
        let (_dir, mut r) = fixture(&server);
        assert!(
            !r.tools()
                .iter()
                .any(|t| t["name"] == "reply_reviewer_message")
        );
        assert!(r.call("reply_reviewer_message", &json!({})).await.is_err());
        r.legacy_reply = true;
        server.mock(|when,then| { when.method(GET); then.header("csrf","safe").header("csrf_ts","123").json_body(json!({"statusCode":"SUCCESS","data":{"appNotes":{"threads":[{"id":"t","active":true,"canDeveloperAddNote":true,"versionId":"v","version":"1.0","messages":[]}]}}})); });
        let post = server.mock(|when, then| {
            when.method(POST);
            then.status(503).body("secret-sentinel");
        });
        let error = r.call("reply_reviewer_message",&json!({"app_id":"123","platform":"ios","thread_id":"t","sender":"Developer","message":"Hello"})).await.unwrap_err().to_string();
        assert!(error.contains("delivery_unknown"));
        assert!(!error.contains("secret-sentinel"));
        post.assert_calls(1);
    }
    #[tokio::test]
    async fn legacy_error_envelope_is_not_success() {
        let server = MockServer::start();
        let (_dir, r) = fixture(&server);
        server.mock(|when, then| {
            when.method(GET);
            then.json_body(json!({"statusCode":"ERROR","secret":"secret-sentinel"}));
        });
        let err = r
            .call(
                "get_legacy_reviewer_context",
                &json!({"app_id":"123","platform":"ios"}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("legacy_api_error"));
        assert!(!err.contains("secret-sentinel"));
    }
    #[test]
    fn advisory_lock_releases_and_ignores_old_marker_file() {
        let dir = tempfile::tempdir().unwrap();
        ensure_root(dir.path()).unwrap();
        fs::write(dir.path().join(".monitor.lock"), "dead-pid").unwrap();
        let lock = MonitorLock::acquire(dir.path()).unwrap();
        assert!(MonitorLock::acquire(dir.path()).is_err());
        drop(lock);
        assert!(MonitorLock::acquire(dir.path()).is_ok());
    }
    #[cfg(unix)]
    #[test]
    #[ignore]
    fn lock_child_process() {
        let Some(root) = std::env::var_os("ASC_LOCK_TEST_ROOT") else {
            return;
        };
        let _lock = MonitorLock::acquire(Path::new(&root)).unwrap();
        fs::write(Path::new(&root).join("ready"), "ready").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
    #[cfg(unix)]
    #[test]
    fn killed_owner_does_not_block_future_monitor() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("reviewer::tests::lock_child_process")
            .arg("--ignored")
            .env("ASC_LOCK_TEST_ROOT", dir.path())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let ready = dir.path().join("ready");
        for _ in 0..100 {
            if ready.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(ready.exists());
        assert!(MonitorLock::acquire(dir.path()).is_err());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(MonitorLock::acquire(dir.path()).is_ok());
    }

    #[tokio::test]
    async fn monitor_unknown_actor_does_not_claim_no_new_messages() {
        let server = MockServer::start();
        let (_dir, r) = fixture(&server);
        server.mock(|when, then| {
            when.method(GET).path("/iris/v1/apps/123/reviewSubmissions");
            then.json_body(json!({"data":[{"id":"sub"}]}));
        });
        server.mock(|when, then| {
            when.method(GET).path("/iris/v1/resolutionCenterThreads");
            then.json_body(json!({"data":[{"id":"thread"}]}));
        });
        server.mock(|when,then|{when.method(GET).path("/iris/v1/resolutionCenterThreads/thread/resolutionCenterMessages");then.json_body(json!({"data":[{"id":"unknown","attributes":{"messageBody":"Message"},"relationships":{"fromActor":{"data":{"id":"u"}}}}],"included":[{"type":"actors","id":"u","attributes":{"actorType":"UNKNOWN"}}]}));});
        assert!(
            r.check_updates()
                .await
                .unwrap_err()
                .to_string()
                .contains("actor_schema_unknown")
        );
        assert!(!r.root.join("messages.json").exists());
    }
}
