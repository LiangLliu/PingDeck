//! YAML reading for imports and skill frontmatter.
//!
//! Built on `serde-saphyr`, configured to read documents the way `serde_yaml` 0.9 did (YAML 1.2
//! core scalars, `<<` as a plain key, duplicate keys left to the target type). The tests pin that
//! reading; the few inputs that now read differently are pinned in `known_differences`.

use serde::de::DeserializeOwned;
use serde_saphyr::{DuplicateKeyPolicy, MergeKeyPolicy, NonFiniteFloatPolicy, Options};

/// A YAML syntax, budget or type error.
pub use serde_saphyr::Error;

/// Nesting limit, equal to `serde_yaml`'s recursion limit.
const MAX_DEPTH: usize = 128;

/// `untyped` reads into a JSON value: YAML 1.1 booleans (`yes`, `on`) stay strings and unknown
/// tags fail. A typed target only converts the fields it declares, so it tolerates both on the
/// fields it skips, as `serde_yaml` did.
fn options(untyped: bool) -> Options {
    serde_saphyr::options! {
        // No node or event cap (callers bound the input size) and no alias-to-anchor ratio: one
        // anchor may be reused any number of times. Alias floods still stop at the default
        // limit of 1,000,000 replayed events.
        budget: serde_saphyr::budget! {
            max_depth: MAX_DEPTH,
            max_events: usize::MAX,
            max_nodes: usize::MAX,
            enforce_alias_anchor_ratio: false,
        },
        // A JSON map keeps the last value; a derived struct rejects a repeated field.
        duplicate_keys: DuplicateKeyPolicy::PassThrough,
        merge_keys: MergeKeyPolicy::AsOrdinary,
        leading_zero_numbers_as_strings: true,
        underscored_numbers_as_strings: true,
        // `.inf` and `.nan` reach JSON as null.
        non_finite_float_policy: NonFiniteFloatPolicy::PassThrough,
        ignore_binary_tag_for_string: true,
        strict_booleans: untyped,
        reject_unsupported_tags: untyped,
        with_snippet: false,
    }
}

/// Parses one YAML document into an untyped JSON value.
///
/// # Errors
/// Invalid YAML, more than one document, an unknown tag, or nesting deeper than 128 levels.
pub fn to_json(text: &str) -> Result<serde_json::Value, Error> {
    serde_saphyr::from_str_with_options(text, options(true))
}

