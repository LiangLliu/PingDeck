//! Argument validation against a tool's JSON Schema.
//!
//! Single responsibility: check the model's arguments object against the small schema subset the
//! SpanBay tools use (`type`, `enum`, `properties`, `required`, `additionalProperties: false`,
//! `items`, numeric and size bounds) and turn the first fault into a validation error that names
//! the argument. `null` members are dropped first, so an optional argument the model sends as
//! `null` counts as absent.

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::AppError;

/// Validates `arguments` against `schema` and deserializes them into `T`.
///
/// # Errors
/// [`AppError::Validation`] naming the faulty argument: `Unknown field "<name>".`, a missing
/// required argument, a wrong type, a value outside its bounds or enum.
pub(crate) fn parse_args<T: DeserializeOwned>(
    schema: &Value,
    arguments: &Value,
) -> Result<T, AppError> {
    let arguments = without_nulls(arguments);
    check(schema, &arguments, "")?;
    serde_json::from_value(arguments)
        .map_err(|error| AppError::validation(format!("invalid arguments: {error}")))
}

/// `value` with every `null` object member removed, at any depth.
fn without_nulls(value: &Value) -> Value {
    match value {
        Value::Object(members) => Value::Object(
            members
                .iter()
                .filter(|(_, member)| !member.is_null())
                .map(|(key, member)| (key.clone(), without_nulls(member)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_nulls).collect()),
        other => other.clone(),
    }
}

/// Names the argument at `path` in a message ("`draft.url`", or "the arguments" at the root).
fn label(path: &str) -> String {
    if path.is_empty() {
        "the arguments".to_owned()
    } else {
        format!("`{path}`")
    }
}

fn child(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn matches_type(expected: &str, value: &Value) -> bool {
    match expected {
        "number" => value.is_number(),
        other => type_name(value) == other,
    }
}

fn check(schema: &Value, value: &Value, path: &str) -> Result<(), AppError> {
    if let Some(expected) = schema.get("type") {
        let allowed: Vec<&str> = match expected {
            Value::String(name) => vec![name.as_str()],
            Value::Array(names) => names.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        if !allowed.is_empty() && !allowed.iter().any(|name| matches_type(name, value)) {
            return Err(AppError::validation(format!(
                "{} must be {}, not {}.",
                label(path),
                allowed.join(" or "),
                type_name(value)
            )));
        }
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array)
        && !options.contains(value)
    {
        let names: Vec<String> = options
            .iter()
            .map(|option| {
                option
                    .as_str()
                    .map_or_else(|| option.to_string(), str::to_owned)
            })
            .collect();
        return Err(AppError::validation(format!(
            "{} must be one of: {}.",
            label(path),
            names.join(", ")
        )));
    }
    match value {
        Value::Object(members) => check_object(schema, members, path),
        Value::Array(items) => check_array(schema, items, path),
        Value::String(text) => {
            if let Some(min) = schema.get("minLength").and_then(Value::as_u64)
                && (text.trim().chars().count() as u64) < min
            {
                return Err(AppError::validation(format!(
                    "{} must not be blank.",
                    label(path)
                )));
            }
            Ok(())
        }
        Value::Number(number) => check_number(schema, number, path),
        _ => Ok(()),
    }
}

fn check_object(
    schema: &Value,
    members: &serde_json::Map<String, Value>,
    path: &str,
) -> Result<(), AppError> {
    let properties = schema.get("properties").and_then(Value::as_object);
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            if !members.contains_key(name) {
                return Err(AppError::validation(format!(
                    "Missing required argument \"{}\".",
                    child(path, name)
                )));
            }
        }
    }
    let closed = schema.get("additionalProperties") == Some(&Value::Bool(false));
    for (key, member) in members {
        match properties.and_then(|properties| properties.get(key)) {
            Some(property) => check(property, member, &child(path, key))?,
            None if closed => {
                return Err(AppError::validation(format!(
                    "Unknown field \"{}\".",
                    child(path, key)
                )));
            }
            None => {}
        }
    }
    Ok(())
}

fn check_array(schema: &Value, items: &[Value], path: &str) -> Result<(), AppError> {
    let count = items.len() as u64;
    if let Some(min) = schema.get("minItems").and_then(Value::as_u64)
        && count < min
    {
        return Err(AppError::validation(format!(
            "{} needs at least {min} item{}.",
            label(path),
            if min == 1 { "" } else { "s" }
        )));
    }
    if let Some(max) = schema.get("maxItems").and_then(Value::as_u64)
        && count > max
    {
        return Err(AppError::validation(format!(
            "{} takes at most {max} items, got {count}.",
            label(path)
        )));
    }
    if let Some(item_schema) = schema.get("items") {
        for (index, item) in items.iter().enumerate() {
            check(item_schema, item, &format!("{path}[{index}]"))?;
        }
    }
    Ok(())
}

fn check_number(schema: &Value, number: &serde_json::Number, path: &str) -> Result<(), AppError> {
    let Some(actual) = number.as_f64() else {
        return Ok(());
    };
    if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
        && actual < min
    {
        return Err(AppError::validation(format!(
            "{} must be at least {min}, got {number}.",
            label(path)
        )));
    }
    if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
        && actual > max
    {
        return Err(AppError::validation(format!(
            "{} must be at most {max}, got {number}.",
            label(path)
        )));
    }
    Ok(())
}
