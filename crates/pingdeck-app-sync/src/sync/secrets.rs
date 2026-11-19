//! Secret values in sync documents: finding them, masking them on export, refilling on import.
//!
//! A secret slot is a string value (or a part of one) inside a document body that must not leave
//! the machine unless the user opted in. Export replaces it with the literal placeholder
//! `{{secret:KEY}}`; import swaps a placeholder for the local value with the same `KEY` in the
//! same entity. `KEY` is the variable key for environments, globals and collections,
//! `mcp.<server>.env.<NAME>` / `mcp.<server>.header.<NAME>` for MCP servers,
//! `llm.<provider id>.key.<key id>` for provider keys and `llm.<provider id>.header.<NAME>` for
//! provider extra headers, `ai.search.tavily` / `ai.search.brave` for the search keys (written as
//! values only when the user turned on "Include secrets in exports"). Gateway client tokens and
//! TLS passphrases are never exported. [`SLOT_TABLE`] lists every slot; export placeholders use
//! only slots in it.
//!
//! URLs (provider `base_url` / `proxy_url`, MCP `url`) keep scheme, host and path; their userinfo
//! becomes `{{secret:KEY.userinfo}}` and their query `{{secret:KEY.query}}`. The proxy URL and
//! the SearXNG address of the settings are local-only and never exported. MCP stdio `args` entries like `--token=VALUE`, and the argument
//! after a bare `--token`, become `{{secret:mcp.<server>.arg.--token}}` when the flag name holds
//! `token`, `key`, `secret`, `password` or `auth`.
//!
//! Request credentials: the positions come from `pingdeck_model::credentials`, the one rule set
//! copied code and Postman export share. A literal (no `{{variable}}` inside) in the `*_ref`
//! fields of a collection, folder or request `auth` becomes `{{secret:<scope>.<id>.auth.<field>}}`;
//! a credential header row (the names `is_secret_header_name` accepts) becomes
//! `{{secret:request.<id>.header.<name>}}`, a credential-named query row or form field
//! `{{secret:request.<id>.query.<name>}}` / `{{secret:request.<id>.form.<name>}}`, and the URL's
//! userinfo and credential-named query values `{{secret:request.<id>.url.userinfo}}` /
//! `{{secret:request.<id>.url.query.<name>}}`. A mock's proxy request headers and callback headers
//! likewise (`mock.<id>.proxy_header.<name>`, `mock.<id>.callback.<id>.header.<name>`). A flow's
//! inline steps take the same rules under `flow.<id>.step.<step id>.{url,header,query}`, a
//! credential-named line of a form-urlencoded body override becomes
//! `{{secret:flow.<id>.step.<step id>.form.<name>}}`, and a credential-named run input becomes
//! `{{secret:flow.<id>.var.<name>}}`.
//!
//! A placeholder resolves on import only inside the slot whose own key it names (a URL slot also
//! takes its `.userinfo` and `.query` parts): a file cannot carry a local secret into another
//! field.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use pingdeck_model::credentials::{
    AuthSecret, CredentialPosition, UrlPart, credential_positions, flow_step_credential_positions,
    form_line_credentials, is_credential_key, url_credentials,
};
use serde_json::Value;

use crate::workspace::FlowStep;
use crate::workspace::models::Request;

use super::SyncKind;

/// One row of the slot mapping table: which stored secret a placeholder slot stands for.
/// `<…>` in a template matches one or more characters.
pub(crate) struct SlotMapping {
    /// What the secret belongs to (documentation of the table).
    #[allow(dead_code, reason = "documents the table; the tests print it")]
    pub(crate) entity: &'static str,
    /// Where it lives in the secret store, or in the URL it was cut out of (documentation).
    #[allow(dead_code, reason = "documents the table; the tests print it")]
    pub(crate) secret_ref: &'static str,
    /// The placeholder key in an export file.
    pub(crate) slot: &'static str,
    /// The document kinds that carry the slot.
    pub(crate) kinds: &'static [SyncKind],
}

