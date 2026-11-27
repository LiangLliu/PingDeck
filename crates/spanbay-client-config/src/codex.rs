//! Codex CLI writer: adds a custom `spanbay` provider to `~/.codex/config.toml`, preserving the
//! user's comments, ordering and unrelated tables.

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table, Value};

use super::inspect::Found;
use super::rewrite::{Swap, decide};
use super::{Edit, ModelFacts, Settings, Writer, display, non_blank, parse_toml, read_optional};
use spanbay_base::error::AppError;

/// Codex is pointed at `<gateway>/v1`.
const ADDRESS_SUFFIX: &str = "/v1";

/// This module as a [`Writer`].
pub(super) const WRITER: Writer = Writer {
    address_suffix: ADDRESS_SUFFIX,
    dir,
    files,
    render,
    inspect,
    written_token,
    retoken,
};

/// Provider id in `[model_providers.<id>]`; built-in ids (`openai`, `ollama`, `lmstudio`) are
/// reserved by Codex and must never be used.
const PROVIDER_ID: &str = "spanbay";
/// Top-level key that selects the provider.
const MODEL_PROVIDER_KEY: &str = "model_provider";
/// Top-level key that selects the model.
const MODEL_KEY: &str = "model";
/// Table of the custom providers.
const PROVIDERS_KEY: &str = "model_providers";
/// Provider key that holds the gateway token.
const TOKEN_KEY: &str = "experimental_bearer_token";
/// Provider key that holds the gateway address.
const BASE_URL_KEY: &str = "base_url";
/// The provider's display name.
const PROVIDER_NAME: &str = "SpanBay";
/// The gateway API Codex speaks.
const WIRE_API: &str = "responses";

fn dir(home: &Path) -> PathBuf {
    super::env::tool_dir(home, super::ClientApp::Codex, home.join(".codex"))
}

pub(super) fn config_path(home: &Path) -> PathBuf {
    dir(home).join("config.toml")
}

fn files(home: &Path) -> Vec<PathBuf> {
    vec![config_path(home)]
}

/// Our `[model_providers.spanbay]` entry, if there is one.
fn provider(doc: &DocumentMut) -> Option<&Item> {
    doc.get(PROVIDERS_KEY)?.get(PROVIDER_ID)
}

/// Whether `model_provider` selects our provider.
fn is_selected(doc: &DocumentMut) -> bool {
    doc.get(MODEL_PROVIDER_KEY).and_then(Item::as_str) == Some(PROVIDER_ID)
}

/// The error for a `key` that holds something other than a table.
fn not_table(path: &Path, key: &str) -> AppError {
    AppError::validation(format!("`{key}` in {} must be a table", display(path)))
}

/// Top-level key: the model's context window in tokens.
const CONTEXT_WINDOW_KEY: &str = "model_context_window";
/// Top-level key: the model's largest output in tokens.
const MAX_OUTPUT_KEY: &str = "model_max_output_tokens";
/// Top-level key: whether the model returns reasoning summaries.
const REASONING_SUMMARIES_KEY: &str = "model_supports_reasoning_summaries";

/// Whether two TOML values are the same string, integer or boolean.
fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(a), Value::String(b)) => a.value() == b.value(),
        (Value::Integer(a), Value::Integer(b)) => a.value() == b.value(),
        (Value::Boolean(a), Value::Boolean(b)) => a.value() == b.value(),
        _ => false,
    }
}

/// Sets `table[key]` to `value`, keeping the existing line's comments and skipping no-op writes.
fn set_value(table: &mut Table, key: &str, value: Value) {
    match table.get_mut(key).and_then(Item::as_value_mut) {
        Some(existing) if same_value(existing, &value) => {}
        Some(existing) => {
            let decor = std::mem::take(existing.decor_mut());
            *existing = value;
            *existing.decor_mut() = decor;
        }
        None => {
            table.insert(key, Item::Value(value));
        }
    }
}

/// [`set_value`] for a string.
fn set_str(table: &mut Table, key: &str, value: &str) {
    set_value(table, key, Value::from(value));
}

