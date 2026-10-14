//! Exact public catalogue exported offline from the previous Go registry.
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::sync::OnceLock;

/// Apple `AgeRatingDeclarationUpdateRequest` attributes added to the legacy
/// tool at runtime: (argument, Apple attribute, kind). Kinds are `bool`,
/// `url` or the name of an enum in [`age_rating_enum`]. A test checks this
/// table against the bundled OpenAPI schema. Deprecated `ageRatingOverride`
/// is deliberately omitted in favour of `ageRatingOverrideV2`.
pub const AGE_RATING_EXTRA: &[(&str, &str, &str)] = &[
    ("advertising", "advertising", "bool"),
    ("guns_or_other_weapons", "gunsOrOtherWeapons", "level"),
    (
        "health_or_wellness_topics",
        "healthOrWellnessTopics",
        "bool",
    ),
    ("kids_age_band", "kidsAgeBand", "kids"),
    ("loot_box", "lootBox", "bool"),
    ("messaging_and_chat", "messagingAndChat", "bool"),
    ("parental_controls", "parentalControls", "bool"),
    ("age_assurance", "ageAssurance", "bool"),
    ("social_media", "socialMedia", "bool"),
    (
        "social_media_age_restricted",
        "socialMediaAgeRestricted",
        "bool",
    ),
    ("user_generated_content", "userGeneratedContent", "bool"),
    ("age_rating_override_v2", "ageRatingOverrideV2", "override"),
    (
        "korea_age_rating_override",
        "koreaAgeRatingOverride",
        "korea",
    ),
    (
        "developer_age_rating_info_url",
        "developerAgeRatingInfoUrl",
        "url",
    ),
];
pub const AGE_RATING_LEVELS: &[&str] = &[
    "NONE",
    "INFREQUENT_OR_MILD",
    "FREQUENT_OR_INTENSE",
    "INFREQUENT",
    "FREQUENT",
];
/// Allowed values for a non-boolean kind in [`AGE_RATING_EXTRA`].
pub fn age_rating_enum(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "level" => AGE_RATING_LEVELS,
        "kids" => &["FIVE_AND_UNDER", "SIX_TO_EIGHT", "NINE_TO_ELEVEN"],
        "override" => &[
            "NONE",
            "NINE_PLUS",
            "THIRTEEN_PLUS",
            "SIXTEEN_PLUS",
            "EIGHTEEN_PLUS",
            "UNRATED",
        ],
        "korea" => &["NONE", "FIFTEEN_PLUS", "NINETEEN_PLUS"],
        _ => return None,
    })
}
pub fn tools() -> Vec<Value> {
    catalogue().clone()
}
pub fn catalogue() -> &'static Vec<Value> {
    static TOOLS: OnceLock<Vec<Value>> = OnceLock::new();
    TOOLS.get_or_init(|| {
        let mut tools:Vec<Value>=serde_json::from_str(include_str!("../data/public_tools.json")).expect("checked public tool catalogue");
        // Add only fields already decoded by legacy handlers, plus the
        // explicitly documented Apple-schema repair for subscription offers.
        let actual:Value=serde_json::from_str(include_str!("../data/handler_arguments.json")).expect("checked handler parameters");
        for tool in &mut tools {
            let name=tool["name"].as_str().unwrap().to_owned();
            let properties=tool["inputSchema"]["properties"].as_object_mut().unwrap();
            if let Some(fields)=actual[&name].as_object() {for (key,property) in fields {properties.entry(key.clone()).or_insert_with(||property.clone());}}
            if name=="get_beta_build_usage_metrics" {
                properties.get_mut("limit").unwrap()["description"]=json!("Omit limit. Apple only accepts it with grouping or filters, which this tool does not expose; positive values are rejected before making a request.");
            }
            if name=="update_age_rating_declaration" {
                let properties=tool["inputSchema"]["properties"].as_object_mut().unwrap();
                for (arg,apple,kind) in AGE_RATING_EXTRA {
                    let property=match age_rating_enum(kind) {
                        Some(values)=>json!({"type":"string","enum":values,"description":format!("Apple ageRatingDeclarations.{apple}: {}.",values.join(", "))}),
                        None if *kind=="url"=>json!({"type":"string","description":format!("Apple ageRatingDeclarations.{apple} (URL).")}),
                        None=>json!({"type":"boolean","description":format!("Apple ageRatingDeclarations.{apple}.")}),
                    };
                    properties.insert((*arg).into(),property);
                }
                for (key,property) in properties.iter_mut() {
                    if key!="declaration_id" && key!="developer_age_rating_info_url" && property["type"]=="string" && property.get("enum").is_none() {
                        property["enum"]=json!(AGE_RATING_LEVELS);
                    }
                }
                properties.get_mut("declaration_id").unwrap()["description"]=json!("The age rating declaration ID (get_age_rating_declaration returns it). Unknown arguments are rejected; null is rejected, omit a field to leave it unchanged. The App Privacy questionnaire has no public API and must be completed in App Store Connect.");
            }
            if name=="set_app_price_schedule" {
                tool["description"]=json!("Replace an app's price schedule. prices is required and must not be empty: each entry has price_point_id (from list_app_price_points) and optional start_date and end_date (YYYY-MM-DD). To make an app free, pass the app price point whose customerPrice is 0 for base_territory_id; an empty prices array is rejected. Prices in territories you do not list are derived automatically from the base territory. Verify with get_app_price_schedule and list_app_price_schedule_prices.");
                tool["inputSchema"]["properties"]["prices"]["description"]=json!("Manual prices (at least one), each an object with price_point_id and optional start_date and end_date (YYYY-MM-DD), e.g. [{\"price_point_id\": \"eyJz...\", \"start_date\": \"2026-01-01\"}]. For a free app use the zero-price point.");
                tool["inputSchema"]["properties"]["prices"]["minItems"]=json!(1);
                let required=tool["inputSchema"]["required"].as_array_mut().unwrap();
                required.push(json!("prices"));
            }
            if name=="list_app_store_review_attachments" {
                let properties=tool["inputSchema"]["properties"].as_object_mut().unwrap();
                properties.insert("review_detail_id".into(),json!({"type":"string","description":"The appStoreReviewDetails ID (from the App Store version's review detail). Provide this or the legacy version_id."}));
                properties.get_mut("version_id").unwrap()["description"]=json!("Legacy alias of review_detail_id: despite its name this must be an appStoreReviewDetails ID, not an App Store version ID. Conflicting values are rejected.");
                tool["description"]=json!("List App Store review attachments of an App Store review detail");
                // Either ID is accepted; prepare_request enforces that one is present.
                tool["inputSchema"]["required"]=json!([]);
            }
            if name=="create_subscription_offer_code" {
                let properties=tool["inputSchema"]["properties"].as_object_mut().unwrap();
                properties.insert("duration".into(),json!({"type":"string","enum":["THREE_DAYS","ONE_WEEK","TWO_WEEKS","ONE_MONTH","TWO_MONTHS","THREE_MONTHS","SIX_MONTHS","ONE_YEAR"],"description":"Apple SubscriptionOfferDuration, required for creation."}));
                properties.insert("customer_eligibilities".into(),json!({"type":"array","items":{"type":"string"},"description":"NEW, EXISTING, or EXPIRED. Alternative to the legacy singular customer_eligibility."}));
                properties.insert("offer_eligibility".into(),json!({"type":"string","enum":["STACK_WITH_INTRO_OFFERS","REPLACE_INTRO_OFFERS"],"description":"Alternative to legacy offer_eligibility_type."}));
                properties.insert("price_ids".into(),json!({"type":"array","items":{"type":"string"},"description":"Existing subscriptionOfferCodePrices identifiers. Provide exactly one of price_ids or prices."}));
                properties.insert("prices".into(),json!({"type":"array","items":{"type":"object"},"description":"Inline offer prices, each with territory_id and price_point_id (a subscriptionPricePoints ID). Provide exactly one of prices or price_ids."}));
                let required=tool["inputSchema"]["required"].as_array_mut().unwrap();
                required.retain(|key|key!="customer_eligibility");
                for key in ["duration","offer_mode","number_of_periods"] {if !required.iter().any(|v|v==key) {required.push(json!(key));}}
            }
        }
        // Native additions have Apple OpenAPI provenance, not legacy Go probes.
        let additions: Vec<Value> = serde_json::from_str(include_str!("../data/native_tools.json"))
            .expect("checked native tool catalogue");
        tools.extend(additions);
        tools
    })
}
pub fn find(name: &str) -> Option<&'static Value> {
    catalogue().iter().find(|t| t["name"] == name)
}
/// Match Go JSON decoding and required argument contracts. Unknown keys remain
/// allowed, as in the original Go anonymous parameter structs.
pub fn validate(name: &str, args: &Value) -> Result<()> {
    let tool = find(name).ok_or_else(|| anyhow::anyhow!("unknown tool: {name}"))?;
    let object = args
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("arguments must be an object"))?;
    let schema = &tool["inputSchema"];
    if let Some(required) = schema["required"].as_array() {
        for key in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(key)
                || object[key].is_null()
                || object[key].as_str() == Some("")
            {
                bail!("{key} is required");
            }
        }
    }
    if let Some(properties) = schema["properties"].as_object() {
        for (key, property) in properties {
            let Some(value) = object.get(key) else {
                continue;
            };
            // JSON null is accepted by Go decoding, leaving value defaults or
            // nil pointers. The request compiler preserves that behavior.
            if value.is_null() {
                continue;
            }
            let valid = match property["type"].as_str() {
                Some("string") => value.is_string(),
                Some("boolean") => value.is_boolean(),
                Some("integer") => value.is_i64() || value.is_u64(),
                Some("number") => value.is_number(),
                Some("array") => value.is_array(),
                Some("object") => value.is_object(),
                _ => true,
            };
            if !valid {
                bail!("invalid argument type for {key}");
            }
            if property["items"]["type"] == "string"
                && value
                    .as_array()
                    .is_some_and(|a| a.iter().any(|x| !x.is_string()))
            {
                bail!("{key} must contain strings");
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_exported_catalogue() {
        assert_eq!(tools().len(), 365);
        let names: std::collections::HashSet<_> = catalogue()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 365);
        assert!(
            find("list_apps").unwrap()["annotations"]["readOnlyHint"]
                .as_bool()
                .unwrap()
        );
    }
    #[test]
    fn argument_contracts() {
        assert!(validate("get_app", &serde_json::json!({"app_id":"1"})).is_ok());
        assert!(validate("get_app", &serde_json::json!({"app_id":1})).is_err());
        assert!(validate("get_app", &serde_json::json!({})).is_err());
        assert!(validate("list_apps", &serde_json::json!({"limit":1.5})).is_err());
    }
}