/// Parses one YAML document into `T`.
///
/// # Errors
/// Invalid YAML, more than one document, nesting deeper than 128 levels, or a shape `T` rejects.
pub fn from_str<T: DeserializeOwned>(text: &str) -> Result<T, Error> {
    serde_saphyr::from_str_with_options(text, options(false))
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde_json::{Value, json};

    use super::{from_str, to_json};

    #[derive(Debug, PartialEq, Deserialize)]
    struct Frontmatter {
        name: Option<String>,
        description: Option<String>,
    }

    fn fm(name: Option<&str>, description: Option<&str>) -> Option<Frontmatter> {
        Some(Frontmatter {
            name: name.map(str::to_owned),
            description: description.map(str::to_owned),
        })
    }

    fn nested(depth: usize) -> String {
        format!("{}{}", "[".repeat(depth), "]".repeat(depth))
    }

    // Unoptimised builds need about 20 KiB of stack per level (2 MiB holds ~83); release builds
    // hold ~570 levels in the 2 MiB of a tokio thread.
    fn on_big_stack<T: Send + 'static>(run: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(8 << 20)
            .spawn(run)
            .unwrap()
            .join()
            .unwrap()
    }

    // The reading serde_yaml 0.9.34 gave; `None` means an error.
    #[test]
    fn untyped_values_keep_the_serde_yaml_reading() {
        let cases: Vec<(&str, Option<Value>)> = vec![
            (
                "a: null\nb: ~\nc:\nd: Null\ne: NULL",
                Some(json!({"a": null, "b": null, "c": null, "d": null, "e": null})),
            ),
            (
                "a: ''\nb: \"\"\nc: 'null'\nd: \"true\"\ne: '123'",
                Some(json!({"a": "", "b": "", "c": "null", "d": "true", "e": "123"})),
            ),
            ("a: 1\nb: 2\na: 3", Some(json!({"a": 3, "b": 2}))),
            (
                "a: yes\nb: no\nc: on\nd: off\ne: y\nf: n\ng: Yes\nh: tRUE",
                Some(json!({
                    "a": "yes", "b": "no", "c": "on", "d": "off",
                    "e": "y", "f": "n", "g": "Yes", "h": "tRUE"
                })),
            ),
            (
                "a: true\nb: True\nc: TRUE\nd: false\ne: False\nf: FALSE",
                Some(json!({"a": true, "b": true, "c": true, "d": false, "e": false, "f": false})),
            ),
            (
                "a: 0o17\nb: 0x1F\nc: 0b101\nd: 017\ne: 08\nf: -007\ng: 00",
                Some(
                    json!({"a": 15, "b": 31, "c": 5, "d": "017", "e": "08", "f": "-007", "g": "00"}),
                ),
            ),
            (
                "a: 1_000\nb: 0x_1F\nc: 0_1",
                Some(json!({"a": "1_000", "b": "0x_1F", "c": "0_1"})),
            ),
            (
                "a: 1e3\nb: 1.5e-3\nc: 6.02e+23\nd: .5\ne: 1.\nf: -0\ng: +12\nh: 01e2\ni: 00.5",
                Some(json!({
                    "a": 1000.0, "b": 0.0015, "c": 6.02e23, "d": 0.5, "e": 1.0,
                    "f": 0, "g": 12, "h": 100.0, "i": 0.5
                })),
            ),
            (
                "a: .inf\nb: -.inf\nc: .nan\nd: +.inf\ne: .NaN",
                Some(json!({"a": null, "b": null, "c": null, "d": null, "e": null})),
            ),
            (
                "a: 2001-12-14t21:59:43.10-05:00\nb: 2002-12-14\nc: 12:30:45",
                Some(
                    json!({"a": "2001-12-14t21:59:43.10-05:00", "b": "2002-12-14", "c": "12:30:45"}),
                ),
            ),
            ("a: 1\n---\nb: 2", None),
            ("---\na: 1\n...\n", Some(json!({"a": 1}))),
            ("", Some(Value::Null)),
            ("# only a comment", Some(Value::Null)),
            ("plain string", Some(json!("plain string"))),
            ("- 1\n- two", Some(json!([1, "two"]))),
            (
                "base: &b\n  x: 1\nref: *b",
                Some(json!({"base": {"x": 1}, "ref": {"x": 1}})),
            ),
            (
                "base: &b\n  x: 1\nchild:\n  <<: *b\n  y: 2",
                Some(json!({"base": {"x": 1}, "child": {"<<": {"x": 1}, "y": 2}})),
            ),
            ("a: &x 1\nb: *missing", None),
            (
                "a: !!str 123\nb: !!int \"7\"\nc: !!float 1\nd: !!binary aGVsbG8=",
                Some(json!({"a": "123", "b": 7, "c": 1.0, "d": "aGVsbG8="})),
            ),
            ("a: !custom foo", None),
            (
                "1: a\ntrue: b\n1.5: c\n\"404\": d",
                Some(json!({"1": "a", "true": "b", "1.5": "c", "404": "d"})),
            ),
            ("? [a, b]\n: c", None),
            (
                "a: \"x\\ny\"\nb: |\n  block\n  text\nc: >\n  folded\n  text\nd: |-\n  kept\n",
                Some(json!({"a": "x\ny", "b": "block\ntext\n", "c": "folded text\n", "d": "kept"})),
            ),
            ("a: [unterminated", None),
            ("a: b: c", None),
        ];
        for (yaml, expected) in cases {
            assert_eq!(to_json(yaml).ok(), expected, "{yaml:?}");
        }
    }

    #[test]
    fn an_openapi_document_reads_as_before() {
        let spec = "openapi: 3.0.3\ninfo:\n  title: Pets\n  version: \"1.0\"\npaths:\n  /pets/{id}:\n    get:\n      parameters:\n        - {name: id, in: path, required: true, schema: {type: integer, format: int64}}\n        - name: flag\n          in: query\n          schema: {type: string, enum: [yes, no, on]}\n      responses:\n        \"200\":\n          content:\n            application/json:\n              schema: {$ref: '#/components/schemas/Pet'}\n        404: {description: missing}\ncomponents:\n  schemas:\n    Pet:\n      type: object\n      properties:\n        id: {type: integer, example: 017}\n        score: {type: number, example: 1.5e3}\n";
        let expected = json!({
            "openapi": "3.0.3",
            "info": {"title": "Pets", "version": "1.0"},
            "paths": {"/pets/{id}": {"get": {
                "parameters": [
                    {"name": "id", "in": "path", "required": true,
                     "schema": {"type": "integer", "format": "int64"}},
                    {"name": "flag", "in": "query",
                     "schema": {"type": "string", "enum": ["yes", "no", "on"]}}
                ],
                "responses": {
                    "200": {"content": {"application/json": {
                        "schema": {"$ref": "#/components/schemas/Pet"}}}},
                    "404": {"description": "missing"}
                }
            }}},
            "components": {"schemas": {"Pet": {"type": "object", "properties": {
                "id": {"type": "integer", "example": "017"},
                "score": {"type": "number", "example": 1500.0}
            }}}}
        });
        assert_eq!(to_json(spec).ok(), Some(expected));
    }

    #[test]
    fn typed_targets_keep_the_serde_yaml_reading() {
        let cases: Vec<(&str, Option<Frontmatter>)> = vec![
            (
                "name: pdf-tools\ndescription: >-\n  Fill and merge PDF forms.\n  Use for any .pdf file.\nlicense: Apache-2.0\nallowed-tools: [Read, Write]\nmetadata:\n  version: 1.2",
                fm(
                    Some("pdf-tools"),
                    Some("Fill and merge PDF forms. Use for any .pdf file."),
                ),
            ),
            ("name: x\nname: y", None),
            ("name: x\nother: 1\nother: 2", fm(Some("x"), None)),
            ("name: x\nmeta:\n  a: 1\n  a: 2", fm(Some("x"), None)),
            (
                "name: yes\ndescription: true",
                fm(Some("yes"), Some("true")),
            ),
            (
                "name: 0o17\ndescription: 1_000",
                fm(Some("0o17"), Some("1_000")),
            ),
            (
                "name: .inf\ndescription: 2001-12-14",
                fm(Some(".inf"), Some("2001-12-14")),
            ),
            ("name: ~\ndescription: 017", fm(None, Some("017"))),
            ("name: \"\"\ndescription: ''", fm(Some(""), Some(""))),
            ("name: 123\ndescription: 1e3", fm(Some("123"), Some("1e3"))),
            ("name: !custom foo", fm(Some("foo"), None)),
            (
                "name: x\nflag: !!bool \"yes\"\ntags: !custom [a]",
                fm(Some("x"), None),
            ),
            (
                "name: &n shared\ndescription: *n",
                fm(Some("shared"), Some("shared")),
            ),
            (
                "name: 'it''s'\ndescription: \"tab\\t\u{e9}\"",
                fm(Some("it's"), Some("tab\t\u{e9}")),
            ),
            ("name: [x]", None),
            ("description: {a: 1}", None),
            ("plain string", None),
            ("name: x\n---\nname: y", None),
        ];
        for (yaml, expected) in cases {
            assert_eq!(from_str::<Frontmatter>(yaml).ok(), expected, "{yaml:?}");
        }
    }

    #[test]
    fn nesting_stops_at_128_levels_and_only_alias_floods_are_refused() {
        let depths = on_big_stack(|| {
            (
                to_json(&nested(128)).is_ok(),
                to_json(&nested(129)).is_err(),
            )
        });
        assert_eq!(depths, (true, true));
        let mut flood = String::from("a: &a [x, x, x, x, x, x, x, x, x, x]");
        for (prev, name) in "abcdefgh".chars().zip("bcdefghi".chars()) {
            let refs = vec![format!("*{prev}"); 10].join(", ");
            flood.push_str(&format!("\n{name}: &{name} [{refs}]"));
        }
        assert!(to_json(&flood).is_err());
        let reused: String = (0..150).map(|n| format!("\nf{n}: *c")).collect();
        let value = to_json(&format!("c: &c {{type: string}}{reused}")).unwrap();
        assert_eq!(value["f149"], json!({"type": "string"}));
    }

    // Inputs serde-saphyr reads differently and no option restores; the old reading is noted.
    #[test]
    fn known_differences() {
        // Was {"null": "c"} and {"~": "c"}: a null key no longer becomes a string key.
        assert!(to_json("null: c").is_err());
        assert!(to_json("~: c").is_err());
        // Was an error: integers past u64/i64 now read as a float (as JSON does) or a string.
        assert_eq!(
            to_json("a: 99999999999999999999").ok(),
            Some(json!({"a": 1e20}))
        );
        assert_eq!(
            to_json("a: 0xFFFFFFFFFFFFFFFFFF").ok(),
            Some(json!({"a": "0xFFFFFFFFFFFFFFFFFF"}))
        );
        // Was an error: a tab before the first key.
        assert_eq!(to_json("\ta: b").ok(), Some(json!({"a": "b"})));
        // Was "x": a block scalar that ends the input without a line break gains one.
        assert_eq!(to_json("a: |\n  x").ok(), Some(json!({"a": "x\n"})));
        // Was ignored: a skipped field still counts toward the nesting limit.
        let skipped =
            on_big_stack(|| from_str::<Frontmatter>(&format!("x: {}", nested(129))).is_err());
        assert!(skipped);
    }
}
