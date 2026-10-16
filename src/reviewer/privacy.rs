//! Session-pinned App Privacy operations through Fastlane's Iris routes.
//! Collection replacement requires a full exact snapshot and durable intent before every write.
use super::*;

pub(super) const NAMES: &[&str] = &[
    "get_app_privacy_details",
    "declare_app_privacy_no_data",
    "publish_app_privacy_no_data",
    "set_app_privacy_collection",
    "publish_app_privacy_collection",
    "acknowledge_app_privacy_partial",
    "acknowledge_app_privacy_published_goal",
];
pub(super) fn tools() -> Vec<Value> {
    NAMES.iter().filter(|name| matches!(**name, "get_app_privacy_details" | "declare_app_privacy_no_data" | "publish_app_privacy_no_data")).map(|name| {
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
    #[serde(default)]
    recovery_snapshot_digest: Option<String>,
    #[serde(default)]
    archived_intent_fingerprint: Option<String>,
    #[serde(default)]
    publication_before: Option<PublicationCheckpoint>,
    #[serde(default)]
    publication_after: Option<PublicationCheckpoint>,
    #[serde(default)]
    confirmed_snapshot_digest: Option<String>,
    #[serde(default)]
    last_error: Option<PrivacyFailure>,
    #[serde(default)]
    publication_protocol_version: Option<u8>,
    #[serde(default)]
    publication_acceptance: Option<PublicationAcceptance>,
    #[serde(default)]
    publication_basis: Option<PublicationBasis>,
    #[serde(default)]
    publication_goal_archive_fingerprint: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicationAcceptance {
    http_status: u16,
}
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PublicationBasis {
    RequestAcceptedCurrentPublished,
    FreshPublicationVerified,
    CurrentPublishedGoalAcknowledged,
}
struct Snapshot {
    count: usize,
    no_data: bool,
    publish_id: String,
    published: bool,
    rows: Vec<UsageRow>,
    publication: PublicationCheckpoint,
}
#[derive(Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PublicationTimestampStatus {
    Valid,
    Null,
    #[default]
    Missing,
    Invalid,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PublicationCheckpoint {
    published: bool,
    last_published_status: PublicationTimestampStatus,
    last_published: Option<DateTime<Utc>>,
}
impl PublicationCheckpoint {
    fn valid_before(&self) -> bool {
        self.last_published_status == PublicationTimestampStatus::Valid
            && self.last_published.is_some()
            || !self.published
                && self.last_published_status == PublicationTimestampStatus::Null
                && self.last_published.is_none()
    }
    fn proves_new_publication(&self, before: &Self) -> bool {
        if !self.published_non_regressing(before) {
            return false;
        }
        !before.published
            || self
                .last_published
                .zip(before.last_published)
                .is_some_and(|(after, before)| after > before)
    }
    fn published_non_regressing(&self, before: &Self) -> bool {
        self.published
            && self.last_published_status == PublicationTimestampStatus::Valid
            && self.last_published.is_some()
            && before.valid_before()
            && before.last_published.is_none_or(|baseline| {
                self.last_published
                    .is_some_and(|current| current >= baseline)
            })
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct PrivacyFailure {
    kind: String,
    http_status: Option<u16>,
}
fn privacy_failure(error: &anyhow::Error) -> PrivacyFailure {
    let text = error.to_string();
    let http_status = text
        .split("HTTP ")
        .nth(1)
        .and_then(|s| s.split_ascii_whitespace().next())
        .and_then(|s| s.trim_end_matches([';', ',', ':']).parse::<u16>().ok())
        .filter(|s| (100..600).contains(s));
    let kind = if text.contains("contradictory publication response") {
        "contradictory_response"
    } else if text.starts_with("reviewer_session_changed") {
        "authentication_source_changed"
    } else if text.contains("invalid JSON") {
        "non_json"
    } else if text.contains("interrupted response") {
        "interrupted_response"
    } else if text.contains("oversized response") {
        "oversized_response"
    } else if text.contains("redirect") {
        "redirect"
    } else if http_status.is_some_and(|s| s >= 500) {
        "server_error"
    } else if http_status.is_some() {
        "http_rejected"
    } else if text.contains("request interrupted") || text.contains("connection failed") {
        "transport"
    } else {
        "client_error"
    };
    PrivacyFailure {
        kind: kind.into(),
        http_status,
    }
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
    // Historical Iris questionnaires store tracking once per category, without
    // a purpose relationship. This is observed source state, never a new row.
    fn legacy_category_tracking(&self) -> bool {
        self.protection == "DATA_USED_TO_TRACK_YOU"
            && self.purpose.is_none()
            && self
                .category
                .as_deref()
                .is_some_and(|category| CATEGORIES.contains(&category))
    }

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
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcknowledgeArgs {
    app_id: String,
    disclosures: Vec<Disclosure>,
    expected_intent_fingerprint: String,
    expected_snapshot_digest: String,
    confirmation: String,
    exclusive_editing_confirmed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishedGoalArgs {
    app_id: String,
    disclosures: Vec<Disclosure>,
    expected_intent_fingerprint: String,
    expected_snapshot_digest: String,
    confirmation: String,
    exclusive_editing_confirmed: bool,
    current_published_goal_acknowledged: bool,
    answers_accurate: bool,
    complies_with_app_store_guidelines_and_law: bool,
    will_update_if_changed: bool,
}
fn valid_digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
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
    let mut result:Vec<Value> = ["set_app_privacy_collection", "publish_app_privacy_collection"].iter().map(|name| {
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
    }).collect();
    let mut schema = result[0]["inputSchema"].clone();
    schema["properties"]["confirmation"] =
        json!({"type":"string","enum":["ACKNOWLEDGE_CONFIRMED_APP_PRIVACY_PARTIAL"]});
    schema["properties"]["expected_intent_fingerprint"] = json!({"type":"string","pattern":"^[0-9a-f]{64}$","description":"Fresh intent_fingerprint from get_app_privacy_details. Binds the exact original pending private record without exposing credentials."});
    schema["required"]
        .as_array_mut()
        .unwrap()
        .push(json!("expected_intent_fingerprint"));
    result.push(json!({"name":"acknowledge_app_privacy_partial","description":"Acknowledge only a conclusively observed applied prefix of an original collection batch. Requires exact original-intent and fresh snapshot digests, unchanged authentication source, original matrix and exclusive editing. Archives original bytes before saving a partial checkpoint. Sends only GET requests to Apple; never retries, rolls back, deletes disclosures, publishes or claims the whole matrix complete. Supports original NoData, a same-desired normalized subset, or a replacement already past every obsolete DELETE and through an exactly observed POST prefix. Every obsolete resource ID must be absent; uncertainty at or before the last obsolete collection DELETE is refused. An exact checkpoint is proven by a bounded archive chain; continuation only POSTs the missing suffix.","inputSchema":schema,"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":true}}));
    let mut schema = result[1]["inputSchema"].clone();
    schema["properties"]["confirmation"] =
        json!({"type":"string","enum":["ACKNOWLEDGE_CURRENT_PUBLISHED_APP_PRIVACY_GOAL"]});
    schema["properties"]["expected_intent_fingerprint"] =
        json!({"type":"string","pattern":"^[0-9a-f]{64}$"});
    schema["properties"]["current_published_goal_acknowledged"] =
        json!({"type":"boolean","const":true});
    schema["required"].as_array_mut().unwrap().extend([
        json!("expected_intent_fingerprint"),
        json!("current_published_goal_acknowledged"),
    ]);
    result.push(json!({"name":"acknowledge_app_privacy_published_goal","description":"Acknowledge only an already published exact goal for a versionless legacy pending collection publish intent. Requires both exact hashes, original matrix/source/resource/rows and unchanged original publication checkpoint, exclusive editing and accuracy/compliance attestations. Archives original bytes; sends only GETs to Apple. Never retries PATCH or claims historical HTTP acceptance or a fresh publication. New receipt-protocol intents are excluded.","inputSchema":schema,"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":true}}));
    result
}
fn publication_original_rows(intent: &Intent) -> Result<Vec<UsageRow>> {
    let previous = intent
        .previous_snapshot
        .as_ref()
        .context("reviewer_privacy_record_invalid")?;
    let before = intent
        .publication_before
        .as_ref()
        .context("reviewer_privacy_record_invalid")?;
    let rows: Vec<UsageRow> = serde_json::from_value(previous["rows"].clone())
        .map_err(|_| anyhow::anyhow!("reviewer_privacy_record_invalid"))?;
    let initial = Snapshot {
        count: rows.len(),
        no_data: false,
        rows: rows.clone(),
        publish_id: intent.publish_state_id.clone(),
        published: before.published,
        publication: before.clone(),
    };
    if previous["snapshot_digest"] != initial.fingerprint(&intent.app_id)
        || previous["published"] != before.published
        || previous["last_published"] != json!(before.last_published)
        || previous["last_published_status"] != json!(before.last_published_status)
        || intent.desired.as_ref() != Some(&initial.tuples()?)
    {
        bail!("reviewer_privacy_record_invalid");
    }
    Ok(rows)
}
fn publication_goal_matches(snapshot: &Snapshot, intent: &Intent) -> bool {
    let Some(before) = &intent.publication_before else {
        return false;
    };
    let Ok(mut original) = publication_original_rows(intent) else {
        return false;
    };
    let mut actual = snapshot.rows.clone();
    original.sort();
    actual.sort();
    snapshot.publish_id == intent.publish_state_id
        && snapshot.publication.published_non_regressing(before)
        && actual == original
        && snapshot
            .tuples()
            .is_ok_and(|tuples| intent.desired.as_ref() == Some(&tuples))
}
fn publication_completion_basis(snapshot: &Snapshot, intent: &Intent) -> Option<PublicationBasis> {
    if !publication_goal_matches(snapshot, intent) {
        return None;
    }
    if intent.publication_acceptance.is_some() {
        Some(PublicationBasis::RequestAcceptedCurrentPublished)
    } else if intent
        .publication_before
        .as_ref()
        .is_some_and(|before| snapshot.publication.proves_new_publication(before))
    {
        Some(PublicationBasis::FreshPublicationVerified)
    } else {
        None
    }
}
fn publication_confirmed_snapshot_matches(snapshot: &Snapshot, intent: &Intent) -> bool {
    intent.state == "confirmed"
        && intent.confirmed_snapshot_digest.as_deref()
            == Some(snapshot.fingerprint(&intent.app_id).as_str())
        && intent.publication_after.as_ref() == Some(&snapshot.publication)
        && publication_goal_matches(snapshot, intent)
}
fn publication_acceptance(
    response: &Value,
    headers: &HeaderMap,
    app: &str,
    publish_id: &str,
) -> Result<PublicationAcceptance> {
    let status = headers
        .get("x-asc-mcp-status")
        .and_then(|s| s.to_str().ok())
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|s| (200..300).contains(s))
        .context("reviewer_privacy_acceptance_status_invalid")?;
    let errors = response.get("errors").is_some_and(|errors| {
        !errors.is_null() && errors.as_array().is_none_or(|array| !array.is_empty())
    });
    let contradictory = response.get("data").is_some_and(|data| {
        !data.is_null()
            && (!data.is_object()
                || data
                    .get("type")
                    .is_some_and(|v| v != "appDataUsagesPublishState")
                || data.get("id").is_some_and(|v| v != publish_id)
                || data
                    .pointer("/attributes/published")
                    .is_some_and(|v| v != true)
                || data
                    .pointer("/relationships/app/data/id")
                    .is_some_and(|v| v != app))
            || data
                .pointer("/relationships/app/data/type")
                .is_some_and(|v| v != "apps")
    });
    if errors || contradictory {
        bail!("reviewer_privacy_contradictory publication response; HTTP {status}");
    }
    Ok(PublicationAcceptance {
        http_status: status,
    })
}
fn legacy_published_goal_intent(intent: &Intent) -> bool {
    intent.action == "collection_publish"
        && intent.state == "pending"
        && intent.step == 1
        && intent.total_steps == 1
        && intent.publication_protocol_version.is_none()
        && intent.publication_acceptance.is_none()
        && intent.publication_basis.is_none()
        && intent.publication_goal_archive_fingerprint.is_none()
        && intent.last_error.is_none()
        && intent
            .publication_before
            .as_ref()
            .is_some_and(|before| before.published && before.valid_before())
        && publication_original_rows(intent).is_ok()
}
impl Snapshot {
    fn fingerprint(&self, app: &str) -> String {
        let mut rows = self.rows.clone();
        rows.sort();
        digest(serde_json::to_vec(&json!({"app_id":app,"rows":rows,"publish_state_id":self.publish_id,"published":self.published,"last_published":self.publication.last_published,"last_published_status":self.publication.last_published_status})).unwrap().as_slice())
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

    // Replacement alone can remove recognized legacy markers by their exact
    // preflight IDs. Desired rows, publication and reconciliation remain strict.
    fn replacement_tuples(&self) -> Result<(Vec<UsageTuple>, Vec<UsageRow>)> {
        Self::replacement_tuples_for(&self.rows)
    }

    fn replacement_tuples_for(rows: &[UsageRow]) -> Result<(Vec<UsageTuple>, Vec<UsageRow>)> {
        let mut ids = HashSet::new();
        let mut legacy_categories = HashSet::new();
        let mut modern = Vec::new();
        let mut legacy = Vec::new();
        for row in rows {
            if !ids.insert(&row.id) {
                bail!("reviewer_privacy_collection_schema_invalid");
            }
            if row.legacy_category_tracking() {
                if !legacy_categories.insert(row.category.as_deref().unwrap()) {
                    bail!("reviewer_privacy_collection_schema_invalid");
                }
                legacy.push(row.clone());
            } else {
                modern.push(row.tuple()?);
            }
        }
        modern.sort();
        if modern.windows(2).any(|w| w[0] == w[1]) {
            bail!("reviewer_privacy_collection_schema_invalid");
        }
        if !legacy.is_empty() {
            // A category marker cannot excuse an incomplete or contradictory
            // collection shape. Purposeful tracking in that category is a
            // different representation and must not be silently combined.
            disclosures_from_tuples(&modern)?;
            for marker in &legacy {
                let category = marker.category.as_deref().unwrap();
                if !modern.iter().any(|t| t.category == category)
                    || modern
                        .iter()
                        .any(|t| t.category == category && t.protection == "DATA_USED_TO_TRACK_YOU")
                    || rows.iter().any(|row| {
                        row.category.as_deref() == Some(category)
                            && !row.legacy_category_tracking()
                            && row.grouping != marker.grouping
                    })
                {
                    bail!("reviewer_privacy_collection_schema_invalid");
                }
            }
        }
        Ok((modern, legacy))
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
    fn privacy_intent_bytes(&self, app: &str) -> Result<Vec<u8>> {
        private_read(&self.privacy_path(app), MAX_BODY)
            .map_err(|_| anyhow::anyhow!("reviewer_privacy_record_unreadable"))
    }
    async fn privacy_acknowledge_published_goal(&self, args: &Value) -> Result<Value> {
        let a: PublishedGoalArgs = serde_json::from_value(args.clone())
            .map_err(|_| anyhow::anyhow!("reviewer_privacy_arguments_invalid"))?;
        if a.confirmation != "ACKNOWLEDGE_CURRENT_PUBLISHED_APP_PRIVACY_GOAL"
            || !a.exclusive_editing_confirmed
            || !a.current_published_goal_acknowledged
            || !a.answers_accurate
            || !a.complies_with_app_store_guidelines_and_law
            || !a.will_update_if_changed
        {
            bail!("reviewer_privacy_confirmation_required");
        }
        if a.app_id.is_empty()
            || a.app_id.len() > 128
            || !a.app_id.bytes().all(|b| b.is_ascii_digit())
            || !valid_digest(&a.expected_intent_fingerprint)
            || !valid_digest(&a.expected_snapshot_digest)
        {
            bail!("reviewer_privacy_arguments_invalid");
        }
        let desired = desired_tuples(&a.disclosures)?;
        let _lock = AppLock::acquire(&self.root, &a.app_id)?;
        let original = self.privacy_intent_bytes(&a.app_id)?;
        if digest(&original) != a.expected_intent_fingerprint {
            bail!("reviewer_privacy_intent_mismatch");
        }
        let mut intent = self
            .privacy_load(&a.app_id)?
            .context("reviewer_privacy_intent_required")?;
        if !legacy_published_goal_intent(&intent) || intent.desired.as_ref() != Some(&desired) {
            bail!("reviewer_privacy_published_goal_not_supported");
        }
        let pin = self.pin().await?;
        if intent.generation != pin.generation {
            bail!("reviewer_privacy_recovery_source_mismatch");
        }
        let snapshot = self.privacy_snapshot(&pin, &a.app_id).await?;
        if snapshot.fingerprint(&a.app_id) != a.expected_snapshot_digest
            || !publication_goal_matches(&snapshot, &intent)
            || intent.publication_before.as_ref() != Some(&snapshot.publication)
        {
            bail!("reviewer_privacy_published_goal_snapshot_mismatch");
        }
        let archive = self.root.join(format!(
            "privacy-{}-published-goal-{}.json",
            a.app_id, a.expected_intent_fingerprint
        ));
        match fs::symlink_metadata(&archive) {
            Ok(_) => {
                if private_read(&archive, MAX_BODY)
                    .map_err(|_| anyhow::anyhow!("reviewer_privacy_archive_invalid"))?
                    != original
                {
                    bail!("reviewer_privacy_archive_conflict");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                write_private_atomic(&archive, &original)
                    .map_err(|_| anyhow::anyhow!("reviewer_privacy_archive_unwritable"))?;
            }
            Err(_) => bail!("reviewer_privacy_archive_unreadable"),
        }
        if private_read(&archive, MAX_BODY)
            .map_err(|_| anyhow::anyhow!("reviewer_privacy_archive_invalid"))?
            != original
        {
            bail!("reviewer_privacy_archive_invalid");
        }
        if self.privacy_intent_bytes(&a.app_id)? != original {
            bail!("reviewer_privacy_intent_mismatch");
        }
        let fresh = self.privacy_snapshot(&pin, &a.app_id).await?;
        if fresh.fingerprint(&a.app_id) != a.expected_snapshot_digest {
            bail!("reviewer_privacy_published_goal_snapshot_mismatch");
        }
        self.check_pin(&pin).await?;
        intent.state = "confirmed".into();
        intent.publication_basis = Some(PublicationBasis::CurrentPublishedGoalAcknowledged);
        intent.publication_after = Some(fresh.publication.clone());
        intent.confirmed_snapshot_digest = Some(a.expected_snapshot_digest.clone());
        intent.publication_goal_archive_fingerprint = Some(a.expected_intent_fingerprint.clone());
        self.privacy_save(&intent)?;
        Ok(
            json!({"app_id":a.app_id,"status":"current_published_goal_acknowledged","request_accepted":null,"current_published":true,"fresh_publication_verified":false,"remote_writes":0,"archive_fingerprint":a.expected_intent_fingerprint,"snapshot_digest":a.expected_snapshot_digest}),
        )
    }
    fn privacy_partial_archive(&self, intent: &Intent) -> Result<(String, Intent)> {
        let fingerprint = intent
            .archived_intent_fingerprint
            .as_ref()
            .filter(|s| valid_digest(s))
            .context("reviewer_privacy_archive_required")?;
        let path = self.root.join(format!(
            "privacy-{}-partial-{fingerprint}.json",
            intent.app_id
        ));
        let bytes = private_read(&path, MAX_BODY)
            .map_err(|_| anyhow::anyhow!("reviewer_privacy_archive_required"))?;
        if digest(&bytes) != *fingerprint {
            bail!("reviewer_privacy_archive_invalid");
        }
        let ancestor: Intent = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("reviewer_privacy_archive_invalid"))?;
        if ancestor.app_id != intent.app_id
            || ancestor.generation != intent.generation
            || ancestor.desired != intent.desired
            || ancestor.publish_state_id != intent.publish_state_id
            || ancestor.action != "collection_set"
            || ancestor.state != "pending"
        {
            bail!("reviewer_privacy_recovery_context_mismatch");
        }
        Ok((fingerprint.clone(), ancestor))
    }
    fn privacy_verify_partial_prefix(
        &self,
        pin: &Pinned,
        intent: &Intent,
        observed: &[UsageRow],
        ancestors: &mut HashSet<String>,
    ) -> Result<()> {
        let desired = intent
            .desired
            .as_ref()
            .context("reviewer_privacy_recovery_not_supported")?;
        let previous = intent
            .previous_snapshot
            .as_ref()
            .context("reviewer_privacy_recovery_not_supported")?;
        let initial: Vec<UsageRow> = serde_json::from_value(previous["rows"].clone())
            .map_err(|_| anyhow::anyhow!("reviewer_privacy_record_invalid"))?;
        let initial_published = previous["published"]
            .as_bool()
            .context("reviewer_privacy_record_invalid")?;
        let mut initial_sorted = initial.clone();
        initial_sorted.sort();
        let mut initial_value = json!({"app_id":intent.app_id,"rows":initial_sorted,"publish_state_id":intent.publish_state_id,"published":initial_published});
        // Legacy records predate timestamp-aware fingerprints. The recorded schema
        // decides which observed fields were originally hashed; no values are guessed.
        if previous.get("last_published_status").is_some() {
            initial_value["last_published_status"] = previous["last_published_status"].clone();
            initial_value["last_published"] = previous["last_published"].clone();
        }
        let initial_digest = digest(&serde_json::to_vec(&initial_value)?);
        if previous["snapshot_digest"] != initial_digest {
            bail!("reviewer_privacy_record_invalid");
        }
        let mut ids = HashSet::new();
        if initial.iter().any(|r| {
            !identifier(&r.id) || r.id.len() > 128 || !pin.public(&r.id) || !ids.insert(&r.id)
        }) {
            bail!("reviewer_privacy_record_invalid");
        }
        let no_data = initial.len() == 1
            && initial[0].protection == "DATA_NOT_COLLECTED"
            && initial[0].category.is_none()
            && initial[0].purpose.is_none()
            && initial[0].grouping.is_none();
        let mut original_tuples = if no_data {
            vec![]
        } else {
            Snapshot::replacement_tuples_for(&initial)?.0
        };
        original_tuples.sort();
        // Reconstruct the original replacement plan. An attempted POST proves
        // every earlier exact-ID DELETE passed its readback before advancing.
        let obsolete: HashSet<_> = initial
            .iter()
            .filter(|row| !row.tuple().is_ok_and(|t| desired.contains(&t)))
            .map(|row| row.id.clone())
            .collect();
        let replacement_post_phase = !no_data && !obsolete.is_empty();
        if let Some(checkpoint) = &intent.recovery_snapshot_digest {
            // A resumed initial snapshot is only an additive desired prefix.
            if replacement_post_phase || checkpoint != &initial_digest || ancestors.len() >= 32 {
                bail!("reviewer_privacy_recovery_provenance_invalid");
            }
            let (fingerprint, ancestor) = self.privacy_partial_archive(intent)?;
            if !ancestors.insert(fingerprint) {
                bail!("reviewer_privacy_recovery_provenance_invalid");
            }
            // A resumed initial shape may be incomplete only when the archived
            // attempted prefix proves these exact retained rows and added tuples.
            self.privacy_verify_partial_prefix(pin, &ancestor, &initial, ancestors)?;
        } else if !original_tuples.is_empty() {
            disclosures_from_tuples(&original_tuples)?;
        }
        let missing: Vec<_> = desired
            .iter()
            .filter(|t| !original_tuples.contains(t))
            .cloned()
            .collect();
        let deletes = obsolete.len();
        if intent.total_steps != deletes + missing.len()
            || intent.step == 0
            || intent.step >= intent.total_steps
            || intent.step < deletes
            || replacement_post_phase && intent.step <= deletes
        {
            bail!("reviewer_privacy_recovery_not_partial");
        }
        let mut retained: Vec<_> = initial
            .into_iter()
            .filter(|row| !obsolete.contains(&row.id))
            .collect();
        let mut observed_retained = Vec::new();
        let mut observed_added = Vec::new();
        let mut seen_ids = HashSet::new();
        for row in observed {
            if !identifier(&row.id)
                || row.id.len() > 128
                || !pin.public(&row.id)
                || !seen_ids.insert(&row.id)
                || obsolete.contains(&row.id)
            {
                bail!("reviewer_privacy_recovery_prefix_mismatch");
            }
            if retained.iter().any(|r| r.id == row.id) {
                observed_retained.push(row.clone());
            } else {
                observed_added.push(row.tuple()?);
            }
        }
        retained.sort();
        observed_retained.sort();
        observed_added.sort();
        let mut expected_added = missing[..intent.step - deletes].to_vec();
        expected_added.sort();
        if observed_retained != retained || observed_added != expected_added {
            bail!(
                "reviewer_privacy_recovery_prefix_mismatch: an absent uncertain POST is never acknowledged"
            );
        }
        Ok(())
    }
    async fn privacy_acknowledge_partial(&self, args: &Value) -> Result<Value> {
        let a: AcknowledgeArgs = serde_json::from_value(args.clone())
            .map_err(|_| anyhow::anyhow!("reviewer_privacy_arguments_invalid"))?;
        if a.confirmation != "ACKNOWLEDGE_CONFIRMED_APP_PRIVACY_PARTIAL"
            || !a.exclusive_editing_confirmed
        {
            bail!("reviewer_privacy_confirmation_required");
        }
        if a.app_id.is_empty()
            || a.app_id.len() > 128
            || !a.app_id.bytes().all(|b| b.is_ascii_digit())
            || !valid_digest(&a.expected_intent_fingerprint)
            || !valid_digest(&a.expected_snapshot_digest)
        {
            bail!("reviewer_privacy_arguments_invalid");
        }
        let desired = desired_tuples(&a.disclosures)?;
        let _lock = AppLock::acquire(&self.root, &a.app_id)?;
        let original = self.privacy_intent_bytes(&a.app_id)?;
        if digest(&original) != a.expected_intent_fingerprint {
            bail!("reviewer_privacy_intent_mismatch");
        }
        let mut intent = self
            .privacy_load(&a.app_id)?
            .context("reviewer_privacy_intent_required")?;
        if intent.action != "collection_set"
            || intent.state != "pending"
            || intent.desired.as_ref() != Some(&desired)
        {
            bail!("reviewer_privacy_recovery_not_supported");
        }
        let pin = self.pin().await?;
        if pin.generation != intent.generation {
            bail!("reviewer_privacy_recovery_source_mismatch");
        }
        let snapshot = self.privacy_snapshot(&pin, &a.app_id).await?;
        if snapshot.fingerprint(&a.app_id) != a.expected_snapshot_digest
            || snapshot.publish_id != intent.publish_state_id
        {
            bail!("reviewer_privacy_recovery_snapshot_mismatch");
        }
        if intent.draft_was_unpublished && snapshot.published {
            bail!("reviewer_privacy_recovery_publication_changed");
        }
        self.privacy_verify_partial_prefix(&pin, &intent, &snapshot.rows, &mut HashSet::new())?;
        let archive = self.root.join(format!(
            "privacy-{}-partial-{}.json",
            a.app_id, a.expected_intent_fingerprint
        ));
        match fs::symlink_metadata(&archive) {
            Ok(_) => {
                if private_read(&archive, MAX_BODY)
                    .map_err(|_| anyhow::anyhow!("reviewer_privacy_archive_invalid"))?
                    != original
                {
                    bail!("reviewer_privacy_archive_conflict");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                write_private_atomic(&archive, &original)
                    .map_err(|_| anyhow::anyhow!("reviewer_privacy_archive_unwritable"))?
            }
            Err(_) => bail!("reviewer_privacy_archive_unreadable"),
        }
        if private_read(&archive, MAX_BODY)
            .map_err(|_| anyhow::anyhow!("reviewer_privacy_archive_invalid"))?
            != original
        {
            bail!("reviewer_privacy_archive_invalid");
        }
        if self.privacy_intent_bytes(&a.app_id)? != original {
            bail!("reviewer_privacy_intent_mismatch");
        }
        let fresh = self.privacy_snapshot(&pin, &a.app_id).await?;
        if fresh.fingerprint(&a.app_id) != a.expected_snapshot_digest {
            bail!("reviewer_privacy_recovery_snapshot_mismatch");
        }
        self.check_pin(&pin).await?;
        intent.state = "partial_confirmed".into();
        intent.recovery_snapshot_digest = Some(a.expected_snapshot_digest.clone());
        intent.archived_intent_fingerprint = Some(a.expected_intent_fingerprint.clone());
        self.privacy_save(&intent)?;
        Ok(
            json!({"app_id":a.app_id,"status":"partial_confirmed","collection_complete":false,"remote_writes":0,"completed_steps":intent.step,"total_steps":intent.total_steps,"archive_fingerprint":a.expected_intent_fingerprint,"recovery_snapshot_digest":a.expected_snapshot_digest}),
        )
    }
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
        let mut prior = self.privacy_load(&app)?;
        if let Some(i) = prior.as_mut() {
            self.privacy_reconcile(&pin, &snapshot, i)?;
            if i.state == "pending" {
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
        let (actual, legacy) = if snapshot.safe_no_data() {
            (vec![], vec![])
        } else if publish {
            (snapshot.tuples()?, vec![])
        } else {
            snapshot.replacement_tuples()?
        };
        let mut authenticated_partial = false;
        if let Some(i) = prior.as_ref().filter(|i| i.state == "partial_confirmed") {
            if i.generation != pin.generation
                || i.publish_state_id != snapshot.publish_id
                || i.desired.as_ref() != Some(&desired)
            {
                bail!("reviewer_privacy_recovery_context_mismatch");
            }
            if !publish
                && i.recovery_snapshot_digest.as_deref()
                    != Some(snapshot.fingerprint(&app).as_str())
            {
                bail!("reviewer_privacy_recovery_checkpoint_changed");
            }
            if publish && actual != desired {
                bail!("reviewer_privacy_partial_collection_incomplete");
            }
            if !publish {
                let (fingerprint, ancestor) = self.privacy_partial_archive(i)?;
                self.privacy_verify_partial_prefix(
                    &pin,
                    &ancestor,
                    &snapshot.rows,
                    &mut HashSet::from([fingerprint]),
                )?;
                authenticated_partial = true;
            }
        }
        // A valid partial prefix may split tracking rows across purposes. Only
        // its exact durable checkpoint and archived prefix authorize continuation.
        if !authenticated_partial && !snapshot.safe_no_data() && !snapshot.rows.is_empty() {
            disclosures_from_tuples(&actual)?;
        }
        if publish && actual != desired {
            bail!("reviewer_privacy_disclosures_mismatch");
        }
        let proven_publish = prior.as_ref().is_some_and(|i| {
            i.state == "confirmed"
                && i.action == "collection_publish"
                && i.generation == pin.generation
                && i.publish_state_id == snapshot.publish_id
                && i.desired.as_ref() == Some(&desired)
                && i.confirmed_snapshot_digest.as_deref()
                    == Some(snapshot.fingerprint(&app).as_str())
                && i.publication_after.as_ref() == Some(&snapshot.publication)
                && (publication_completion_basis(&snapshot, i).is_some()
                    || i.publication_basis
                        == Some(PublicationBasis::CurrentPublishedGoalAcknowledged)
                        && publication_goal_matches(&snapshot, i))
        });
        if !publish && actual == desired && legacy.is_empty() || publish && proven_publish {
            return Ok(
                json!({"app_id":app,"status":if publish {"published"} else {"draft_verified"},"already_present":true,"disclosures":snapshot.disclosures()?,"snapshot_digest":snapshot.fingerprint(&app),"published":snapshot.published,"current_published":snapshot.published,"request_accepted":prior.as_ref().and_then(|i|i.publication_acceptance.as_ref()).map(|_|true),"fresh_publication_verified":publish && prior.as_ref().is_some_and(|i|i.publication_basis != Some(PublicationBasis::CurrentPublishedGoalAcknowledged) && i.publication_before.as_ref().is_some_and(|before|snapshot.publication.proves_new_publication(before)))}),
            );
        }
        if publish && !snapshot.publication.valid_before() {
            bail!(
                "reviewer_privacy_publication_baseline_required: lastPublished is missing or invalid"
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
                json!({"rows":snapshot.rows,"snapshot_digest":expected,"published":snapshot.published,"last_published":snapshot.publication.last_published,"last_published_status":snapshot.publication.last_published_status}),
            ),
            step: 0,
            total_steps,
            draft_was_unpublished: false,
            recovery_snapshot_digest: if authenticated_partial {
                Some(expected.clone())
            } else {
                None
            },
            archived_intent_fingerprint: prior
                .as_ref()
                .and_then(|i| i.archived_intent_fingerprint.clone()),
            publication_before: if publish {
                Some(snapshot.publication.clone())
            } else {
                None
            },
            publication_after: None,
            confirmed_snapshot_digest: None,
            last_error: None,
            publication_protocol_version: publish.then_some(1),
            publication_acceptance: None,
            publication_basis: None,
            publication_goal_archive_fingerprint: None,
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
            let response = if method == Method::POST && path == "/iris/v1/appDataUsages" {
                self.pinned_privacy_create(
                    &pin,
                    payload.context("reviewer_privacy_arguments_invalid")?,
                )
                .await
            } else {
                self.pinned(&pin, method, self.url(&path, &[])?, payload, true)
                    .await
            };
            let response = if publish {
                response.and_then(|(body, headers)| {
                    publication_acceptance(&body, &headers, &app, &intent.publish_state_id)
                        .map(Some)
                })
            } else {
                response.map(|_| None)
            };
            let acceptance = match response {
                Ok(acceptance) => acceptance,
                Err(error) => {
                    intent.last_error = Some(privacy_failure(&error));
                    self.privacy_save(&intent)?;
                    if index == 0 && known_rejection(&error) {
                        intent.state = "rejected".into();
                        self.privacy_save(&intent)?;
                        bail!("reviewer_privacy_write_rejected: no retry was made");
                    }
                    bail!(
                        "reviewer_privacy_outcome_unknown: write may have completed; error_kind={}; http_status={}; inspect with get_app_privacy_details; no write was retried",
                        intent.last_error.as_ref().unwrap().kind,
                        intent
                            .last_error
                            .as_ref()
                            .unwrap()
                            .http_status
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "unknown".into())
                    );
                }
            };
            if let Some(receipt) = acceptance {
                intent.publication_acceptance = Some(receipt);
                // This durable receipt precedes GET. A crash or failed save never
                // authorizes retrying PATCH or entering the versionless recovery path.
                self.privacy_save(&intent)?;
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
                before_rows == after_rows && publication_completion_basis(&after, &intent).is_some()
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
            json!({"app_id":app,"status":if publish {"published"} else {"draft_verified"},"disclosures":snapshot.disclosures()?,"snapshot_digest":snapshot.fingerprint(&app),"published":snapshot.published,"current_published":snapshot.published,"request_accepted":intent.publication_acceptance.as_ref().map(|_|true),"fresh_publication_verified":publish && intent.publication_before.as_ref().is_some_and(|before|snapshot.publication.proves_new_publication(before)),"publication_basis":intent.publication_basis,"reconciliation":"desired_state_confirmed"}),
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
            || !matches!(
                i.state.as_str(),
                "pending" | "confirmed" | "rejected" | "partial_confirmed"
            )
        {
            bail!("reviewer_privacy_record_invalid");
        }
        if i.publication_protocol_version.is_some_and(|v| v != 1)
            || i.action != "collection_publish"
                && (i.publication_protocol_version.is_some()
                    || i.publication_acceptance.is_some()
                    || i.publication_basis.is_some()
                    || i.publication_goal_archive_fingerprint.is_some())
            || i.publication_acceptance.as_ref().is_some_and(|receipt| {
                i.publication_protocol_version != Some(1)
                    || !(200..300).contains(&receipt.http_status)
                    || i.step != 1
                    || i.total_steps != 1
                    || i.last_error.is_some()
            })
            || i.publication_basis.is_some_and(|basis| {
                i.state != "confirmed"
                    || match basis {
                        PublicationBasis::RequestAcceptedCurrentPublished => {
                            i.publication_acceptance.is_none()
                        }
                        PublicationBasis::FreshPublicationVerified => {
                            !i.publication_after.as_ref().is_some_and(|after| {
                                i.publication_before
                                    .as_ref()
                                    .is_some_and(|before| after.proves_new_publication(before))
                            })
                        }
                        PublicationBasis::CurrentPublishedGoalAcknowledged => {
                            i.publication_goal_archive_fingerprint.is_none()
                                || i.publication_protocol_version.is_some()
                                || i.publication_acceptance.is_some()
                        }
                    }
            })
        {
            bail!("reviewer_privacy_record_invalid");
        }
        if let Some(fingerprint) = &i.publication_goal_archive_fingerprint {
            if !valid_digest(fingerprint)
                || i.publication_basis != Some(PublicationBasis::CurrentPublishedGoalAcknowledged)
            {
                bail!("reviewer_privacy_record_invalid");
            }
            let archive = self
                .root
                .join(format!("privacy-{app}-published-goal-{fingerprint}.json"));
            let bytes = private_read(&archive, MAX_BODY)
                .map_err(|_| anyhow::anyhow!("reviewer_privacy_archive_required"))?;
            if digest(&bytes) != *fingerprint {
                bail!("reviewer_privacy_archive_invalid");
            }
            let original: Intent = serde_json::from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("reviewer_privacy_archive_invalid"))?;
            if !legacy_published_goal_intent(&original)
                || original.app_id != i.app_id
                || original.generation != i.generation
                || original.desired != i.desired
                || original.publish_state_id != i.publish_state_id
                || original.previous_snapshot != i.previous_snapshot
                || original.publication_before != i.publication_before
                || original.publication_before != i.publication_after
                || i.previous_snapshot
                    .as_ref()
                    .map(|v| v["snapshot_digest"].as_str())
                    != Some(i.confirmed_snapshot_digest.as_deref())
            {
                bail!("reviewer_privacy_archive_invalid");
            }
        }
        if let Some(fingerprint) = &i.archived_intent_fingerprint {
            if !valid_digest(fingerprint) {
                bail!("reviewer_privacy_record_invalid");
            }
            let archive = self
                .root
                .join(format!("privacy-{app}-partial-{fingerprint}.json"));
            let bytes = private_read(&archive, MAX_BODY)
                .map_err(|_| anyhow::anyhow!("reviewer_privacy_archive_required"))?;
            if digest(&bytes) != *fingerprint {
                bail!("reviewer_privacy_archive_invalid");
            }
        }
        if i.state == "partial_confirmed"
            && (i.action != "collection_set"
                || i.archived_intent_fingerprint.is_none()
                || !i
                    .recovery_snapshot_digest
                    .as_deref()
                    .is_some_and(valid_digest))
        {
            bail!("reviewer_privacy_record_invalid");
        }
        if i.last_error.as_ref().is_some_and(|e| {
            !matches!(
                e.kind.as_str(),
                "authentication_source_changed"
                    | "non_json"
                    | "interrupted_response"
                    | "oversized_response"
                    | "redirect"
                    | "server_error"
                    | "http_rejected"
                    | "transport"
                    | "client_error"
                    | "contradictory_response"
            ) || e.http_status.is_some_and(|s| !(100..600).contains(&s))
        }) {
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
        let (last_published_status, last_published) =
            match published.pointer("/data/attributes/lastPublished") {
                None => (PublicationTimestampStatus::Missing, None),
                Some(Value::Null) => (PublicationTimestampStatus::Null, None),
                Some(Value::String(s)) if pin.public(s) => match DateTime::parse_from_rfc3339(s) {
                    Ok(date) => (
                        PublicationTimestampStatus::Valid,
                        Some(date.with_timezone(&Utc)),
                    ),
                    Err(_) => (PublicationTimestampStatus::Invalid, None),
                },
                _ => (PublicationTimestampStatus::Invalid, None),
            };
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
            publication: PublicationCheckpoint {
                published,
                last_published_status,
                last_published,
            },
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
                    || i.action == "collection_publish"
                        && publication_completion_basis(s, i).is_some())
        } else {
            s.safe_no_data()
                && (i.action == "declare" || s.published && i.publish_state_id == s.publish_id)
        };
        if matches {
            i.state = "confirmed".into();
            i.confirmed_snapshot_digest = Some(s.fingerprint(&i.app_id));
            if i.action == "collection_publish" {
                i.publication_after = Some(s.publication.clone());
                i.publication_basis = publication_completion_basis(s, i);
            }
            self.privacy_save(i)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub(super) async fn privacy_call(&self, name: &str, args: &Value) -> Result<Value> {
        if name == "acknowledge_app_privacy_partial" {
            return self.privacy_acknowledge_partial(args).await;
        }
        if name == "acknowledge_app_privacy_published_goal" {
            return self.privacy_acknowledge_published_goal(args).await;
        }
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
            let intent_fingerprint = if prior.is_some() {
                Some(digest(&self.privacy_intent_bytes(app)?))
            } else {
                None
            };
            return Ok(
                json!({"app_id":app,"usage_count":s.count,"data_not_collected":s.safe_no_data(),"published":s.published,"current_published":s.published,"request_accepted":prior.as_ref().and_then(|i|i.publication_acceptance.as_ref()).map(|_|true),"fresh_publication_verified":source_matches==Some(true) && prior.as_ref().is_some_and(|i|i.action=="collection_publish" && i.publication_basis!=Some(PublicationBasis::CurrentPublishedGoalAcknowledged) && publication_confirmed_snapshot_matches(&s,i) && publication_completion_basis(&s,i).is_some() && i.publication_before.as_ref().is_some_and(|before|s.publication.proves_new_publication(before))),"publication_basis":prior.as_ref().and_then(|i|i.publication_basis),"publish_state_id":s.publish_id,"last_published":s.publication.last_published,"last_published_status":s.publication.last_published_status,"rows":s.rows,"disclosures":s.disclosures().ok(),"snapshot_digest":s.fingerprint(app),"intent_fingerprint":intent_fingerprint,"intent_state":prior.as_ref().map(|i|&i.state),"partial_confirmed":prior.as_ref().is_some_and(|i|i.state=="partial_confirmed"),"collection_complete":prior.as_ref().and_then(|i|i.desired.as_ref()).map(|d|s.tuples().is_ok_and(|t|t==*d)),"last_error":prior.as_ref().and_then(|i|i.last_error.as_ref()),"outcome_unknown":pending,"authentication_source_matches_intent":source_matches,"reconciliation":if reconciled {Some("desired_state_confirmed")}else{None}}),
            );
        }
        if prior
            .as_ref()
            .is_some_and(|i| i.state == "partial_confirmed")
        {
            bail!("reviewer_privacy_partial_collection_in_progress");
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
            recovery_snapshot_digest: None,
            archived_intent_fingerprint: None,
            publication_before: None,
            publication_after: None,
            confirmed_snapshot_digest: None,
            last_error: None,
            publication_protocol_version: None,
            publication_acceptance: None,
            publication_basis: None,
            publication_goal_archive_fingerprint: None,
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
