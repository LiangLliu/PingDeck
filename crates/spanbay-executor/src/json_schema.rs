//! JSON Schema validation for the `AiJsonSchemaValid` and `JsonSchemaValid` assertions and the
//! scripts' `pm.response.to.have.jsonSchema`, backed by `jsonschema`.
//!
//! The validator is offline: a `$ref` resolves only inside the schema itself, never over HTTP or
//! from a file. Formats are asserted and `pattern` uses the linear-time `regex` engine. Unknown
//! keywords (usually typos such as `requried`) fail before validation instead of being ignored.

use jsonschema::paths::LocationSegment;
use jsonschema::{PatternOptions, Validator};
use serde_json::Value;

/// Keywords of the Draft 2020-12 vocabularies plus `definitions`, which older schemas still use as
/// a `$ref` target.
const KNOWN_KEYWORDS: &[&str] = &[
    "$anchor",
    "$comment",
    "$defs",
    "$dynamicAnchor",
    "$dynamicRef",
    "$id",
    "$ref",
    "$schema",
    "$vocabulary",
    "additionalProperties",
    "allOf",
    "anyOf",
    "const",
    "contains",
    "contentEncoding",
    "contentMediaType",
    "contentSchema",
    "default",
    "definitions",
    "dependentRequired",
    "dependentSchemas",
    "deprecated",
    "description",
    "else",
    "enum",
    "examples",
    "exclusiveMaximum",
    "exclusiveMinimum",
    "format",
    "if",
    "items",
    "maxContains",
    "maxItems",
    "maxLength",
    "maxProperties",
    "maximum",
    "minContains",
    "minItems",
    "minLength",
    "minProperties",
    "minimum",
    "multipleOf",
    "not",
    "oneOf",
    "pattern",
    "patternProperties",
    "prefixItems",
    "properties",
    "propertyNames",
    "readOnly",
    "required",
    "then",
    "title",
    "type",
    "unevaluatedItems",
    "unevaluatedProperties",
    "uniqueItems",
    "writeOnly",
];

/// Annotation keywords an OpenAPI schema object adds to JSON Schema. A response contract copied
/// from an OpenAPI document carries them; they describe, they do not constrain (`nullable` is
/// not enforced: write `"type": ["string", "null"]` for that).
const OPENAPI_KEYWORDS: &[&str] = &[
    "discriminator",
    "example",
    "externalDocs",
    "nullable",
    "xml",
];

/// Which keywords besides the JSON Schema vocabularies a schema may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    /// JSON Schema only: a schema written for a model reply.
    Strict,
    /// JSON Schema plus OpenAPI annotations and `x-` extensions: a response contract.
    Document,
}

/// Validates `instance` against `schema`; the error names the location (`$.a[0].b`) and the first
/// violated rule.
pub fn validate(schema: &Value, instance: &Value) -> Result<(), String> {
    validate_in(schema, instance, Dialect::Strict)
}

/// [`validate`] for a response contract: OpenAPI annotation keywords and `x-` extensions are
/// accepted, every other unknown keyword still fails.
pub fn validate_document(schema: &Value, instance: &Value) -> Result<(), String> {
    validate_in(schema, instance, Dialect::Document)
}

fn validate_in(schema: &Value, instance: &Value, dialect: Dialect) -> Result<(), String> {
    check_keywords(schema, dialect)?;
    let validator = build(schema)?;
    match validator.iter_errors(instance).next() {
        None => Ok(()),
        Some(error) => Err(format!("{}: {error}", location(error.instance_path()))),
    }
}

fn build(schema: &Value) -> Result<Validator, String> {
    jsonschema::options()
        .offline()
        .should_validate_formats(true)
        .with_pattern_options(PatternOptions::regex())
        .build(schema)
        .map_err(|error| format!("Invalid schema: {error}"))
}