/// The one mapping table from each stored secret to its sync placeholder slot. Export
/// placeholders use only these slots; the test of every export fixture enforces it.
pub(crate) const SLOT_TABLE: &[SlotMapping] = &[
    SlotMapping {
        entity: "environment, global or collection secret variable",
        secret_ref: "env.<environment id>.<key> | global.<workspace id>.<key> | collection.<collection id>.<key>",
        slot: "<key>",
        kinds: &[
            SyncKind::Environment,
            SyncKind::Globals,
            SyncKind::Collection,
        ],
    },
    SlotMapping {
        entity: "provider key",
        secret_ref: "llm.key.<key id>",
        slot: "llm.<provider id>.key.<key id>",
        kinds: &[SyncKind::LlmProvider],
    },
    SlotMapping {
        entity: "provider extra header",
        secret_ref: "llm.<provider id>.header.<name>",
        slot: "llm.<provider id>.header.<name>",
        kinds: &[SyncKind::LlmProvider],
    },
    SlotMapping {
        entity: "provider base URL user info or query",
        secret_ref: "(part of the stored URL)",
        slot: "llm.<provider id>.base_url.<userinfo|query>",
        kinds: &[SyncKind::LlmProvider],
    },
    SlotMapping {
        entity: "provider proxy URL user info or query",
        secret_ref: "(part of the stored URL)",
        slot: "llm.<provider id>.proxy_url.<userinfo|query>",
        kinds: &[SyncKind::LlmProvider],
    },
    SlotMapping {
        entity: "MCP environment value",
        secret_ref: "mcp.<server id>.env.<name>",
        slot: "mcp.<server name>.env.<name>",
        kinds: &[SyncKind::McpServer],
    },
    SlotMapping {
        entity: "MCP header value",
        secret_ref: "mcp.<server id>.header.<name>",
        slot: "mcp.<server name>.header.<name>",
        kinds: &[SyncKind::McpServer],
    },
    SlotMapping {
        entity: "MCP argument value",
        secret_ref: "mcp.<server id>.arg.<flag>",
        slot: "mcp.<server name>.arg.<flag>",
        kinds: &[SyncKind::McpServer],
    },
    SlotMapping {
        entity: "MCP URL user info or query",
        secret_ref: "(part of the stored URL)",
        slot: "mcp.<server name>.url.<userinfo|query>",
        kinds: &[SyncKind::McpServer],
    },
    SlotMapping {
        entity: "request, folder or collection auth credential (a literal, not a {{variable}})",
        secret_ref: "(the `*_ref` field of the auth config)",
        slot: "<scope>.<id>.auth.<field>",
        kinds: &[SyncKind::Collection, SyncKind::LooseRequests],
    },
    SlotMapping {
        entity: "request credential header (Authorization, Cookie, X-Api-Key, …)",
        secret_ref: "(the header row value)",
        slot: "request.<request id>.header.<name>",
        kinds: &[SyncKind::Collection, SyncKind::LooseRequests],
    },
    SlotMapping {
        entity: "request credential query row (key, token, client_secret, …)",
        secret_ref: "(the query row value)",
        slot: "request.<request id>.query.<name>",
        kinds: &[SyncKind::Collection, SyncKind::LooseRequests],
    },
    SlotMapping {
        entity: "request credential form field (urlencoded or multipart text)",
        secret_ref: "(the field value)",
        slot: "request.<request id>.form.<name>",
        kinds: &[SyncKind::Collection, SyncKind::LooseRequests],
    },
    SlotMapping {
        entity: "request URL user info",
        secret_ref: "(part of the request URL)",
        slot: "request.<request id>.url.userinfo",
        kinds: &[SyncKind::Collection, SyncKind::LooseRequests],
    },
    SlotMapping {
        entity: "request URL credential query value",
        secret_ref: "(part of the request URL)",
        slot: "request.<request id>.url.query.<name>",
        kinds: &[SyncKind::Collection, SyncKind::LooseRequests],
    },
    SlotMapping {
        entity: "flow step credential header",
        secret_ref: "(the header override row value)",
        slot: "flow.<flow id>.step.<step id>.header.<name>",
        kinds: &[SyncKind::Flow],
    },
    SlotMapping {
        entity: "flow step credential query row",
        secret_ref: "(the query override row value)",
        slot: "flow.<flow id>.step.<step id>.query.<name>",
        kinds: &[SyncKind::Flow],
    },
    SlotMapping {
        entity: "flow step credential form line (form-urlencoded body override)",
        secret_ref: "(part of the body override text)",
        slot: "flow.<flow id>.step.<step id>.form.<name>",
        kinds: &[SyncKind::Flow],
    },
    SlotMapping {
        entity: "flow step URL user info",
        secret_ref: "(part of the URL override)",
        slot: "flow.<flow id>.step.<step id>.url.userinfo",
        kinds: &[SyncKind::Flow],
    },
    SlotMapping {
        entity: "flow step URL credential query value",
        secret_ref: "(part of the URL override)",
        slot: "flow.<flow id>.step.<step id>.url.query.<name>",
        kinds: &[SyncKind::Flow],
    },
    SlotMapping {
        entity: "flow credential-named run input",
        secret_ref: "(the initial variable value)",
        slot: "flow.<flow id>.var.<name>",
        kinds: &[SyncKind::Flow],
    },
    SlotMapping {
        entity: "mock proxy request credential header",
        secret_ref: "(the header row value)",
        slot: "mock.<server id>.proxy_header.<name>",
        kinds: &[SyncKind::Mock],
    },
    SlotMapping {
        entity: "mock callback credential header",
        secret_ref: "(the header row value)",
        slot: "mock.<server id>.callback.<callback id>.header.<name>",
        kinds: &[SyncKind::Mock],
    },
    SlotMapping {
        entity: "Tavily search key",
        secret_ref: "ai.search.tavily",
        slot: "ai.search.tavily",
        kinds: &[SyncKind::Settings],
    },
    SlotMapping {
        entity: "Brave search key",
        secret_ref: "ai.search.brave",
        slot: "ai.search.brave",
        kinds: &[SyncKind::Settings],
    },
];