/// Sets `table[key]` to `value`, or removes the key when the value is unknown: a hint left from
/// another model would be wrong for this one.
fn set_or_remove(table: &mut Table, key: &str, value: Option<Value>) {
    match value {
        Some(value) => set_value(table, key, value),
        None => {
            table.remove(key);
        }
    }
}

/// Writes the capability hints of the model Codex is pointed at: Codex knows a model's family
/// from its slug, which a `provider/model` name does not match.
fn write_facts(table: &mut Table, facts: &ModelFacts) {
    let tokens = |n: Option<u64>| n.and_then(|n| i64::try_from(n).ok()).map(Value::from);
    set_or_remove(table, CONTEXT_WINDOW_KEY, tokens(facts.context_window));
    set_or_remove(table, MAX_OUTPUT_KEY, tokens(facts.max_output_tokens));
    set_or_remove(
        table,
        REASONING_SUMMARIES_KEY,
        facts.reasoning.map(Value::from),
    );
}

fn render(home: &Path, settings: &Settings<'_>) -> Result<Vec<Edit>, AppError> {
    let path = config_path(home);
    let before = read_optional(&path)?;
    let mut doc = parse_toml(home, &path, before.as_deref().unwrap_or(""))?;

    set_str(doc.as_table_mut(), MODEL_PROVIDER_KEY, PROVIDER_ID);
    if let Some(model) = settings.model {
        set_str(doc.as_table_mut(), MODEL_KEY, model);
        write_facts(doc.as_table_mut(), settings.facts);
    }

    let providers = doc
        .entry(PROVIDERS_KEY)
        .or_insert_with(|| {
            let mut table = Table::new();
            table.set_implicit(true);
            Item::Table(table)
        })
        .as_table_mut()
        .ok_or_else(|| not_table(&path, PROVIDERS_KEY))?;
    let provider = providers
        .entry(PROVIDER_ID)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| not_table(&path, &format!("{PROVIDERS_KEY}.{PROVIDER_ID}")))?;
    set_str(provider, "name", PROVIDER_NAME);
    set_str(
        provider,
        BASE_URL_KEY,
        &format!("{}{ADDRESS_SUFFIX}", settings.gateway),
    );
    set_str(provider, "wire_api", WIRE_API);
    set_str(provider, TOKEN_KEY, settings.token);
    // Codex rejects mixing credential sources; the token above replaces any old env var.
    provider.remove("env_key");

    Ok(vec![Edit {
        path,
        before,
        after: doc.to_string(),
    }])
}

/// Reads back what [`render`] writes; a config whose `model_provider` is another provider counts
/// as not configured.
fn inspect(home: &Path) -> Result<Found, AppError> {
    let path = config_path(home);
    let Some(text) = read_optional(&path)? else {
        return Ok(Found::default());
    };
    let doc = parse_toml(home, &path, &text)?;
    if !is_selected(&doc) {
        return Ok(Found::default());
    }
    let get = |key: &str| provider(&doc)?.get(key)?.as_str();
    Ok(Found::new(
        get(BASE_URL_KEY),
        get(TOKEN_KEY),
        doc.get(MODEL_KEY).and_then(Item::as_str),
    ))
}

/// The token currently in the config, trimmed; `None` when absent, blank or when another
/// provider is selected.
fn written_token(home: &Path) -> Result<Option<String>, AppError> {
    let path = config_path(home);
    let Some(text) = read_optional(&path)? else {
        return Ok(None);
    };
    let doc = parse_toml(home, &path, &text)?;
    if !is_selected(&doc) {
        return Ok(None);
    }
    let token = provider(&doc).and_then(|p| p.get(TOKEN_KEY)?.as_str());
    Ok(non_blank(token).map(str::to_owned))
}

