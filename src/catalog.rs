//! Exact public catalogue exported offline from the previous Go registry.
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::sync::OnceLock;

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
            if name=="create_subscription_offer_code" {
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
        assert_eq!(tools().len(), 356);
        let names: std::collections::HashSet<_> = catalogue()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 356);
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