/// Whether `text` fits `template`: `<…>` stands for one or more characters, everything else is
/// literal.
fn fits_template(template: &str, text: &str) -> bool {
    let mut literals: Vec<&str> = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        literals.push(&rest[..open]);
        rest = &rest[open + close + 1..];
    }
    literals.push(rest);
    let Some((first, tail)) = literals.split_first() else {
        return false;
    };
    let Some(mut remaining) = text.strip_prefix(first) else {
        return false;
    };
    let Some((last, middle)) = tail.split_last() else {
        return remaining.is_empty();
    };
    for piece in middle {
        let Some(first_char) = remaining.chars().next() else {
            return false;
        };
        let start = first_char.len_utf8();
        let Some(at) = remaining[start..].find(piece) else {
            return false;
        };
        remaining = &remaining[start + at + piece.len()..];
    }
    remaining.chars().count() > last.chars().count() && remaining.ends_with(last)
}

/// Whether a placeholder `slot` of a `kind` document is in [`SLOT_TABLE`].
pub(crate) fn slot_in_table(kind: SyncKind, slot: &str) -> bool {
    SLOT_TABLE.iter().any(|row| {
        row.kinds.contains(&kind)
            && match row.slot {
                // A variable's slot is its key, whatever that is.
                "<key>" => !slot.is_empty(),
                template if template.contains("<userinfo|query>") => ["userinfo", "query"]
                    .iter()
                    .any(|part| fits_template(&template.replace("<userinfo|query>", part), slot)),
                template => fits_template(template, slot),
            }
    })
}

/// Every `{{secret:KEY}}` placeholder inside the strings of `value`.
fn placeholders_in(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            let mut rest = text.as_str();
            while let Some(start) = rest.find("{{secret:") {
                let after = &rest[start + "{{secret:".len()..];
                let Some(end) = after.find("}}") else { break };
                out.push(after[..end].to_owned());
                rest = &after[end + 2..];
            }
        }
        Value::Array(items) => items.iter().for_each(|item| placeholders_in(item, out)),
        Value::Object(fields) => fields
            .values()
            .for_each(|field| placeholders_in(field, out)),
        _ => {}
    }
}

/// Fails with the first placeholder of a masked `kind` document that is not in [`SLOT_TABLE`]:
/// an export never writes a slot the table does not list.
pub(super) fn check_slots(kind: SyncKind, body: &Value) -> Result<(), String> {
    let mut found = Vec::new();
    placeholders_in(body, &mut found);
    found
        .into_iter()
        .find(|slot| !slot_in_table(kind, slot))
        .map_or(Ok(()), |slot| {
            Err(format!(
                "placeholder {{{{secret:{slot}}}}} is not in the slot mapping table"
            ))
        })
}

