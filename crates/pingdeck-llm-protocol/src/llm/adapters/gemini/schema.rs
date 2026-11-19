//! Gemini's OpenAPI-subset schemas (`responseSchema`, function `parameters`) as JSON Schema.

use serde_json::{Map, Value, json};

/// Converts a Gemini OpenAPI schema to JSON Schema.
///
/// Type names are lowercased, `nullable` becomes a `null` type (or `anyOf` branch), count limits
/// sent as strings become numbers, and `propertyOrdering`, `example` and `TYPE_UNSPECIFIED`
/// are dropped.
pub(super) fn schema_from_gemini(schema: &Value) -> Value {
    let Some(object) = schema.as_object() else {
        return schema.clone();
    };
    let mut out = Map::new();
    for (key, value) in object {
        let converted = match key.as_str() {
            "nullable" | "propertyOrdering" | "example" => continue,
            "type" => match value.as_str() {
                Some("TYPE_UNSPECIFIED") => continue,
                Some(name) => Value::String(name.to_ascii_lowercase()),
                None => value.clone(),
            },
            "items" => schema_from_gemini(value),
            "properties" => match value.as_object() {
                Some(properties) => Value::Object(
                    properties
                        .iter()
                        .map(|(name, property)| (name.clone(), schema_from_gemini(property)))
                        .collect(),
                ),
                None => value.clone(),
            },
            "anyOf" => match value.as_array() {
                Some(branches) => Value::Array(branches.iter().map(schema_from_gemini).collect()),
                None => value.clone(),
            },
            "minItems" | "maxItems" | "minProperties" | "maxProperties" | "minLength"
            | "maxLength" => value
                .as_str()
                .and_then(|text| text.parse::<u64>().ok())
                .map_or_else(|| value.clone(), Value::from),
            _ => value.clone(),
        };
        out.insert(key.clone(), converted);
    }
    if object.get("nullable").and_then(Value::as_bool) == Some(true) {
        make_nullable(&mut out);
    }
    Value::Object(out)
}

/// Lets a converted schema accept `null`: `type` becomes `[type, "null"]` (or `anyOf` gains a
/// null branch) and an `enum` gains `null`.
fn make_nullable(schema: &mut Map<String, Value>) {
    match schema.get_mut("type") {
        Some(Value::String(name)) => {
            let name = std::mem::take(name);
            schema.insert("type".into(), json!([name, "null"]));
        }
        Some(_) => return,
        None => match schema.get_mut("anyOf").and_then(Value::as_array_mut) {
            Some(branches) => branches.push(json!({"type": "null"})),
            None => return,
        },
    }
    if let Some(Value::Array(values)) = schema.get_mut("enum") {
        values.push(Value::Null);
    }
}
