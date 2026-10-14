//! Session-pinned App Privacy operations through Fastlane's Iris routes.
//! Collection replacement requires a full exact snapshot and durable intent before every write.
use super::*;

pub(super) const NAMES: &[&str] = &[
    "get_app_privacy_details",
    "declare_app_privacy_no_data",
    "publish_app_privacy_no_data",
    "set_app_privacy_collection",
    "publish_app_privacy_collection",
];
pub(super) fn tools() -> Vec<Value> {
    NAMES.iter().filter(|name| !name.ends_with("_collection")).map(|name| {
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
    }).chain(collection_tools()).collect()
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
    #[serde(default)]
    desired: Option<Vec<UsageTuple>>,
    #[serde(default)]
    previous_snapshot: Option<Value>,
    #[serde(default)]
    step: usize,
    #[serde(default)]
    total_steps: usize,
    #[serde(default)]
    draft_was_unpublished: bool,
}
struct Snapshot {
    count: usize,
    no_data: bool,
    publish_id: String,
    published: bool,
    rows: Vec<UsageRow>,
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

// IDs and one-row-per-purpose/protection semantics are from Fastlane 2.239.0's
// AppDataUsage models and upload_app_privacy_details_to_app_store action.
const CATEGORIES: &[&str] = &[
    "PAYMENT_INFORMATION",
    "CREDIT_AND_FRAUD",
    "OTHER_FINANCIAL_INFO",
    "PRECISE_LOCATION",
    "SENSITIVE_INFO",
    "PHYSICAL_ADDRESS",
    "EMAIL_ADDRESS",
    "NAME",
    "PHONE_NUMBER",
    "OTHER_CONTACT_INFO",
    "CONTACTS",
    "EMAILS_OR_TEXT_MESSAGES",
    "PHOTOS_OR_VIDEOS",
    "AUDIO",
    "GAMEPLAY_CONTENT",
    "CUSTOMER_SUPPORT",
    "OTHER_USER_CONTENT",
    "BROWSING_HISTORY",
    "SEARCH_HISTORY",
    "USER_ID",
    "DEVICE_ID",
    "PURCHASE_HISTORY",
    "PRODUCT_INTERACTION",
    "ADVERTISING_DATA",
    "OTHER_USAGE_DATA",
    "CRASH_DATA",
    "PERFORMANCE_DATA",
    "OTHER_DIAGNOSTIC_DATA",
    "OTHER_DATA",
    "HEALTH",
    "FITNESS",
    "COARSE_LOCATION",
];
const PURPOSES: &[&str] = &[
    "THIRD_PARTY_ADVERTISING",
    "DEVELOPERS_ADVERTISING",
    "ANALYTICS",
    "PRODUCT_PERSONALIZATION",
    "APP_FUNCTIONALITY",
    "OTHER_PURPOSES",
];
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct UsageTuple {
    category: String,
    purpose: String,
    protection: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct UsageRow {
    id: String,
    category: Option<String>,
    purpose: Option<String>,
    grouping: Option<String>,
    protection: String,
}
impl UsageRow {
    fn tuple(&self) -> Result<UsageTuple> {
        let category = self
            .category
            .as_deref()
            .context("reviewer_privacy_collection_schema_invalid")?;
        let purpose = self
            .purpose
            .as_deref()
            .context("reviewer_privacy_collection_schema_invalid")?;
        if !CATEGORIES.contains(&category)
            || !PURPOSES.contains(&purpose)
            || !matches!(
                self.protection.as_str(),
                "DATA_LINKED_TO_YOU" | "DATA_NOT_LINKED_TO_YOU" | "DATA_USED_TO_TRACK_YOU"
            )
        {
            bail!("reviewer_privacy_collection_schema_invalid");
        }
        Ok(UsageTuple {
            category: category.into(),
            purpose: purpose.into(),
            protection: self.protection.clone(),
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Disclosure {
    category: String,
    purposes: Vec<String>,
    linked_to_user: bool,
    used_for_tracking: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetArgs {
    app_id: String,
    disclosures: Vec<Disclosure>,
    expected_snapshot_digest: String,
    confirmation: String,
    exclusive_editing_confirmed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishArgs {
    app_id: String,
    disclosures: Vec<Disclosure>,
    expected_snapshot_digest: String,
    confirmation: String,
    exclusive_editing_confirmed: bool,
    answers_accurate: bool,
    complies_with_app_store_guidelines_and_law: bool,
    will_update_if_changed: bool,
}
fn desired_tuples(disclosures: &[Disclosure]) -> Result<Vec<UsageTuple>> {
    if disclosures.is_empty() || disclosures.len() > CATEGORIES.len() {
        bail!("reviewer_privacy_disclosures_invalid");
    }
    let mut categories = HashSet::new();
    let mut result = Vec::new();
    for d in disclosures {
        if !CATEGORIES.contains(&d.category.as_str())
            || !categories.insert(&d.category)
            || d.purposes.is_empty()
            || d.purposes.len() > PURPOSES.len()
        {
            bail!("reviewer_privacy_disclosures_invalid");
        }
        let mut purposes = HashSet::new();
        for purpose in &d.purposes {
            if !PURPOSES.contains(&purpose.as_str()) || !purposes.insert(purpose) {
                bail!("reviewer_privacy_disclosures_invalid");
            }
            result.push(UsageTuple {
                category: d.category.clone(),
                purpose: purpose.clone(),
                protection: if d.linked_to_user {
                    "DATA_LINKED_TO_YOU"
                } else {
                    "DATA_NOT_LINKED_TO_YOU"
                }
                .into(),
            });
            if d.used_for_tracking {
                result.push(UsageTuple {
                    category: d.category.clone(),
                    purpose: purpose.clone(),
                    protection: "DATA_USED_TO_TRACK_YOU".into(),
                });
            }
        }
    }
    result.sort();
    Ok(result)
}
fn collection_tools() -> Vec<Value> {
    ["set_app_privacy_collection", "publish_app_privacy_collection"].iter().map(|name| {
        let publish = *name == "publish_app_privacy_collection";
        let mut properties = json!({"app_id":{"type":"string","pattern":"^[0-9]{1,128}$"},
          "expected_snapshot_digest":{"type":"string","pattern":"^[0-9a-f]{64}$","description":"Exact snapshot_digest from a fresh get_app_privacy_details call; matches the full observed preflight questionnaire, not a server conditional write."},
          "exclusive_editing_confirmed":{"type":"boolean","const":true,"description":"Caller confirms no other questionnaire editor or mutation caller will operate for this entire batch. Client preflight/readback cannot detect every concurrent same-resource change."},
          "confirmation":{"type":"string","enum":[if publish { "PUBLISH_APP_PRIVACY_DISCLOSURES_ACCURATE" } else { "REPLACE_APP_PRIVACY_DISCLOSURES" }]},
          "disclosures":{"type":"array","minItems":1,"maxItems":32,"items":{"type":"object","additionalProperties":false,"required":["category","purposes","linked_to_user","used_for_tracking"],"properties":{"category":{"type":"string","enum":CATEGORIES},"purposes":{"type":"array","minItems":1,"maxItems":6,"uniqueItems":true,"items":{"type":"string","enum":PURPOSES}},"linked_to_user":{"type":"boolean"},"used_for_tracking":{"type":"boolean"}}}}});
        let mut required = vec!["app_id", "expected_snapshot_digest", "confirmation", "exclusive_editing_confirmed", "disclosures"];
        if publish { for field in ["answers_accurate", "complies_with_app_store_guidelines_and_law", "will_update_if_changed"] {
            properties[field] = json!({"type":"boolean","const":true}); required.push(field);
        }}
        json!({"name":name,"description":if publish { "Publish only the exact intended collection disclosure matrix observed in a fresh full snapshot. Requires exclusive questionnaire editing, accuracy, compliance and future-update attestations. One pinned PATCH, verified GET, no retries. No server conditional-write contract: client checks cannot detect all concurrent changes." } else { "Replace the full collection questionnaire with an explicitly confirmed matrix only if its fresh snapshot digest matches. Requires exclusive questionnaire editing for the entire batch. Removes only obsolete exact preflight rows and adds missing tuples. Writes are non-atomic; client preflight/readback is not server compare-and-swap and cannot detect all concurrent same-resource changes. Pinned durable intents block uncertain partial results." },"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":!publish,"openWorldHint":true}})
    }).collect()
}
impl Snapshot {
    fn fingerprint(&self, app: &str) -> String {
        let mut rows = self.rows.clone();
        rows.sort();
        digest(serde_json::to_vec(&json!({"app_id":app,"rows":rows,"publish_state_id":self.publish_id,"published":self.published})).unwrap().as_slice())
    }
    fn tuples(&self) -> Result<Vec<UsageTuple>> {
        let mut ids = HashSet::new();
        let mut result = Vec::new();
        for row in &self.rows {
            if !ids.insert(&row.id) {
                bail!("reviewer_privacy_collection_schema_invalid");
            }
            result.push(row.tuple()?);
        }
        result.sort();
        if result.windows(2).any(|w| w[0] == w[1]) {
            bail!("reviewer_privacy_collection_schema_invalid");
        }
        Ok(result)
    }
    fn disclosures(&self) -> Result<Vec<Disclosure>> {
        if self.safe_no_data() || self.rows.is_empty() {
            return Ok(vec![]);
        }
        let tuples = self.tuples()?;
        disclosures_from_tuples(&tuples)
    }
}
fn disclosures_from_tuples(tuples: &[UsageTuple]) -> Result<Vec<Disclosure>> {
    let mut categories: BTreeMap<String, BTreeMap<String, HashSet<String>>> = BTreeMap::new();
    for t in tuples {
        categories
            .entry(t.category.clone())
            .or_default()
            .entry(t.purpose.clone())
            .or_default()
            .insert(t.protection.clone());
    }
    let mut result = Vec::new();
    for (category, purposes) in categories {
        let mut shape = None;
        for protections in purposes.values() {
            let linked = protections.contains("DATA_LINKED_TO_YOU");
            let unlinked = protections.contains("DATA_NOT_LINKED_TO_YOU");
            if linked == unlinked {
                bail!("reviewer_privacy_collection_schema_invalid");
            }
            let pair = (linked, protections.contains("DATA_USED_TO_TRACK_YOU"));
            if shape.is_some_and(|s| s != pair) {
                bail!("reviewer_privacy_collection_schema_invalid");
            }
            shape = Some(pair);
        }
        let (linked_to_user, used_for_tracking) =
            shape.context("reviewer_privacy_collection_schema_invalid")?;
        result.push(Disclosure {
            category,
            purposes: purposes.into_keys().collect(),
            linked_to_user,
            used_for_tracking,
        });
    }
    Ok(result)
}
impl Reviewer {
    async fn privacy_collection_call(&self, name: &str, args: &Value) -> Result<Value> {
        let publish = name == "publish_app_privacy_collection";
        let (app, disclosures, expected) = if publish {
            let a: PublishArgs = serde_json::from_value(args.clone())
                .map_err(|_| anyhow::anyhow!("reviewer_privacy_arguments_invalid"))?;
            if !a.exclusive_editing_confirmed {
                bail!("reviewer_privacy_exclusive_editing_required");
            }
            if a.confirmation != "PUBLISH_APP_PRIVACY_DISCLOSURES_ACCURATE"
                || !a.answers_accurate
                || !a.complies_with_app_store_guidelines_and_law
                || !a.will_update_if_changed
            {
                bail!("reviewer_privacy_confirmation_required");
            }
            (a.app_id, a.disclosures, a.expected_snapshot_digest)
        } else {
            let a: SetArgs = serde_json::from_value(args.clone())
                .map_err(|_| anyhow::anyhow!("reviewer_privacy_arguments_invalid"))?;
            if !a.exclusive_editing_confirmed {
                bail!("reviewer_privacy_exclusive_editing_required");
            }
            if a.confirmation != "REPLACE_APP_PRIVACY_DISCLOSURES" {
                bail!("reviewer_privacy_confirmation_required");
            }
            (a.app_id, a.disclosures, a.expected_snapshot_digest)
        };
        if app.is_empty() || app.len() > 128 || !app.bytes().all(|b| b.is_ascii_digit()) {
            bail!("invalid app_id");
        }
        if expected.len() != 64
            || !expected
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
        {
            bail!("reviewer_privacy_snapshot_invalid");
        }
        let desired = desired_tuples(&disclosures)?;
        let _lock = AppLock::acquire(&self.root, &app)?;
        let pin = self.pin().await?;
        if !pin.public(&app)
            || desired.iter().any(|t| {
                !pin.public(&t.category) || !pin.public(&t.purpose) || !pin.public(&t.protection)
            })
        {
            bail!("reviewer_privacy_target_invalid");
        }
        let mut snapshot = self.privacy_snapshot(&pin, &app).await?;
        if let Some(mut prior) = self.privacy_load(&app)? {
            self.privacy_reconcile(&pin, &snapshot, &mut prior)?;
            if prior.state == "pending" {
                bail!(
                    "reviewer_privacy_outcome_unknown: inspect with get_app_privacy_details; no write was retried"
                );
            }
        }
        if snapshot.fingerprint(&app) != expected {
            bail!(
                "reviewer_privacy_snapshot_mismatch: read the entire current questionnaire before replacing or publishing it"
            );
        }
        // Refuse ambiguous existing collection shapes before any destructive request.
        if !snapshot.safe_no_data() && !snapshot.rows.is_empty() {
            snapshot.disclosures()?;
        }
        let actual = if snapshot.safe_no_data() {
            vec![]
        } else {
            snapshot.tuples()?
        };
        if publish && actual != desired {
            bail!("reviewer_privacy_disclosures_mismatch");
        }
        if !publish && actual == desired || publish && snapshot.published {
            return Ok(
                json!({"app_id":app,"status":if publish {"published"} else {"draft_verified"},"already_present":true,"disclosures":snapshot.disclosures()?,"snapshot_digest":snapshot.fingerprint(&app),"published":snapshot.published}),
            );
        }
        let obsolete: Vec<UsageRow> = snapshot
            .rows
            .iter()
            .filter(|row| !row.tuple().is_ok_and(|t| desired.contains(&t)))
            .cloned()
            .collect();
        let missing: Vec<UsageTuple> = desired
            .iter()
            .filter(|t| !actual.contains(t))
            .cloned()
            .collect();
        let total_steps = if publish {
            1
        } else {
            obsolete.len() + missing.len()
        };
        let mut intent = Intent {
            app_id: app.clone(),
            generation: pin.generation.clone(),
            action: if publish {
                "collection_publish"
            } else {
                "collection_set"
            }
            .into(),
            state: "pending".into(),
            publish_state_id: snapshot.publish_id.clone(),
            desired: Some(desired.clone()),
            previous_snapshot: Some(
                json!({"rows":snapshot.rows,"snapshot_digest":expected,"published":snapshot.published}),
            ),
            step: 0,
            total_steps,
            draft_was_unpublished: false,
        };
        let mut operations = Vec::new();
        if publish {
            operations.push((Method::PATCH, format!("/iris/v1/appDataUsagesPublishState/{}", snapshot.publish_id), Some(json!({"data":{"type":"appDataUsagesPublishState","id":snapshot.publish_id,"attributes":{"published":true}}})), None, None));
        } else {
            for row in obsolete {
                operations.push((
                    Method::DELETE,
                    format!("/iris/v1/appDataUsages/{}", row.id),
                    None,
                    Some(row.id),
                    None,
                ));
            }
            for t in missing {
                let payload = json!({"data":{"type":"appDataUsages","relationships":{"app":{"data":{"type":"apps","id":app}},"category":{"data":{"type":"appDataUsageCategories","id":t.category}},"purpose":{"data":{"type":"appDataUsagePurposes","id":t.purpose}},"dataProtection":{"data":{"type":"appDataUsageDataProtections","id":t.protection}}}}});
                operations.push((
                    Method::POST,
                    "/iris/v1/appDataUsages".into(),
                    Some(payload),
                    None,
                    Some(t),
                ));
            }
        }
        for (index, (method, path, payload, removed, added)) in operations.into_iter().enumerate() {
            let fresh = self.privacy_snapshot(&pin, &app).await.map_err(|_| {
                anyhow::anyhow!(
                    "reviewer_privacy_outcome_unknown: preflight failed; no further write was made"
                )
            })?;
            if fresh.fingerprint(&app) != snapshot.fingerprint(&app) {
                bail!("reviewer_privacy_snapshot_changed: no further write was made");
            }
            intent.step = index + 1;
            if !publish && !snapshot.published {
                intent.draft_was_unpublished = true;
            }
            self.check_pin(&pin).await?;
            self.privacy_save(&intent)?;
            if let Err(error) = self
                .pinned(&pin, method, self.url(&path, &[])?, payload, true)
                .await
            {
                if index == 0 && known_rejection(&error) {
                    intent.state = "rejected".into();
                    self.privacy_save(&intent)?;
                    bail!("reviewer_privacy_write_rejected: no retry was made");
                }
                bail!(
                    "reviewer_privacy_outcome_unknown: a draft write may have completed; inspect with get_app_privacy_details; no write was retried"
                );
            }
            let after = self.privacy_snapshot(&pin, &app).await.map_err(|_| {
                anyhow::anyhow!(
                    "reviewer_privacy_outcome_unknown: readback failed; no retry was made"
                )
            })?;
            // Save this observation before validating the step or confirming the
            // final matrix, including a one-DELETE batch with no next preflight.
            if !publish
                && after.publish_id == intent.publish_state_id
                && !after.published
                && !intent.draft_was_unpublished
            {
                intent.draft_was_unpublished = true;
                self.privacy_save(&intent)?;
            }
            let mut before_rows = snapshot.rows.clone();
            before_rows.sort();
            let mut after_rows = after.rows.clone();
            after_rows.sort();
            // A published readback after an unpublished checkpoint is outside
            // draft-only authorization. Keep the prewritten intent pending.
            if !publish && intent.draft_was_unpublished && after.published {
                bail!(
                    "reviewer_privacy_outcome_unknown: draft was published during replacement; no further write was made"
                );
            }
            let confirmed = if let Some(id) = removed {
                before_rows.retain(|r| r.id != id);
                before_rows == after_rows
            } else if let Some(tuple) = added {
                let new_rows: Vec<_> = after_rows
                    .iter()
                    .filter(|r| !before_rows.iter().any(|b| b.id == r.id))
                    .collect();
                let retained: Vec<_> = after_rows
                    .iter()
                    .filter(|r| before_rows.iter().any(|b| b.id == r.id))
                    .cloned()
                    .collect();
                retained == before_rows
                    && new_rows.len() == 1
                    && new_rows[0].tuple().is_ok_and(|t| t == tuple)
            } else {
                after.published
                    && after.publish_id == intent.publish_state_id
                    && before_rows == after_rows
            };
            if !confirmed || after.publish_id != intent.publish_state_id {
                bail!(
                    "reviewer_privacy_outcome_unknown: exact step outcome is not confirmed; no retry was made"
                );
            }
            snapshot = after;
        }
        if !self.privacy_reconcile(&pin, &snapshot, &mut intent)? {
            bail!(
                "reviewer_privacy_outcome_unknown: exact desired state is not confirmed; no retry was made"
            );
        }
        Ok(
            json!({"app_id":app,"status":if publish {"published"} else {"draft_verified"},"disclosures":snapshot.disclosures()?,"snapshot_digest":snapshot.fingerprint(&app),"published":snapshot.published,"reconciliation":"desired_state_confirmed"}),
        )
    }
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
        let bytes = private_read(&path, MAX_BODY)
            .map_err(|_| anyhow::anyhow!("reviewer_privacy_record_unreadable"))?;
        let i: Intent = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("reviewer_privacy_record_invalid"))?;
        if i.app_id != app
            || !matches!(
                i.action.as_str(),
                "declare" | "publish" | "collection_set" | "collection_publish"
            )
            || !matches!(i.state.as_str(), "pending" | "confirmed" | "rejected")
        {
            bail!("reviewer_privacy_record_invalid");
        }
        let collection = matches!(i.action.as_str(), "collection_set" | "collection_publish");
        if collection != i.desired.is_some() {
            bail!("reviewer_privacy_record_invalid");
        }
        if let Some(desired) = &i.desired {
            if desired.is_empty()
                || desired.len() > CATEGORIES.len() * PURPOSES.len() * 2
                || desired.windows(2).any(|w| w[0] >= w[1])
                || desired.iter().any(|t| {
                    !CATEGORIES.contains(&t.category.as_str())
                        || !PURPOSES.contains(&t.purpose.as_str())
                        || !matches!(
                            t.protection.as_str(),
                            "DATA_LINKED_TO_YOU"
                                | "DATA_NOT_LINKED_TO_YOU"
                                | "DATA_USED_TO_TRACK_YOU"
                        )
                })
            {
                bail!("reviewer_privacy_record_invalid");
            }
            let disclosures = disclosures_from_tuples(desired)
                .map_err(|_| anyhow::anyhow!("reviewer_privacy_record_invalid"))?;
            if desired_tuples(&disclosures)
                .map_err(|_| anyhow::anyhow!("reviewer_privacy_record_invalid"))?
                != *desired
            {
                bail!("reviewer_privacy_record_invalid");
            }
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
        let mut rows = Vec::new();
        for page in 0..20 {
            if !seen.insert(next.as_str().to_owned()) {
                bail!("reviewer_pagination_cycle");
            }
            let (response, _) = self.pinned(pin, Method::GET, next, None, true).await?;
            let usages = response["data"]
                .as_array()
                .context("reviewer_privacy_schema_invalid")?;
            for usage in usages {
                let id = resource_id(pin, usage, "appDataUsages")?;
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
                rows.push(UsageRow {
                    id,
                    category: relation(usage, "category")?
                        .map(|v| resource_id(pin, v, "appDataUsageCategories"))
                        .transpose()?,
                    purpose: relation(usage, "purpose")?
                        .map(|v| resource_id(pin, v, "appDataUsagePurposes"))
                        .transpose()?,
                    grouping: relation(usage, "grouping")?
                        .map(|v| resource_id(pin, v, "appDataUsageGroupings"))
                        .transpose()?,
                    protection,
                });
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
            rows,
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
        let matches = if let Some(desired) = &i.desired {
            s.tuples().is_ok_and(|actual| actual == *desired)
                && i.publish_state_id == s.publish_id
                && (i.action == "collection_set" && (!i.draft_was_unpublished || !s.published)
                    || i.action == "collection_publish" && s.published)
        } else {
            s.safe_no_data()
                && (i.action == "declare" || s.published && i.publish_state_id == s.publish_id)
        };
        if matches {
            i.state = "confirmed".into();
            self.privacy_save(i)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub(super) async fn privacy_call(&self, name: &str, args: &Value) -> Result<Value> {
        if name.ends_with("_collection") {
            return self.privacy_collection_call(name, args).await;
        }
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
                json!({"app_id":app,"usage_count":s.count,"data_not_collected":s.safe_no_data(),"published":s.published,"publish_state_id":s.publish_id,"rows":s.rows,"disclosures":s.disclosures().ok(),"snapshot_digest":s.fingerprint(app),"outcome_unknown":pending,"authentication_source_matches_intent":source_matches,"reconciliation":if reconciled {Some("desired_state_confirmed")}else{None}}),
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
            desired: None,
            previous_snapshot: None,
            step: 0,
            total_steps: 1,
            draft_was_unpublished: false,
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