/// The `(tavily, brave)` values of the `search` object of an `ai` settings document body; empty
/// strings when absent.
pub(crate) fn search_keys(body: &Value) -> (String, String) {
    let text = |field: &str| {
        body.pointer(&format!("/search/{field}"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    (text("tavily_key"), text("brave_key"))
}

/// `body` without the search keys it carries (they are not settings).
pub(crate) fn without_search_keys(body: &Value) -> Value {
    let mut body = body.clone();
    if let Some(Value::Object(search)) = body.get_mut("search") {
        search.remove("tavily_key");
        search.remove("brave_key");
    }
    body
}

/// The placeholder standing in for the secret `key`.
pub(super) fn placeholder(key: &str) -> String {
    format!("{{{{secret:{key}}}}}")
}

/// Which part of a string value is secret.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    /// The whole value.
    Whole,
    /// The whole value, unless it holds a `{{variable}}` reference (then it is no literal secret).
    Literal,
    /// The userinfo and the query of a URL.
    Url,
    /// The literal userinfo and the literal credential-named query values of a request URL
    /// (`url_credentials`).
    RequestUrl,
    /// What follows the first `=` (a `--flag=value` argument).
    AfterEquals,
    /// The literal values of the credential-named `key=value` lines of a form text
    /// (`form_line_credentials`).
    FormLines,
}

type Slots<'a> = Vec<(String, Part, &'a mut Value)>;

/// The string at the JSON pointer `/path` of `value`, empty when absent.
fn text_field(value: &Value, path: &str) -> String {
    value
        .pointer(&format!("/{path}"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Credential header rows of the `headers` array (`{key, value, enabled}`) under `prefix`; a
/// value that is a `{{variable}}` reference stays readable. shortcut: rows that repeat a header
/// name share one slot key, upgrade when a real file needs two different secret rows of one name.
fn header_slots<'a>(prefix: &str, headers: &'a mut Value, out: &mut Slots<'a>) {
    let Value::Array(rows) = headers else { return };
    for row in rows {
        let name = text_field(row, "key");
        if pingdeck_base::secrets::is_secret_header_name(&name)
            && let Some(value) = row.get_mut("value")
            && value.is_string()
        {
            out.push((format!("{prefix}.{name}"), Part::Literal, value));
        }
    }
}

/// Credential fields of an `auth` value (`{"Bearer": {"token_ref": …}}`) of the entity `scope`:
/// the `*_ref` fields hold whatever the user typed, a literal token or password as well as a
/// `{{variable}}` reference.
fn auth_slots<'a>(scope: &str, auth: &'a mut Value, out: &mut Slots<'a>) {
    let Value::Object(variant) = auth else { return };
    for fields in variant.values_mut() {
        let Value::Object(fields) = fields else {
            continue;
        };
        for (field, value) in fields.iter_mut() {
            if AuthSecret::ALL
                .iter()
                .any(|secret| secret.field() == field.as_str())
                && value.is_string()
            {
                out.push((format!("{scope}.auth.{field}"), Part::Literal, value));
            }
        }
    }
}

/// The value of each row of the `rows` array at an index in `wanted`, keyed `<prefix>.<row key>`.
fn indexed_rows<'a>(
    rows: &'a mut Value,
    wanted: &HashSet<usize>,
    prefix: &str,
    out: &mut Slots<'a>,
) {
    let Value::Array(rows) = rows else { return };
    for (index, row) in rows.iter_mut().enumerate() {
        if !wanted.contains(&index) {
            continue;
        }
        let name = text_field(row, "key");
        if let Some(value) = row.get_mut("value")
            && value.is_string()
        {
            out.push((format!("{prefix}.{name}"), Part::Literal, value));
        }
    }
}

/// Row indexes of the non-URL positions, by kind: query rows, header rows, form fields.
#[derive(Default)]
struct Rows {
    url: bool,
    form_lines: bool,
    query: HashSet<usize>,
    header: HashSet<usize>,
    form: HashSet<usize>,
}

impl Rows {
    fn of(positions: Vec<CredentialPosition>) -> Self {
        let mut rows = Self::default();
        for position in positions {
            match position {
                CredentialPosition::Url(_) => rows.url = true,
                CredentialPosition::FormLine(_) => rows.form_lines = true,
                CredentialPosition::QueryParam(index) => {
                    rows.query.insert(index);
                }
                CredentialPosition::Header(index) => {
                    rows.header.insert(index);
                }
                CredentialPosition::FormField(index)
                | CredentialPosition::MultipartField(index) => {
                    rows.form.insert(index);
                }
                // Auth leaves are walked by field name (`auth_slots`), as for folders and
                // collections.
                CredentialPosition::Auth(_) => {}
            }
        }
        rows
    }
}

/// Credential slots of one request, at the positions `credential_positions` names. A request
/// body that does not parse as a request (a hand-edited file) still has its auth and its
/// credential headers found by name.
fn request_slots<'a>(request: &'a mut Value, out: &mut Slots<'a>) {
    let id = text_field(request, "id");
    let rows = serde_json::from_value::<Request>(request.clone())
        .ok()
        .map(|typed| Rows::of(credential_positions(&typed)));
    let Value::Object(fields) = request else {
        return;
    };
    for (field, value) in fields.iter_mut() {
        match (field.as_str(), &rows) {
            ("auth", _) => auth_slots(&format!("request.{id}"), value, out),
            ("headers", None) => header_slots(&format!("request.{id}.header"), value, out),
            ("headers", Some(rows)) => {
                indexed_rows(value, &rows.header, &format!("request.{id}.header"), out);
            }
            ("query_params", Some(rows)) => {
                indexed_rows(value, &rows.query, &format!("request.{id}.query"), out);
            }
            ("url", Some(rows)) if rows.url && value.is_string() => {
                out.push((format!("request.{id}.url"), Part::RequestUrl, value));
            }
            ("body", Some(rows)) if !rows.form.is_empty() => {
                if let Value::Object(variant) = value {
                    for (name, inner) in variant.iter_mut() {
                        let list = match name.as_str() {
                            "FormUrlEncoded" => inner.get_mut("fields"),
                            "Multipart" => inner.get_mut("parts"),
                            _ => None,
                        };
                        if let Some(list) = list {
                            indexed_rows(list, &rows.form, &format!("request.{id}.form"), out);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Credential slots of one flow step's overrides, at the positions
/// `flow_step_credential_positions` names (headers by name when the step does not parse).
fn flow_step_slots<'a>(flow: &str, step: &'a mut Value, out: &mut Slots<'a>) {
    let prefix = format!("flow.{flow}.step.{}", text_field(step, "id"));
    let rows = serde_json::from_value::<FlowStep>(step.clone())
        .ok()
        .map(|typed| Rows::of(flow_step_credential_positions(&typed)));
    let Value::Object(fields) = step else {
        return;
    };
    for (field, value) in fields.iter_mut() {
        match (field.as_str(), &rows) {
            ("headers_override", None) => header_slots(&format!("{prefix}.header"), value, out),
            ("headers_override", Some(rows)) => {
                indexed_rows(value, &rows.header, &format!("{prefix}.header"), out);
            }
            ("query_override", Some(rows)) => {
                indexed_rows(value, &rows.query, &format!("{prefix}.query"), out);
            }
            ("url_override", Some(rows)) if rows.url && value.is_string() => {
                out.push((format!("{prefix}.url"), Part::RequestUrl, value));
            }
            ("body_override", Some(rows)) if rows.form_lines && value.is_string() => {
                out.push((format!("{prefix}.form"), Part::FormLines, value));
            }
            _ => {}
        }
    }
}

/// Credential slots of a flow document: its steps' overrides and its credential-named run inputs.
fn flow_slots<'a>(body: &'a mut Value, out: &mut Slots<'a>) {
    let id = text_field(body, "flow/id");
    let Some(Value::Object(flow)) = body.get_mut("flow") else {
        return;
    };
    for (field, value) in flow.iter_mut() {
        match (field.as_str(), value) {
            ("steps", Value::Array(steps)) => {
                for step in steps {
                    flow_step_slots(&id, step, out);
                }
            }
            ("options", Value::Object(options)) => {
                if let Some(Value::Object(inputs)) = options.get_mut("initial_variables") {
                    for (name, value) in inputs
                        .iter_mut()
                        .filter(|(name, value)| value.is_string() && is_credential_key(name))
                    {
                        out.push((format!("flow.{id}.var.{name}"), Part::Literal, value));
                    }
                }
            }
            _ => {}
        }
    }
}

fn variable_slots<'a>(variables: Option<&'a mut Value>, prefix: &str, out: &mut Slots<'a>) {
    let Some(Value::Array(variables)) = variables else {
        return;
    };
    for variable in variables {
        let key = variable
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if variable.get("is_secret").and_then(Value::as_bool) == Some(true)
            && let Some(value) = variable.get_mut("value")
            && value.is_string()
        {
            out.push((format!("{prefix}{key}"), Part::Whole, value));
        }
    }
}

/// Whether a command-line flag name (without dashes) names a secret.
fn secret_flag(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["token", "key", "secret", "password", "auth"]
        .iter()
        .any(|word| name.contains(word))
}

/// Slots of stdio `args`: `--token=VALUE` entries and the entry after a bare `--token`.
fn arg_slots<'a>(args: &'a mut [Value], server: &str, out: &mut Slots<'a>) {
    let mut pending: Option<String> = None;
    for arg in args {
        let Some(text) = arg.as_str() else {
            pending = None;
            continue;
        };
        if let Some(flag) = pending.take() {
            out.push((format!("mcp.{server}.arg.{flag}"), Part::Whole, arg));
            continue;
        }
        if !text.starts_with('-') {
            continue;
        }
        let (flag, assigned) = match text.split_once('=') {
            Some((flag, _)) => (flag.to_owned(), true),
            None => (text.to_owned(), false),
        };
        if !secret_flag(flag.trim_start_matches('-')) {
            continue;
        }
        if assigned {
            out.push((format!("mcp.{server}.arg.{flag}"), Part::AfterEquals, arg));
        } else {
            pending = Some(flag);
        }
    }
}

/// Every secret string inside `body` of a `kind` document, with its placeholder key.
fn slots(kind: SyncKind, body: &mut Value) -> Slots<'_> {
    let mut out = Slots::new();
    match kind {
        SyncKind::Environment => {
            variable_slots(body.pointer_mut("/environment/variables"), "", &mut out)
        }
        SyncKind::Globals => variable_slots(body.get_mut("variables"), "", &mut out),
        SyncKind::Collection => {
            let id = text_field(body, "collection/id");
            if let Value::Object(root) = body {
                for (field, value) in root.iter_mut() {
                    match (field.as_str(), value) {
                        ("collection", Value::Object(collection)) => {
                            for (field, value) in collection.iter_mut() {
                                match field.as_str() {
                                    "variables" => variable_slots(Some(value), "", &mut out),
                                    "auth" => {
                                        auth_slots(&format!("collection.{id}"), value, &mut out)
                                    }
                                    _ => {}
                                }
                            }
                        }
                        ("folders", Value::Array(folders)) => {
                            for folder in folders {
                                let scope = format!("folder.{}", text_field(folder, "id"));
                                if let Some(auth) = folder.get_mut("auth") {
                                    auth_slots(&scope, auth, &mut out);
                                }
                            }
                        }
                        ("requests", Value::Array(requests)) => {
                            requests
                                .iter_mut()
                                .for_each(|request| request_slots(request, &mut out));
                        }
                        _ => {}
                    }
                }
            }
        }
        SyncKind::LlmProvider => {
            let id = body
                .pointer("/provider/id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if let Value::Object(root) = body {
                for (section, value) in root.iter_mut() {
                    match (section.as_str(), value) {
                        ("provider", Value::Object(provider)) => {
                            for (field, value) in provider.iter_mut() {
                                match (field.as_str(), value) {
                                    (field @ ("base_url" | "proxy_url"), url)
                                        if url.is_string() =>
                                    {
                                        out.push((format!("llm.{id}.{field}"), Part::Url, url));
                                    }
                                    ("quirks", Value::Object(quirks)) => {
                                        if let Some(Value::Object(headers)) =
                                            quirks.get_mut("extra_headers")
                                        {
                                            for (name, value) in headers
                                                .iter_mut()
                                                .filter(|(_, value)| value.is_string())
                                            {
                                                out.push((
                                                    format!("llm.{id}.header.{name}"),
                                                    Part::Whole,
                                                    value,
                                                ));
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        ("keys", Value::Array(keys)) => {
                            for key in keys {
                                let key_id = key
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned();
                                if let Some(secret) = key.get_mut("secret")
                                    && secret.is_string()
                                {
                                    out.push((
                                        format!("llm.{id}.key.{key_id}"),
                                        Part::Whole,
                                        secret,
                                    ));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        SyncKind::McpServer => {
            let name = body
                .pointer("/server/name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if let Some(Value::Object(transport)) = body.pointer_mut("/server/transport") {
                for (field, value) in transport.iter_mut() {
                    match (field.as_str(), value) {
                        ("url", value) if value.is_string() => {
                            out.push((format!("mcp.{name}.url"), Part::Url, value));
                        }
                        ("args", Value::Array(args)) => arg_slots(args, &name, &mut out),
                        (field @ ("env" | "headers"), Value::Object(map)) => {
                            let label = if field == "env" { "env" } else { "header" };
                            for (entry, value) in
                                map.iter_mut().filter(|(_, value)| value.is_string())
                            {
                                out.push((
                                    format!("mcp.{name}.{label}.{entry}"),
                                    Part::Whole,
                                    value,
                                ));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        SyncKind::Settings => {
            // The search keys ride in the `ai` section's `search` object (see `search_keys`);
            // the proxy URL and the SearXNG address are local-only and never exported.
            if let Some(Value::Object(search)) = body.get_mut("search") {
                for (field, value) in search.iter_mut().filter(|(_, value)| value.is_string()) {
                    match field.as_str() {
                        "tavily_key" => {
                            out.push((
                                crate::secrets::SEARCH_TAVILY.to_owned(),
                                Part::Whole,
                                value,
                            ));
                        }
                        "brave_key" => {
                            out.push((crate::secrets::SEARCH_BRAVE.to_owned(), Part::Whole, value));
                        }
                        _ => {}
                    }
                }
            }
        }
        SyncKind::LooseRequests => {
            if let Value::Object(root) = body {
                for (field, value) in root.iter_mut() {
                    if let ("requests", Value::Array(requests)) = (field.as_str(), value) {
                        requests
                            .iter_mut()
                            .for_each(|request| request_slots(request, &mut out));
                    }
                }
            }
        }
        SyncKind::Mock => {
            let id = text_field(body, "mock_server/id");
            if let Some(Value::Object(server)) = body.get_mut("mock_server") {
                for (field, value) in server.iter_mut() {
                    match (field.as_str(), value) {
                        ("proxy_req_headers", headers) => {
                            header_slots(&format!("mock.{id}.proxy_header"), headers, &mut out);
                        }
                        ("callbacks", Value::Array(callbacks)) => {
                            for callback in callbacks {
                                let prefix = format!(
                                    "mock.{id}.callback.{}.header",
                                    text_field(callback, "id")
                                );
                                if let Value::Object(fields) = callback {
                                    for (field, value) in fields.iter_mut() {
                                        if field == "headers" {
                                            header_slots(&prefix, value, &mut out);
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        SyncKind::Flow => flow_slots(body, &mut out),
        SyncKind::Workspace | SyncKind::LlmAlias | SyncKind::Skills => {}
    }
    out
}

/// The secret byte ranges of `text` with their placeholder keys.
fn ranges(part: Part, key: &str, text: &str) -> Vec<(String, Range<usize>)> {
    match part {
        Part::Whole => vec![(key.to_owned(), 0..text.len())],
        Part::Literal if text.contains("{{") => Vec::new(),
        Part::Literal => vec![(key.to_owned(), 0..text.len())],
        Part::AfterEquals => text
            .find('=')
            .map(|at| vec![(key.to_owned(), at + 1..text.len())])
            .unwrap_or_default(),
        Part::RequestUrl => url_credentials(text)
            .into_iter()
            .filter(|found| {
                text.get(found.range.clone())
                    .is_some_and(|part| !part.contains("{{"))
            })
            .map(|found| {
                let suffix = match found.part {
                    UrlPart::Userinfo => "userinfo".to_owned(),
                    UrlPart::Query(name) => format!("query.{name}"),
                };
                (format!("{key}.{suffix}"), found.range)
            })
            .collect(),
        Part::FormLines => form_line_credentials(text)
            .into_iter()
            .filter(|found| {
                text.get(found.range.clone())
                    .is_some_and(|value| !value.contains("{{"))
            })
            .map(|found| (format!("{key}.{}", found.name), found.range))
            .collect(),
        Part::Url => {
            let Some(scheme) = text.find("://") else {
                return Vec::new();
            };
            let start = scheme + 3;
            let authority_end = text[start..]
                .find(['/', '?', '#'])
                .map_or(text.len(), |at| start + at);
            let mut found = Vec::new();
            if let Some(at) = text[start..authority_end].rfind('@') {
                found.push((format!("{key}.userinfo"), start..start + at));
            }
            let fragment = text[authority_end..]
                .find('#')
                .map_or(text.len(), |at| authority_end + at);
            if let Some(at) = text[authority_end..fragment].find('?') {
                found.push((format!("{key}.query"), authority_end + at + 1..fragment));
            }
            found
        }
    }
}

/// Replaces every non-empty secret in a document by its placeholder.
pub(super) fn mask(kind: SyncKind, body: &mut Value) {
    for (key, part, slot) in slots(kind, body) {
        let Some(text) = slot.as_str() else { continue };
        let mut masked = text.to_owned();
        for (key, range) in ranges(part, &key, text).into_iter().rev() {
            if !range.is_empty() {
                masked.replace_range(range, &placeholder(&key));
            }
        }
        *slot = Value::String(masked);
    }
}

/// The secret values of a document by placeholder key.
fn values(kind: SyncKind, body: &mut Value) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (key, part, slot) in slots(kind, body) {
        if let Some(text) = slot.as_str() {
            for (key, range) in ranges(part, &key, text) {
                map.entry(key).or_insert_with(|| text[range].to_owned());
            }
        }
    }
    map
}

/// `text` with every `{{secret:KEY}}` swapped for its local value; a key without one becomes
/// empty and adds a message to `warnings`. Only keys for which `allowed` holds resolve: a
/// placeholder naming another slot's secret is dropped, so a file cannot move a secret into a
/// field that sends it somewhere else.
fn fill_text(
    text: &str,
    label: &str,
    local: &HashMap<String, String>,
    allowed: impl Fn(&str) -> bool,
    warnings: &mut Vec<String>,
) -> String {
    const OPEN: &str = "{{secret:";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find("}}") else { break };
        out.push_str(&rest[..start]);
        let key = &after[..end];
        if !allowed(key) {
            warnings.push(format!(
                "{label}: {{{{secret:{key}}}}} names another field's secret; imported empty"
            ));
        } else if let Some(value) = local.get(key) {
            out.push_str(value);
        } else {
            warnings.push(format!(
                "{label}: {{{{secret:{key}}}}} has no local value; imported empty"
            ));
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Where the variable list of a `kind` document sits (only the kinds whose variables have ids).
fn variables_pointer(kind: SyncKind) -> Option<&'static str> {
    match kind {
        SyncKind::Environment => Some("/environment/variables"),
        SyncKind::Globals => Some("/variables"),
        SyncKind::Collection => Some("/collection/variables"),
        _ => None,
    }
}

/// Refills the placeholder of each incoming secret variable from the local variable with the same
/// `id`, so a variable renamed on the other machine keeps its local value. A variable
/// without a local match is left for the by-key fill.
fn fill_by_identity(kind: SyncKind, incoming: &mut Value, local: &Value) {
    let Some(pointer) = variables_pointer(kind) else {
        return;
    };
    let text = |variable: &Value, field: &str| {
        variable
            .get(field)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let local_values: HashMap<String, String> = local
        .pointer(pointer)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|variable| variable.get("is_secret").and_then(Value::as_bool) == Some(true))
        .filter_map(|variable| Some((text(variable, "id")?, text(variable, "value")?)))
        .collect();
    let Some(Value::Array(variables)) = incoming.pointer_mut(pointer) else {
        return;
    };
    for variable in variables {
        let (Some(id), Some(key), Some(value)) = (
            text(variable, "id"),
            text(variable, "key"),
            text(variable, "value"),
        ) else {
            continue;
        };
        if value == placeholder(&key)
            && let Some(local_value) = local_values.get(&id)
        {
            variable["value"] = Value::String(local_value.clone());
        }
    }
}

/// Fills the placeholders of an incoming document from `local` (the same entity's local body).
/// A placeholder without a local value becomes empty and adds a message to `warnings`.
pub(super) fn fill(
    kind: SyncKind,
    label: &str,
    incoming: &mut Value,
    local: Option<&Value>,
    warnings: &mut Vec<String>,
) {
    if let Some(local) = local {
        fill_by_identity(kind, incoming, local);
    }
    let local = local.map_or_else(HashMap::new, |body| values(kind, &mut body.clone()));
    for (key, part, slot) in slots(kind, incoming) {
        let Some(text) = slot.as_str() else { continue };
        // A placeholder resolves only inside the slot it names (or that URL's own parts).
        let allowed = |name: &str| {
            name == key
                || match (part, name.strip_prefix(key.as_str())) {
                    (Part::Url, Some(rest)) => matches!(rest, ".userinfo" | ".query"),
                    (Part::RequestUrl, Some(rest)) => {
                        rest == ".userinfo"
                            || rest.strip_prefix(".query.").is_some_and(|q| !q.is_empty())
                    }
                    (Part::FormLines, Some(rest)) => {
                        rest.strip_prefix('.').is_some_and(|name| !name.is_empty())
                    }
                    _ => false,
                }
        };
        let mut filled = fill_text(text, label, &local, allowed, warnings);
        if matches!(part, Part::Url | Part::RequestUrl) && filled != text {
            // An emptied userinfo or query leaves no dangling `@` or `?` behind.
            filled = filled.replacen("://@", "://", 1);
            if part == Part::Url && filled.ends_with('?') {
                filled.pop();
            }
        }
        if filled != text {
            *slot = Value::String(filled);
        }
    }
}