/// Replaces the token of an earlier write when it is the one with hash `old_sha256`. Unlike
/// [`inspect`], it does not require our provider to be selected.
fn retoken(
    home: &Path,
    path: &Path,
    text: &str,
    old_sha256: &str,
    new_token: &str,
) -> Result<Swap, AppError> {
    let mut doc = parse_toml(home, path, text)?;
    let current = provider(&doc).and_then(|p| p.get(TOKEN_KEY)?.as_str());
    decide(current, old_sha256, new_token).into_swap(|| {
        let provider = doc
            .get_mut(PROVIDERS_KEY)
            .and_then(Item::as_table_mut)
            .and_then(|providers| providers.get_mut(PROVIDER_ID))
            .and_then(Item::as_table_mut)
            .ok_or_else(|| not_table(path, &format!("{PROVIDERS_KEY}.{PROVIDER_ID}")))?;
        set_str(provider, TOKEN_KEY, new_token);
        Ok(doc.to_string())
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::super::test_util::{TempHome, apply_now, request};
    use super::super::{ClientApp, preview};
    use super::*;

    const EXISTING: &str = r#"# my codex config
model = "gpt-5"   # daily driver
approval_policy = "on-request"

[model_providers.other]
name = "Other"  # keep me
base_url = "https://other.example/v1"
env_key = "OTHER_KEY"

[projects."/tmp/x"]
trust_level = "trusted"
"#;

    #[test]
    fn empty_home_creates_valid_document() {
        let home = TempHome::new();
        let plan = preview(&home.0, &request(ClientApp::Codex, Some("fast"))).unwrap();
        let doc: toml_edit::DocumentMut = plan.files[0].after.parse().unwrap();
        assert_eq!(doc["model_provider"].as_str(), Some("spanbay"));
        assert_eq!(doc["model"].as_str(), Some("fast"));
        let p = &doc["model_providers"]["spanbay"];
        assert_eq!(p["base_url"].as_str(), Some("http://127.0.0.1:4141/v1"));
        assert_eq!(p["wire_api"].as_str(), Some("responses"));
        assert_eq!(
            p["experimental_bearer_token"].as_str(),
            Some("<gateway token>")
        );
        assert!(!plan.files[0].after.contains("[model_providers]\n"));
    }

    #[test]
    fn merge_preserves_comments_and_other_tables() {
        let home = TempHome::new();
        home.write(".codex/config.toml", EXISTING);
        let req = request(ClientApp::Codex, Some("fast"));

        let plan = preview(&home.0, &req).unwrap();
        let after = &plan.files[0].after;
        for kept in [
            "# my codex config",
            "approval_policy = \"on-request\"",
            "name = \"Other\"  # keep me",
            "env_key = \"OTHER_KEY\"",
            "[projects.\"/tmp/x\"]",
        ] {
            assert!(after.contains(kept), "lost `{kept}` in:\n{after}");
        }
        // The user's model is replaced but its trailing comment survives.
        assert!(
            after.contains("model = \"fast\"   # daily driver"),
            "{after}"
        );
        let doc: toml_edit::DocumentMut = after.parse().unwrap();
        assert_eq!(doc["model_provider"].as_str(), Some("spanbay"));
        assert!(plan.files[0].diff.contains("-model = \"gpt-5\""));
        assert!(plan.files[0].diff.contains("+model = \"fast\""));

        let applied = apply_now(&home, &req).unwrap();
        let backup = applied.files[0].backup_path.clone().unwrap();
        assert_eq!(fs::read_to_string(backup).unwrap(), EXISTING);
        assert!(apply_now(&home, &req).unwrap().files.is_empty());
    }

    #[test]
    fn stale_env_key_in_our_provider_is_removed() {
        let home = TempHome::new();
        home.write(
            ".codex/config.toml",
            "[model_providers.spanbay]\nenv_key = \"OLD\"\nwire_api = \"chat\"\n",
        );
        let plan = preview(&home.0, &request(ClientApp::Codex, None)).unwrap();
        let doc: toml_edit::DocumentMut = plan.files[0].after.parse().unwrap();
        let p = &doc["model_providers"]["spanbay"];
        assert!(p.get("env_key").is_none());
        assert_eq!(p["wire_api"].as_str(), Some("responses"));
        assert!(doc.get("model").is_none());
    }

    #[test]
    fn the_models_known_facts_are_written_and_unknown_ones_removed() {
        let home = TempHome::new();
        home.write(
            ".codex/config.toml",
            "model_context_window = 8000 # old model\n[projects.\"/x\"]\ntrust_level = \"trusted\"\n",
        );
        let mut req = request(ClientApp::Codex, Some("Acme/deep-1"));
        req.facts = ModelFacts {
            context_window: Some(200_000),
            max_output_tokens: None,
            reasoning: Some(true),
            ..ModelFacts::default()
        };
        let plan = preview(&home.0, &req).unwrap();
        let after = &plan.files[0].after;
        let doc: toml_edit::DocumentMut = after.parse().unwrap();
        assert_eq!(doc["model_context_window"].as_integer(), Some(200_000));
        assert!(
            after.contains("model_context_window = 200000 # old model"),
            "{after}"
        );
        assert_eq!(
            doc["model_supports_reasoning_summaries"].as_bool(),
            Some(true)
        );
        assert!(doc.get("model_max_output_tokens").is_none());
        // The hints sit at the top level, not inside the last table.
        assert!(
            doc["projects"]["/x"]
                .get("model_supports_reasoning_summaries")
                .is_none()
        );
        assert!(
            plan.files[0]
                .diff
                .contains("+model_supports_reasoning_summaries = true")
        );

        // Without a model the hints are left as they are.
        let plan = preview(&home.0, &request(ClientApp::Codex, None)).unwrap();
        assert!(plan.files[0].after.contains("model_context_window = 8000"));
    }

    #[test]
    fn invalid_toml_is_rejected() {
        let home = TempHome::new();
        home.write(".codex/config.toml", "model = = 1\n");
        let err = apply_now(&home, &request(ClientApp::Codex, None)).unwrap_err();
        assert!(err.message().contains("config.toml"));
        assert_eq!(home.read(".codex/config.toml").unwrap(), "model = = 1\n");

        home.write(".codex/config.toml", "model_providers = 3\n");
        assert!(preview(&home.0, &request(ClientApp::Codex, None)).is_err());
    }

    #[test]
    fn comments_survive_a_second_apply_with_a_new_token() {
        let home = TempHome::new();
        home.write(".codex/config.toml", EXISTING);
        let mut req = request(ClientApp::Codex, Some("fast"));
        apply_now(&home, &req).unwrap();
        req.token = "pd_cx_abcdefghijklmnop".to_owned();
        apply_now(&home, &req).unwrap();
        let text = home.read(".codex/config.toml").unwrap();
        for kept in ["# my codex config", "# keep me", "# daily driver"] {
            assert!(text.contains(kept), "lost `{kept}` in:\n{text}");
        }
        assert!(text.contains("pd_cx_abcdefghijklmnop"));
    }

    #[test]
    fn written_token_reads_the_bearer_token_of_our_provider() {
        let home = TempHome::new();
        assert_eq!(written_token(&home.0).unwrap(), None);
        home.write(".codex/config.toml", EXISTING);
        assert_eq!(written_token(&home.0).unwrap(), None);
        let mut req = request(ClientApp::Codex, None);
        req.token = "pd_cx_abcdefghijklmnop".to_owned();
        apply_now(&home, &req).unwrap();
        assert_eq!(
            written_token(&home.0).unwrap().as_deref(),
            Some("pd_cx_abcdefghijklmnop")
        );
        // Another selected provider means this config does not use our token.
        let text = home
            .read(".codex/config.toml")
            .unwrap()
            .replace("model_provider = \"spanbay\"", "model_provider = \"other\"");
        home.write(".codex/config.toml", &text);
        assert_eq!(written_token(&home.0).unwrap(), None);
        home.write(".codex/config.toml", "a = = 1\n");
        assert!(written_token(&home.0).is_err());
    }

    #[test]
    fn inspect_ignores_a_config_using_another_provider() {
        let home = TempHome::new();
        home.write(".codex/config.toml", EXISTING);
        let found = inspect(&home.0).unwrap();
        assert!(found.base_url.is_none() && found.token_sha256.is_none());
    }

    #[test]
    fn inspect_rejects_invalid_toml() {
        let home = TempHome::new();
        home.write(".codex/config.toml", "a = = 1\n");
        assert!(inspect(&home.0).is_err());
    }
}
