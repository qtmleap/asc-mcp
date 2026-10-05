//! Public API request dispatch, compiled from offline Go handler probes.
use crate::client::ApiClient;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiRequest {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Option<Value>,
}
fn recipes() -> &'static Value {
    static RECIPES: OnceLock<Value> = OnceLock::new();
    RECIPES.get_or_init(|| {
        serde_json::from_str(include_str!("../data/request_recipes.json"))
            .expect("checked request recipes")
    })
}
pub fn is_compound(name: &str) -> bool {
    recipes()[name]["kind"] == "compound"
}
pub fn resource_segment(value: &str) -> Result<String> {
    if value == "." || value == ".." || value.contains('/') || value.contains('\\') {
        bail!("resource IDs must be a single path segment")
    }
    const RESERVED: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
        .add(b' ')
        .add(b'%')
        .add(b'?')
        .add(b'#')
        .add(b':')
        .add(b'[')
        .add(b']')
        .add(b'@')
        .add(b'!')
        .add(b'$')
        .add(b'&')
        .add(b'\'')
        .add(b'(')
        .add(b')')
        .add(b'*')
        .add(b'+')
        .add(b',')
        .add(b';')
        .add(b'=');
    Ok(percent_encoding::utf8_percent_encode(value, RESERVED).to_string())
}
fn string(v: &Value) -> String {
    v.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string())
}
fn nonempty(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
        _ => true,
    }
}
fn subvalue(mut v: Value, sub: &Value) -> Option<Value> {
    if let Some(parts) = sub.as_array() {
        for part in parts {
            v = if let Some(a) = v.as_array() {
                a.get(part.as_str()?.parse::<usize>().ok()?)?.clone()
            } else {
                v.get(part.as_str()?)?.clone()
            };
        }
    }
    Some(v)
}
fn eval(node: &Value, args: &Value, item: Option<&Value>, index: usize) -> Result<Option<Value>> {
    let key = node["name"].as_str().unwrap_or("");
    let arg = args.get(key).filter(|x| !x.is_null());
    Ok(match node["op"].as_str().context("recipe operation")? {
        "literal" => Some(node["value"].clone()),
        "object" => {
            let mut fields = Map::new();
            for (key, value) in node["fields"].as_object().context("recipe object")? {
                if let Some(v) = eval(value, args, item, index)? {
                    fields.insert(key.clone(), v);
                }
            }
            if fields.is_empty() && node["omit_if_empty"] == true {
                None
            } else {
                Some(Value::Object(fields))
            }
        }
        "array" => {
            let mut values = Vec::new();
            for value in node["items"].as_array().context("recipe array")? {
                if let Some(v) = eval(value, args, item, index)? {
                    values.push(v);
                }
            }
            Some(Value::Array(values))
        }
        "arg" => {
            let mut v = arg.cloned().or_else(|| node.get("default").cloned());
            if let Some(value) = v.as_mut() {
                if value.as_str() == Some("")
                    && let Some(default) = node.get("empty_default")
                {
                    *value = default.clone();
                }
                if value.as_i64().is_some_and(|n| n <= 0)
                    && let Some(default) = node.get("nonpositive_default")
                {
                    *value = default.clone();
                }
                if let Some(max) = node["max"].as_i64()
                    && value.as_i64().is_some_and(|n| n > max)
                {
                    *value = json!(max);
                }
            }
            if node["omit_false"] == true && v == Some(Value::Bool(false)) {
                return Ok(None);
            }
            if let Some(value) = v {
                let v = subvalue(value, &node["sub"]);
                if node["string"] == true {
                    v.map(|x| Value::String(string(&x)))
                } else {
                    v
                }
            } else {
                None
            }
        }
        "when_positive" => {
            let present = arg
                .and_then(Value::as_i64)
                .map(|v| v > 0)
                .unwrap_or(node["allow_missing"] == true);
            if present {
                eval(&node["value"], args, item, index)?
            } else {
                None
            }
        }
        "when" => {
            let present = arg.is_some_and(|v| node["condition"] == "present" || nonempty(v));
            if present {
                eval(&node["value"], args, item, index)?
            } else {
                None
            }
        }
        "each" => {
            let values = arg.and_then(Value::as_array);
            if values.is_some_and(|v| v.is_empty())
                && let Some(default) = node.get("empty_default")
            {
                return Ok(Some(default.clone()));
            }
            let mut out = Vec::new();
            if let Some(values) = values {
                for (i, value) in values.iter().enumerate() {
                    if let Some(v) = eval(&node["value"], args, Some(value), i)? {
                        out.push(v)
                    }
                }
            }
            if values.is_none() {
                Some(node.get("default").cloned().unwrap_or_else(|| json!([])))
            } else {
                Some(Value::Array(out))
            }
        }
        "item" => {
            let v = item.cloned().and_then(|v| subvalue(v, &node["sub"]));
            if node["omit_empty"] == true {
                v.filter(nonempty)
            } else {
                v
            }
        }
        "item_format" => item.map(|v| {
            json!(format!(
                "{}{}{}",
                node["prefix"].as_str().unwrap_or(""),
                string(v),
                node["suffix"].as_str().unwrap_or("")
            ))
        }),
        "index" => Some(json!(format!(
            "{}{}",
            node["prefix"].as_str().unwrap_or(""),
            index + 1
        ))),
        "join" => arg.and_then(Value::as_array).map(|a| {
            json!(
                a.iter()
                    .map(string)
                    .collect::<Vec<_>>()
                    .join(node["separator"].as_str().unwrap_or(","))
            )
        }),
        "format" => {
            if let Some(value) = arg {
                let mut value = string(value);
                if node["encode_path"] == true {
                    value = resource_segment(&value)?;
                }
                Some(json!(format!(
                    "{}{}{}",
                    node["prefix"].as_str().unwrap_or(""),
                    value,
                    node["suffix"].as_str().unwrap_or("")
                )))
            } else {
                None
            }
        }
        unknown => bail!("unknown recipe operation: {unknown}"),
    })
}
/// Build an HTTP request without credentials, network access, or side effects.
pub fn build_request(name: &str, args: &Value) -> Result<ApiRequest> {
    let recipe = recipes()
        .get(name)
        .with_context(|| format!("unknown tool: {name}"))?;
    if recipe["supports_cursor"] == true
        && let Some(cursor) = args["cursor"].as_str().filter(|s| !s.is_empty())
    {
        let url = url::Url::parse(cursor).context("invalid pagination cursor")?;
        return Ok(ApiRequest {
            method: "GET".into(),
            path: url.path().into(),
            query: url
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect(),
            body: None,
        });
    }
    let mut selected = recipe;
    if let Some(branches) = recipe["branches"].as_array() {
        for branch in branches {
            let matches = if let Some(absent) = branch["when_absent"].as_str() {
                !args.get(absent).is_some_and(nonempty)
            } else {
                let value = args.get(branch["when"].as_str().unwrap_or(""));
                value.is_some_and(nonempty)
                    && branch["equals"]
                        .as_str()
                        .map(|s| {
                            value
                                .unwrap()
                                .as_str()
                                .is_some_and(|v| v.eq_ignore_ascii_case(s))
                        })
                        .unwrap_or(true)
            };
            if matches {
                selected = branch;
                break;
            }
        }
    }
    let mut request = eval(&selected["request"], args, None, 0)?.context("empty recipe")?;
    let query = request["query"].as_object_mut().context("recipe query")?;
    if let Some(maps) = recipe["query_maps"].as_array() {
        for map in maps.iter().filter_map(Value::as_str) {
            if let Some(values) = args[map].as_object() {
                for (key, value) in values {
                    let array = value
                        .as_array()
                        .with_context(|| format!("{map}.{key} must be an array of strings"))?;
                    if array.iter().any(|v| !v.is_string()) {
                        bail!("{map}.{key} must be an array of strings")
                    }
                    if !array.is_empty() {
                        let query_key = format!("{map}[{key}]");
                        if recipe["query_map_overrides"]
                            .as_array()
                            .is_some_and(|keys| keys.iter().any(|v| v == &query_key))
                        {
                            query.remove(&query_key);
                        }
                        query.entry(query_key).or_insert_with(|| {
                            json!([array.iter().map(string).collect::<Vec<_>>().join(",")])
                        });
                    }
                }
            }
        }
    }
    let mut query_pairs = Vec::new();
    for (key, value) in request["query"].as_object().unwrap() {
        for v in value.as_array().context("query values")? {
            query_pairs.push((key.clone(), string(v)));
        }
    }
    Ok(ApiRequest {
        method: request["method"]
            .as_str()
            .context("request method")?
            .to_owned(),
        path: request["path"].as_str().context("request path")?.to_owned(),
        query: query_pairs,
        body: (!request["body"].is_null()).then(|| request["body"].clone()),
    })
}

