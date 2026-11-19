//! The `pm.*` script snippet catalog offered next to the pre-request and test editors.

use serde::Serialize;

/// Which script editor a snippet belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptPhase {
    /// Runs before the request is sent.
    Pre,
    /// Runs after the response arrives.
    Post,
}

/// One insertable script template.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ScriptSnippet {
    /// Stable identifier.
    pub id: &'static str,
    /// The editor it is offered in.
    pub phase: ScriptPhase,
    /// Short button label.
    pub label: &'static str,
    /// Code inserted at the cursor (ends with a newline).
    pub code: &'static str,
}

const CATALOG: [ScriptSnippet; 8] = [
    ScriptSnippet {
        id: "pre-set-variable",
        phase: ScriptPhase::Pre,
        label: "Set variable",
        code: "pm.environment.set(\"key\", \"value\");\n",
    },
    ScriptSnippet {
        id: "pre-add-header",
        phase: ScriptPhase::Pre,
        label: "Add header",
        code: "pm.request.headers.add({ key: \"X-Debug\", value: \"1\" });\n",
    },
    ScriptSnippet {
        id: "pre-send-request",
        phase: ScriptPhase::Pre,
        label: "Send request",
        code: "pm.sendRequest(\"https://example.com\", (error, response) => {\n  if (!error) { console.log(response.code); }\n});\n",
    },
    ScriptSnippet {
        id: "pre-log-request-name",
        phase: ScriptPhase::Pre,
        label: "Log request name",
        code: "console.log(\"debug\", pm.info.requestName);\n",
    },
    ScriptSnippet {
        id: "post-status-test",
        phase: ScriptPhase::Post,
        label: "Status test",
        code: "pm.test(\"status is 200\", () => {\n  pm.response.to.have.status(200);\n});\n",
    },
    ScriptSnippet {
        id: "post-json-field",
        phase: ScriptPhase::Post,
        label: "JSON field",
        code: "pm.test(\"has expected field\", () => {\n  pm.expect(pm.response.json().field).to.equal(\"value\");\n});\n",
    },
    ScriptSnippet {
        id: "post-response-time",
        phase: ScriptPhase::Post,
        label: "Response time",
        code: "pm.test(\"responds within 500ms\", () => {\n  pm.expect(pm.response.responseTime).to.be.below(500);\n});\n",
    },
    ScriptSnippet {
        id: "post-save-variable",
        phase: ScriptPhase::Post,
        label: "Save variable",
        code: "pm.test(\"captures id\", () => {\n  pm.environment.set(\"id\", pm.response.json().id);\n});\n",
    },
];

/// The snippets in display order: the pre-request ones, then the post-response ones.
#[must_use]
pub fn script_snippet_catalog() -> Vec<ScriptSnippet> {
    CATALOG.to_vec()
}