/// Rejects unknown keywords in `schema` and every sub-schema; `enum`, `const`, `default` and
/// `examples` hold data, not schemas, so their contents are never inspected.
fn check_keywords(schema: &Value, dialect: Dialect) -> Result<(), String> {
    let Value::Object(object) = schema else {
        return Ok(());
    };
    let check = |value: &Value| check_keywords(value, dialect);
    for (keyword, value) in object {
        let annotation = dialect == Dialect::Document
            && (OPENAPI_KEYWORDS.contains(&keyword.as_str()) || keyword.starts_with("x-"));
        if annotation {
            continue;
        }
        if !KNOWN_KEYWORDS.contains(&keyword.as_str()) {
            return Err(format!("Unknown schema keyword `{keyword}`"));
        }
        match keyword.as_str() {
            "$defs" | "definitions" | "dependentSchemas" | "patternProperties" | "properties" => {
                if let Value::Object(members) = value {
                    members.values().try_for_each(check)?;
                }
            }
            "allOf" | "anyOf" | "oneOf" | "prefixItems" => {
                if let Value::Array(members) = value {
                    members.iter().try_for_each(check)?;
                }
            }
            "additionalProperties"
            | "contains"
            | "contentSchema"
            | "else"
            | "if"
            | "items"
            | "not"
            | "propertyNames"
            | "then"
            | "unevaluatedItems"
            | "unevaluatedProperties" => {
                check(value)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Renders a JSON pointer location as `$.name[0]`.
fn location(path: &jsonschema::paths::Location) -> String {
    let mut out = String::from("$");
    for segment in path {
        match segment {
            LocationSegment::Property(name) => {
                out.push('.');
                out.push_str(&name);
            }
            LocationSegment::Index(index) => out.push_str(&format!("[{index}]")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{validate, validate_document};

    fn fails(schema: Value, instance: Value, needle: &str) {
        let error = validate(&schema, &instance).expect_err("must fail");
        assert!(error.contains(needle), "{error:?} lacks {needle:?}");
    }

    #[test]
    fn violations_name_the_location() {
        let schema = json!({
            "type": "object",
            "required": ["name", "tags"],
            "properties": {
                "name": {"type": "string", "minLength": 1},
                "tags": {"type": "array", "items": {"type": "string"}},
                "id": {"type": "integer"}
            },
            "additionalProperties": false
        });
        assert_eq!(
            validate(&schema, &json!({"name": "a", "tags": ["x"], "id": 3.0})),
            Ok(())
        );
        fails(
            schema.clone(),
            json!({"name": "a"}),
            "$: \"tags\" is a required",
        );
        fails(
            schema.clone(),
            json!({"name": "a", "tags": ["x", 2]}),
            "$.tags[1]: 2 is not",
        );
        fails(
            schema.clone(),
            json!({"name": "a", "tags": [], "z": 1}),
            "'z' was unexpected",
        );
        fails(
            schema,
            json!({"name": "a", "tags": [], "id": 1.5}),
            "$.id: 1.5 is not",
        );
    }

    #[test]
    fn unknown_keywords_fail_at_any_depth_but_data_is_not_inspected() {
        fails(
            json!({"requried": ["a"]}),
            json!({}),
            "Unknown schema keyword `requried`",
        );
        fails(
            json!({"anyOf": [{"type": "string"}, {"items": {"maxItem": 1}}]}),
            json!("x"),
            "Unknown schema keyword `maxItem`",
        );
        let data = json!({"enum": [{"bogus": 1}], "default": {"bogus": 2}});
        assert_eq!(validate(&data, &json!({"bogus": 1})), Ok(()));
    }

    #[test]
    fn a_document_schema_accepts_openapi_annotations_and_extensions_only() {
        let schema = json!({
            "type": "object",
            "x-internal": true,
            "properties": {"id": {"type": "integer", "example": 7, "nullable": false}},
            "discriminator": {"propertyName": "id"}
        });
        assert_eq!(validate_document(&schema, &json!({"id": 1})), Ok(()));
        let error = validate_document(&schema, &json!({"id": "x"})).expect_err("must fail");
        assert!(error.contains("$.id"), "{error}");
        assert!(validate(&schema, &json!({"id": 1})).is_err());
        let typo = validate_document(&json!({"requried": ["id"]}), &json!({}));
        assert!(typo.is_err_and(|error| error.contains("requried")));
    }

    #[test]
    fn local_refs_resolve_and_remote_refs_are_refused() {
        let schema = json!({"$defs": {"id": {"type": "integer"}}, "items": {"$ref": "#/$defs/id"}});
        assert_eq!(validate(&schema, &json!([1, 2])), Ok(()));
        fails(schema, json!([1, "x"]), "$[1]:");
        for target in ["http://127.0.0.1:9/schema.json", "file:///etc/hosts"] {
            fails(json!({"$ref": target}), json!(1), "Invalid schema");
        }
    }

    #[test]
    fn malformed_schemas_and_formats_fail() {
        fails(json!({"type": "wibble"}), json!(1), "Invalid schema");
        fails(json!({"minLength": -1}), json!("a"), "Invalid schema");
        fails(
            json!({"format": "email"}),
            json!("not-an-email"),
            "is not a \"email\"",
        );
    }
}