// Intentional legacy schema repairs, checked against bundled Apple OpenAPI.
fn alias(args: &mut Map<String, Value>, old: &str, new: &str, value: Value) -> Result<()> {
    if let Some(existing) = args.get(new).filter(|v| !v.is_null())
        && *existing != value
    {
        bail!("conflicting {old} and {new} arguments")
    }
    args.insert(new.into(), value);
    args.remove(old);
    Ok(())
}
fn enum_string<'a>(value: &'a Value, field: &str, allowed: &[&str]) -> Result<&'a str> {
    let value = value
        .as_str()
        .with_context(|| format!("{field} must be a string"))?;
    if !allowed.contains(&value) {
        bail!("invalid {field}; expected {}", allowed.join(", "))
    }
    Ok(value)
}
fn canonical_arguments(name: &str, args: &Value) -> Result<Value> {
    let mut args = args
        .as_object()
        .context("arguments must be an object")?
        .clone();
    if name == "create_subscription_offer_code" {
        if let Some(v) = args
            .get("customer_eligibility")
            .filter(|v| !v.is_null())
            .cloned()
        {
            enum_string(&v, "customer_eligibility", &["NEW", "EXISTING", "EXPIRED"])?;
            alias(
                &mut args,
                "customer_eligibility",
                "customer_eligibilities",
                json!([v]),
            )?;
        }
        if let Some(v) = args
            .get("offer_eligibility_type")
            .filter(|v| !v.is_null())
            .cloned()
        {
            enum_string(
                &v,
                "offer_eligibility_type",
                &["STACK_WITH_INTRO_OFFERS", "REPLACE_INTRO_OFFERS"],
            )?;
            alias(&mut args, "offer_eligibility_type", "offer_eligibility", v)?;
        }
    }
    if name == "update_subscription_offer_code"
        && let Some(v) = args.get("is_active").filter(|v| !v.is_null()).cloned()
    {
        if !v.is_boolean() {
            bail!("is_active must be a boolean")
        }
        alias(&mut args, "is_active", "active", v)?;
    }
    if matches!(
        name,
        "create_promoted_purchase" | "update_promoted_purchase"
    ) && let Some(v) = args
        .get("visibility_type")
        .filter(|v| !v.is_null())
        .cloned()
    {
        let v = enum_string(
            &v,
            "visibility_type",
            &["SHOW_FOR_ALL_USERS", "APP_STORE_CONNECT_ONLY"],
        )?;
        alias(
            &mut args,
            "visibility_type",
            "visible_for_all_users",
            json!(v == "SHOW_FOR_ALL_USERS"),
        )?;
    }
    Ok(Value::Object(args))
}
/// Construct the runtime request, including explicitly tested repairs of legacy
/// schema mistakes. `build_request` remains the Go differential baseline.
pub fn prepare_request(name: &str, args: &Value) -> Result<ApiRequest> {
    crate::catalog::validate(name, args)?;
    let canonical = canonical_arguments(name, args)?;
    validate_supported_arguments(name, &canonical)?;
    let mut request = build_request(name, &canonical)?;
    if name == "create_promoted_purchase"
        && let Some(v) = canonical
            .get("visible_for_all_users")
            .filter(|v| !v.is_null())
    {
        request.body.as_mut().context("promoted purchase body")?["data"]["attributes"]["visibleForAllUsers"] =
            v.clone();
    }
    if name == "update_age_rating_declaration"
        && let Some(v) = canonical
            .get("horror_or_fear_themes")
            .filter(|v| !v.is_null())
    {
        enum_string(
            v,
            "horror_or_fear_themes",
            &[
                "NONE",
                "INFREQUENT_OR_MILD",
                "FREQUENT_OR_INTENSE",
                "INFREQUENT",
                "FREQUENT",
            ],
        )?;
        request.body.as_mut().context("age rating body")?["data"]["attributes"]["horrorOrFearThemes"] =
            v.clone();
    }
    if name == "create_subscription_offer_code" {
        enum_string(
            &canonical["duration"],
            "duration",
            &[
                "THREE_DAYS",
                "ONE_WEEK",
                "TWO_WEEKS",
                "ONE_MONTH",
                "TWO_MONTHS",
                "THREE_MONTHS",
                "SIX_MONTHS",
                "ONE_YEAR",
            ],
        )?;
        enum_string(
            &canonical["offer_eligibility"],
            "offer_eligibility",
            &["STACK_WITH_INTRO_OFFERS", "REPLACE_INTRO_OFFERS"],
        )?;
        enum_string(
            &canonical["offer_mode"],
            "offer_mode",
            &["FREE_TRIAL", "PAY_AS_YOU_GO", "PAY_UP_FRONT"],
        )?;
        if canonical["number_of_periods"]
            .as_i64()
            .is_none_or(|n| n <= 0)
        {
            bail!("number_of_periods must be positive")
        }
        let customers = canonical["customer_eligibilities"]
            .as_array()
            .context("customer_eligibility or customer_eligibilities is required")?;
        if customers.is_empty() {
            bail!("customer_eligibilities must not be empty")
        }
        for value in customers {
            enum_string(
                value,
                "customer_eligibilities",
                &["NEW", "EXISTING", "EXPIRED"],
            )?;
        }
        let ids = canonical.get("price_ids").filter(|v| !v.is_null());
        let prices = canonical.get("prices").filter(|v| !v.is_null());
        if ids.is_some() == prices.is_some() {
            bail!("provide exactly one of price_ids or prices")
        }
        let mut references = Vec::new();
        let mut included = Vec::new();
        if let Some(ids) = ids {
            let ids = ids.as_array().context("price_ids must be an array")?;
            if ids.is_empty() {
                bail!("price_ids must not be empty")
            }
            for id in ids {
                let id = id
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .context("each price_id must be a nonempty string")?;
                references.push(json!({"type":"subscriptionOfferCodePrices","id":id}));
            }
        }
        if let Some(prices) = prices {
            let prices = prices.as_array().context("prices must be an array")?;
            if prices.is_empty() {
                bail!("prices must not be empty")
            }
            for (index, price) in prices.iter().enumerate() {
                let territory = price["territory_id"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .context("each price requires territory_id")?;
                let point = price["price_point_id"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .context("each price requires price_point_id")?;
                let id = format!("price-{}", index + 1);
                references.push(json!({"type":"subscriptionOfferCodePrices","id":id}));
                included.push(json!({"type":"subscriptionOfferCodePrices","id":id,"relationships":{"territory":{"data":{"type":"territories","id":territory}},"subscriptionPricePoint":{"data":{"type":"subscriptionPricePoints","id":point}}}}));
            }
        }
        let body = request.body.as_mut().context("subscription offer body")?;
        body["data"]["relationships"]["prices"] = json!({"data":references});
        if !included.is_empty() {
            body["included"] = json!(included);
        }
    }
    Ok(request)
}

/// Advertised arguments that the legacy implementation ignored are rejected
/// explicitly rather than reporting a successful mutation of the wrong fields.
pub fn validate_supported_arguments(name: &str, args: &Value) -> Result<()> {
    let recipe = recipes()
        .get(name)
        .with_context(|| format!("unknown tool: {name}"))?;
    if let Some(ignored) = recipe["legacy_ignored"].as_array() {
        for key in ignored.iter().filter_map(Value::as_str) {
            if let Some(value) = args.get(key).filter(|v| nonempty(v)) {
                let default = crate::catalog::find(name)
                    .and_then(|t| t["inputSchema"]["properties"][key].get("default"));
                if name == "update_age_rating_declaration" && key == "horror_or_fear_themes" {
                    continue;
                }
                if default != Some(value) {
                    bail!(
                        "{name}: argument {key} was advertised but never implemented by the legacy handler; omit it (see migration/public-api-coverage.md)"
                    )
                }
            }
        }
    }
    if matches!(
        name,
        "create_game_center_leaderboard_set"
            | "create_game_center_activity"
            | "create_game_center_challenge"
    ) {
        let count = ["game_center_detail_id", "game_center_group_id"]
            .into_iter()
            .filter(|k| args.get(*k).is_some_and(nonempty))
            .count();
        if count != 1 {
            bail!("exactly one of game_center_detail_id or game_center_group_id is required")
        }
    }
    if name == "add_review_submission_item" {
        let keys = [
            "app_store_version_id",
            "app_custom_product_page_version_id",
            "app_store_version_experiment_id",
            "app_event_id",
            "game_center_achievement_version_id",
            "game_center_leaderboard_version_id",
            "game_center_leaderboard_set_version_id",
            "game_center_activity_version_id",
            "game_center_challenge_version_id",
            "in_app_purchase_version_id",
            "subscription_version_id",
            "subscription_group_version_id",
        ];
        if keys
            .into_iter()
            .filter(|k| args.get(*k).is_some_and(nonempty))
            .count()
            != 1
        {
            bail!("exactly one review submission item resource is required")
        }
    }
    if name == "list_app_store_version_experiments"
        && !["app_id", "version_id"]
            .iter()
            .any(|k| args.get(*k).is_some_and(nonempty))
    {
        bail!("app_id or version_id is required")
    }
    if name == "get_background_asset_version_release"
        && !matches!(
            args["release_type"].as_str(),
            Some("INTERNAL_BETA" | "EXTERNAL_BETA" | "APP_STORE")
        )
    {
        bail!("release_type must be INTERNAL_BETA, EXTERNAL_BETA, or APP_STORE")
    }
    if matches!(
        name,
        "set_app_price_schedule"
            | "set_in_app_purchase_price_schedule"
            | "create_in_app_purchase_offer_code"
    ) {
        for price in args["prices"]
            .as_array()
            .context("prices must be an array")?
        {
            if price["price_point_id"]
                .as_str()
                .is_none_or(|s| s.is_empty())
            {
                bail!("each price requires price_point_id")
            }
            if name == "create_in_app_purchase_offer_code"
                && price["territory_id"].as_str().is_none_or(|s| s.is_empty())
            {
                bail!("each price requires territory_id")
            }
        }
    }
    Ok(())
}

pub async fn call(client: &ApiClient, name: &str, args: &Value) -> Result<Value> {
    if is_compound(name) {
        bail!("{name} requires the asset/report transfer dispatcher")
    }
    let request = prepare_request(name, args)?;
    let path = if recipes()[name]["supports_cursor"] == true {
        args["cursor"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(&request.path)
    } else {
        &request.path
    };
    let query = if path == request.path {
        request.query
    } else {
        Vec::new()
    };
    let response = client
        .request(&request.method, path, query, request.body)
        .await?;
    if name == "submit_app_for_review" {
        let id = response["data"]["id"]
            .as_str()
            .context("review submission response is missing data.id")?;
        let item = json!({"data":{"type":"reviewSubmissionItems","relationships":{"reviewSubmission":{"data":{"type":"reviewSubmissions","id":id}},"appStoreVersion":{"data":{"type":"appStoreVersions","id":args["version_id"]}}}}});
        client
            .request("POST", "/v1/reviewSubmissionItems", vec![], Some(item))
            .await
            .with_context(|| format!("review submission {id} created; failed to attach version"))?;
        let body =
            json!({"data":{"type":"reviewSubmissions","id":id,"attributes":{"submitted":true}}});
        return client
            .request(
                "PATCH",
                &format!("/v1/reviewSubmissions/{}", resource_segment(id)?),
                vec![],
                Some(body),
            )
            .await
            .with_context(|| {
                format!("review submission {id} created and version attached; failed to submit")
            });
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_captured_requests_match_go() {
        let probes: Value =
            serde_json::from_str(include_str!("../data/go_request_probes.json")).unwrap();
        let mut covered = std::collections::HashSet::new();
        let mut failures = Vec::new();
        let mut count = 0;
        for (name, cases) in probes.as_object().unwrap() {
            for (label, case) in cases.as_object().unwrap() {
                let Some(expected) = case["requests"].as_array().and_then(|a| a.first()) else {
                    continue;
                };
                let actual = build_request(name, &case["args"]).unwrap();
                let mut query = Map::new();
                for (k, v) in actual.query {
                    query
                        .entry(k)
                        .or_insert_with(|| json!([]))
                        .as_array_mut()
                        .unwrap()
                        .push(json!(v));
                }
                let result = json!({"method":actual.method,"path":actual.path,"query":query,"body":actual.body});
                if result != *expected {
                    failures.push(format!(
                        "{name}/{label}\n expected {expected}\n actual   {result}"
                    ));
                }
                covered.insert(name);
                count += 1;
            }
        }
        assert_eq!(covered.len(), 356);
        assert!(count > 2700, "{count} request probes");
        assert!(
            failures.is_empty(),
            "{} failures:\n{}",
            failures.len(),
            failures.into_iter().take(45).collect::<Vec<_>>().join("\n")
        );
    }
}
