//! Bounded, session-pinned App Privacy no-collection operations through Fastlane's Iris routes.
//! No disclosure deletion or arbitrary privacy edits. Durable intent records precede all writes.
use super::*;

pub(super) const NAMES: &[&str] = &[
    "get_app_privacy_details",
    "declare_app_privacy_no_data",
    "publish_app_privacy_no_data",
];
pub(super) fn tools() -> Vec<Value> {
    NAMES.iter().map(|name| {
        let mut props=json!({"app_id":{"type":"string","pattern":"^[0-9]{1,128}$"}});
        let mut required=vec!["app_id"];
        if *name!="get_app_privacy_details" {
            props["confirmation"]=json!({"type":"string","enum":["DATA_NOT_COLLECTED"]}); required.push("confirmation");
        }
        json!({"name":name,"description":match *name {
            "get_app_privacy_details"=>"Read App Privacy declaration and publication through session-backed Iris GETs. Reconcile a private uncertain intent only when the desired state is conclusively visible; never writes to Apple.",
            "declare_app_privacy_no_data"=>"Declare DATA_NOT_COLLECTED once through session-backed Iris. Requires explicit confirmation, refuses existing collection disclosures and unknown intents, never deletes or retries a write.",
            _=>"Publish a verified single DATA_NOT_COLLECTED declaration through session-backed Iris. Requires explicit confirmation and fresh preflight. Never retries an uncertain PATCH."},
            "inputSchema":{"type":"object","properties":props,"required":required,"additionalProperties":false},
            "annotations":{"readOnlyHint":*name=="get_app_privacy_details","destructiveHint":false,"openWorldHint":true}})
    }).collect()
}
struct AppLock(fs::File);
impl AppLock {
    fn acquire(root: &Path, app: &str) -> Result<Self> {
        let file = open_private_lock(root, &format!(".privacy-{app}.lock"))?;
        fs2::FileExt::try_lock_exclusive(&file)
            .map_err(|_| anyhow::anyhow!("reviewer_privacy_busy"))?;
        Ok(Self(file))
    }
}
impl Drop for AppLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
#[derive(Serialize, Deserialize)]
struct Intent {
    app_id: String,
    generation: String,
    action: String,
    state: String,
    publish_state_id: String,
}
struct Snapshot {
    count: usize,
    no_data: bool,
    publish_id: String,
    published: bool,
}
impl Snapshot {
    fn safe_no_data(&self) -> bool {
        self.count == 1 && self.no_data
    }
    fn require_no_collection(&self) -> Result<()> {
        if self.count > 1 || self.count == 1 && !self.no_data {
            bail!("reviewer_privacy_conflict: existing disclosures are never deleted");
        }
        Ok(())
    }
}
fn valid_public_id(pin: &Pinned, value: &Value) -> Result<String> {
    let s = value
        .as_str()
        .filter(|s| identifier(s) && s.len() <= 128 && pin.public(s))
        .context("reviewer_privacy_schema_invalid")?;
    Ok(s.into())
}
fn relation<'a>(item: &'a Value, key: &str) -> Result<Option<&'a Value>> {
    match item.pointer(&format!("/relationships/{key}")) {
        None => Ok(None),
        Some(v) => match v.get("data") {
            Some(Value::Null) => Ok(None),
            Some(v) if v.is_object() => Ok(Some(v)),
            _ => bail!("reviewer_privacy_schema_invalid"),
        },
    }
}
fn resource_id(pin: &Pinned, value: &Value, kind: &str) -> Result<String> {
    if value["type"] != kind {
        bail!("reviewer_privacy_schema_invalid");
    }
    valid_public_id(pin, &value["id"])
}
fn known_rejection(error: &anyhow::Error) -> bool {
    let text = error.to_string();
    if text.starts_with("reviewer_session_changed") {
        return true;
    }
    let status = text
        .split("HTTP ")
        .nth(1)
        .and_then(|v| v.split_ascii_whitespace().next())
        .and_then(|v| v.trim_end_matches(';').parse::<u16>().ok());
    status.is_some_and(|s| (400..500).contains(&s) && s != 408)
}
impl Reviewer {
    fn privacy_path(&self, app: &str) -> PathBuf {
        self.root.join(format!("privacy-{app}.json"))
    }
    fn privacy_load(&self, app: &str) -> Result<Option<Intent>> {
        let path = self.privacy_path(app);
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => bail!("reviewer_privacy_record_unreadable"),
            Ok(_) => (),
        }
        let bytes = private_read(&path, 4096)
            .map_err(|_| anyhow::anyhow!("reviewer_privacy_record_unreadable"))?;
        let i: Intent = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("reviewer_privacy_record_invalid"))?;
        if i.app_id != app
            || !matches!(i.action.as_str(), "declare" | "publish")
            || !matches!(i.state.as_str(), "pending" | "confirmed" | "rejected")
        {
            bail!("reviewer_privacy_record_invalid");
        }
        Ok(Some(i))
    }
    fn privacy_save(&self, i: &Intent) -> Result<()> {
        atomic_json(&self.privacy_path(&i.app_id), &serde_json::to_value(i)?)
            .map(|_| ())
            .map_err(|_| anyhow::anyhow!("reviewer_privacy_record_unwritable"))
    }
    async fn privacy_snapshot(&self, pin: &Pinned, app: &str) -> Result<Snapshot> {
        let (owner, _) = self
            .pinned(
                pin,
                Method::GET,
                self.url(&format!("/iris/v1/apps/{app}"), &[])?,
                None,
                true,
            )
            .await?;
        if resource_id(pin, &owner["data"], "apps")? != app {
            bail!("reviewer_privacy_target_mismatch");
        }
        let path = format!("/iris/v1/apps/{app}/dataUsages");
        let first = self.url(
            &path,
            &[
                ("include", "category,grouping,purpose,dataProtection"),
                ("limit", "500"),
            ],
        )?;
        let mut next = first.clone();
        let mut seen = HashSet::new();
        let mut count = 0;
        let mut no_data = false;
        for page in 0..20 {
            if !seen.insert(next.as_str().to_owned()) {
                bail!("reviewer_pagination_cycle");
            }
            let (response, _) = self.pinned(pin, Method::GET, next, None, true).await?;
            let usages = response["data"]
                .as_array()
                .context("reviewer_privacy_schema_invalid")?;
            for usage in usages {
                resource_id(pin, usage, "appDataUsages")?;
                if let Some(owner) = relation(usage, "app")?
                    && resource_id(pin, owner, "apps")? != app
                {
                    bail!("reviewer_privacy_target_mismatch");
                }
                let protection = relation(usage, "dataProtection")?
                    .context("reviewer_privacy_schema_invalid")?;
                let protection = resource_id(pin, protection, "appDataUsageDataProtections")?;
                let mut empty = true;
                for (field, kind) in [
                    ("category", "appDataUsageCategories"),
                    ("purpose", "appDataUsagePurposes"),
                    ("grouping", "appDataUsageGroupings"),
                ] {
                    if let Some(v) = relation(usage, field)? {
                        resource_id(pin, v, kind)?;
                        empty = false;
                    }
                }
                count += 1;
                if count > 10000 {
                    bail!("reviewer_privacy_schema_invalid");
                }
                no_data = count == 1 && protection == "DATA_NOT_COLLECTED" && empty;
            }
            match response.pointer("/links/next") {
                None | Some(Value::Null) => break,
                Some(Value::String(s)) if s.is_empty() => break,
                Some(Value::String(s)) => {
                    if page == 19 {
                        bail!("reviewer_pagination_incomplete");
                    }
                    let candidate = Url::parse(s)
                        .map_err(|_| anyhow::anyhow!("reviewer_unsafe_pagination_url"))?;
                    if candidate.origin() != self.origin.origin()
                        || candidate.path() != path
                        || !candidate.username().is_empty()
                        || candidate.password().is_some()
                        || candidate.fragment().is_some()
                    {
                        bail!("reviewer_unsafe_pagination_url");
                    }
                    next = scoped_next(candidate, &first)?;
                }
                _ => bail!("reviewer_unsafe_pagination_url"),
            }
        }
        let (published, _) = self
            .pinned(
                pin,
                Method::GET,
                self.url(&format!("/iris/v1/apps/{app}/dataUsagePublishState"), &[])?,
                None,
                true,
            )
            .await?;
        let publish_id = resource_id(pin, &published["data"], "appDataUsagesPublishState")?;
        let published = published["data"]["attributes"]["published"]
            .as_bool()
            .context("reviewer_privacy_schema_invalid")?;
        self.check_pin(pin).await?;
        Ok(Snapshot {
            count,
            no_data,
            publish_id,
            published,
        })
    }
    fn privacy_reconcile(&self, pin: &Pinned, s: &Snapshot, i: &mut Intent) -> Result<bool> {
        if i.state != "pending" {
            return Ok(false);
        }
        if i.generation != pin.generation {
            bail!(
                "reviewer_privacy_outcome_unknown: authentication source differs from pending intent"
            );
        }
        if s.safe_no_data()
            && (i.action == "declare" || s.published && i.publish_state_id == s.publish_id)
        {
            i.state = "confirmed".into();
            self.privacy_save(i)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub(super) async fn privacy_call(&self, name: &str, args: &Value) -> Result<Value> {
        let app = arg(args, "app_id")?;
        if app.is_empty() || app.len() > 128 || !app.bytes().all(|b| b.is_ascii_digit()) {
            bail!("invalid app_id");
        }
        let read = name == "get_app_privacy_details";
        if !read && arg(args, "confirmation")? != "DATA_NOT_COLLECTED" {
            bail!("reviewer_privacy_confirmation_required");
        }
        let _lock = AppLock::acquire(&self.root, app)?;
        let pin = self.pin().await?;
        if !pin.public(app) {
            bail!("reviewer_privacy_target_invalid");
        }
        let s = self.privacy_snapshot(&pin, app).await?;
        let mut prior = self.privacy_load(app)?;
        let mut reconciled = false;
        let source_matches = prior.as_ref().map(|i| i.generation == pin.generation);
        if let Some(i) = prior.as_mut() {
            // Status remains readable after authentication is replaced, but the old
            // generation's pending intent can never be cleared using the new source.
            if !read || i.generation == pin.generation {
                reconciled = self.privacy_reconcile(&pin, &s, i)?;
            }
        }
        let pending = prior.as_ref().is_some_and(|i| i.state == "pending");
        if read {
            return Ok(
                json!({"app_id":app,"usage_count":s.count,"data_not_collected":s.safe_no_data(),"published":s.published,"publish_state_id":s.publish_id,"outcome_unknown":pending,"authentication_source_matches_intent":source_matches,"reconciliation":if reconciled {Some("desired_state_confirmed")}else{None}}),
            );
        }
        if pending {
            bail!(
                "reviewer_privacy_outcome_unknown: inspect with get_app_privacy_details; no write was retried"
            );
        }
        s.require_no_collection()?;
        let publish = name == "publish_app_privacy_no_data";
        if publish && !s.safe_no_data() {
            bail!("reviewer_privacy_declaration_required");
        }
        let status = if publish { "published" } else { "declared" };
        if !publish && s.safe_no_data() || publish && s.published {
            return Ok(json!({"app_id":app,"status":status,"already_present":true}));
        }
        self.check_pin(&pin).await?;
        let mut intent = Intent {
            app_id: app.into(),
            generation: pin.generation.clone(),
            action: if publish { "publish" } else { "declare" }.into(),
            state: "pending".into(),
            publish_state_id: s.publish_id.clone(),
        };
        self.privacy_save(&intent)?;
        let (method, path, payload) = if publish {
            (
                Method::PATCH,
                format!("/iris/v1/appDataUsagesPublishState/{}", s.publish_id),
                json!({"data":{"type":"appDataUsagesPublishState","id":s.publish_id,"attributes":{"published":true}}}),
            )
        } else {
            (
                Method::POST,
                "/iris/v1/appDataUsages".into(),
                json!({"data":{"type":"appDataUsages","relationships":{"app":{"data":{"type":"apps","id":app}},"dataProtection":{"data":{"type":"appDataUsageDataProtections","id":"DATA_NOT_COLLECTED"}}}}}),
            )
        };
        if let Err(error) = self
            .pinned(&pin, method, self.url(&path, &[])?, Some(payload), true)
            .await
        {
            if known_rejection(&error) {
                intent.state = "rejected".into();
                self.privacy_save(&intent)?;
                bail!("reviewer_privacy_write_rejected: no retry was made");
            }
            bail!(
                "reviewer_privacy_outcome_unknown: write may have completed; inspect with get_app_privacy_details"
            );
        }
        let after = self.privacy_snapshot(&pin, app).await.map_err(|_| {
            anyhow::anyhow!("reviewer_privacy_outcome_unknown: readback failed; no retry was made")
        })?;
        if !self.privacy_reconcile(&pin, &after, &mut intent)? {
            bail!(
                "reviewer_privacy_outcome_unknown: desired state is not confirmed; no retry was made"
            );
        }
        Ok(json!({"app_id":app,"status":status,"reconciliation":"desired_state_confirmed"}))
    }
}
