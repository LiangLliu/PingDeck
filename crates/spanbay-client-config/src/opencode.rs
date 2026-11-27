//! OpenCode writer: adds a `spanbay` OpenAI-compatible provider to
//! `~/.config/opencode/opencode.json` and selects it as the default model.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::inspect::Found;
use super::rewrite::{Swap, decide};
use super::{
    Edit, ModelFacts, Settings, Writer, child_object, edit_json, non_blank, parse_json_object,
    read_json_object, read_optional,
};
use spanbay_base::error::AppError;

/// OpenCode is pointed at `<gateway>/v1`.
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

/// Provider id inside `opencode.json`.
const PROVIDER_ID: &str = "spanbay";
/// Key of the JSON schema reference, added when the file has none.
const SCHEMA_KEY: &str = "$schema";
/// OpenCode's config schema.
const SCHEMA_URL: &str = "https://opencode.ai/config.json";
/// The AI SDK provider package that speaks `/v1/chat/completions`.
const NPM_PACKAGE: &str = "@ai-sdk/openai-compatible";
/// The provider's display name.
const PROVIDER_NAME: &str = "SpanBay";

fn dir(home: &Path) -> PathBuf {
    home.join(".config").join("opencode")
}

fn config_path(home: &Path) -> PathBuf {
    dir(home).join("opencode.json")
}

fn files(home: &Path) -> Vec<PathBuf> {
    vec![config_path(home)]
}

/// Our `provider.spanbay` entry, if there is one.
fn provider(root: &Map<String, Value>) -> Option<&Value> {
    root.get("provider")?.get(PROVIDER_ID)
}

/// The string at `provider.spanbay.options.apiKey`, if there is one.
fn api_key(root: &Map<String, Value>) -> Option<&str> {
    provider(root)?.get("options")?.get("apiKey")?.as_str()
}

fn render(home: &Path, settings: &Settings<'_>) -> Result<Vec<Edit>, AppError> {
    let path = config_path(home);
    let before = read_optional(&path)?;
    let after = edit_json(home, &path, before.as_deref(), |root| {
        root.entry(SCHEMA_KEY)
            .or_insert_with(|| Value::from(SCHEMA_URL));
        let provider = child_object(child_object(root, "provider", &path)?, PROVIDER_ID, &path)?;
        provider.insert("npm".to_owned(), NPM_PACKAGE.into());
        provider.insert("name".to_owned(), PROVIDER_NAME.into());
        // Only the two keys SpanBay manages are set; the user's other options stay. A value
        // that is not an object is replaced.
        if !provider.get("options").is_some_and(Value::is_object) {
            provider.insert("options".to_owned(), Value::Object(Map::new()));
        }
        let options = child_object(provider, "options", &path)?;
        options.insert(
            "baseURL".to_owned(),
            format!("{}{ADDRESS_SUFFIX}", settings.gateway).into(),
        );
        options.insert("apiKey".to_owned(), settings.token.into());
        let models = child_object(provider, "models", &path)?;
        if let Some(model) = settings.model {
            let entry = models
                .entry(model)
                .or_insert_with(|| json!({ "name": model }));
            if let Some(entry) = entry.as_object_mut() {
                fill_facts(entry, settings.facts);
            }
            root.insert("model".to_owned(), format!("{PROVIDER_ID}/{model}").into());
        }
        Ok(())
    })?;
    Ok(vec![Edit {
        path,
        before,
        after,
    }])
}

/// Adds the model's known facts to its entry where the entry has no value of its own: OpenCode
/// cannot tell the limits and abilities of a model of a custom provider by its name.
fn fill_facts(entry: &mut Map<String, Value>, facts: &ModelFacts) {
    if let (Some(context), Some(output)) = (facts.context_window, facts.max_output_tokens) {
        entry
            .entry("limit")
            .or_insert_with(|| json!({ "context": context, "output": output }));
    }
    for (key, known) in [
        ("reasoning", facts.reasoning),
        ("tool_call", facts.tool_call),
        ("attachment", facts.attachment),
    ] {
        if let Some(known) = known {
            entry.entry(key).or_insert(Value::Bool(known));
        }
    }
}

/// Reads back what [`render`] writes; the alias is the part of `model` after `spanbay/`.
fn inspect(home: &Path) -> Result<Found, AppError> {
    let Some(root) = read_json_object(home, &config_path(home))? else {
        return Ok(Found::default());
    };
    let Some(provider) = provider(&root) else {
        return Ok(Found::default());
    };
    let option = |key: &str| provider.get("options")?.get(key)?.as_str();
    let alias = root
        .get("model")
        .and_then(Value::as_str)
        .and_then(|model| model.strip_prefix(PROVIDER_ID)?.strip_prefix('/'));
    Ok(Found::new(option("baseURL"), option("apiKey"), alias))
}

/// The token currently in the config, trimmed; `None` when absent or blank.
fn written_token(home: &Path) -> Result<Option<String>, AppError> {
    let root = read_json_object(home, &config_path(home))?;
    Ok(root
        .as_ref()
        .and_then(|root| non_blank(api_key(root)))
        .map(str::to_owned))
}

/// Replaces the token of an earlier write when it is the one with hash `old_sha256`.
fn retoken(
    home: &Path,
    path: &Path,
    text: &str,
    old_sha256: &str,
    new_token: &str,
) -> Result<Swap, AppError> {
    let root = parse_json_object(home, path, text)?;
    decide(api_key(&root), old_sha256, new_token).into_swap(|| {
        edit_json(home, path, Some(text), |root| {
            let provider = child_object(child_object(root, "provider", path)?, PROVIDER_ID, path)?;
            child_object(provider, "options", path)?.insert("apiKey".to_owned(), new_token.into());
            Ok(())
        })
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::super::test_util::{TempHome, apply_now, request};
    use super::super::{ClientApp, preview};
    use super::*;

    const EXISTING: &str = r#"{
  "model": "anthropic/x",
  "provider": {
    "anthropic": {
      "options": {
        "timeout": 1
      }
    },
    "spanbay": {
      "models": {
        "fast": {
          "limit": {
            "context": 9
          },
          "name": "Custom"
        }
      }
    }
  },
  "theme": "dark"
}
"#;

    #[test]
    fn merges_into_rich_config_and_backs_up() {
        let home = TempHome::new();
        home.write(".config/opencode/opencode.json", EXISTING);
        let req = request(ClientApp::OpenCode, Some("fast"));

        let plan = preview(&home.0, &req).unwrap();
        let after: serde_json::Value = serde_json::from_str(&plan.files[0].after).unwrap();
        assert_eq!(after["theme"], "dark");
        assert_eq!(after["provider"]["anthropic"]["options"]["timeout"], 1);
        let ours = &after["provider"]["spanbay"];
        assert_eq!(ours["npm"], "@ai-sdk/openai-compatible");
        assert_eq!(ours["options"]["baseURL"], "http://127.0.0.1:4141/v1");
        assert_eq!(ours["options"]["apiKey"], "<gateway token>");
        // An existing model entry is kept as the user wrote it.
        assert_eq!(ours["models"]["fast"]["name"], "Custom");
        assert_eq!(ours["models"]["fast"]["limit"]["context"], 9);
        assert_eq!(after["model"], "spanbay/fast");
        assert!(plan.files[0].diff.contains("-  \"model\": \"anthropic/x\""));
        assert!(
            plan.files[0]
                .diff
                .contains("+  \"model\": \"spanbay/fast\"")
        );

        let applied = apply_now(&home, &req).unwrap();
        let backup = applied.files[0].backup_path.clone().unwrap();
        assert_eq!(fs::read_to_string(backup).unwrap(), EXISTING);
        assert!(apply_now(&home, &req).unwrap().files.is_empty());
    }

    #[test]
    fn user_options_of_our_provider_survive_a_write() {
        let home = TempHome::new();
        home.write(
            ".config/opencode/opencode.json",
            r#"{"provider":{"spanbay":{"options":{"timeout":5,"headers":{"X-A":"1"},"baseURL":"http://old/v1","apiKey":"old"}}}}"#,
        );
        let plan = preview(&home.0, &request(ClientApp::OpenCode, None)).unwrap();
        let after: serde_json::Value = serde_json::from_str(&plan.files[0].after).unwrap();
        let options = &after["provider"]["spanbay"]["options"];
        assert_eq!(options["timeout"], 5);
        assert_eq!(options["headers"]["X-A"], "1");
        assert_eq!(options["baseURL"], "http://127.0.0.1:4141/v1");
        assert_eq!(options["apiKey"], "<gateway token>");
        assert_eq!(options.as_object().unwrap().len(), 4);
    }

    #[test]
    fn model_none_keeps_existing_selection() {
        let home = TempHome::new();
        home.write(".config/opencode/opencode.json", EXISTING);
        let plan = preview(&home.0, &request(ClientApp::OpenCode, None)).unwrap();
        let after: serde_json::Value = serde_json::from_str(&plan.files[0].after).unwrap();
        assert_eq!(after["model"], "anthropic/x");
    }

    #[test]
    fn invalid_json_is_rejected_and_untouched() {
        let home = TempHome::new();
        let req = request(ClientApp::OpenCode, None);
        home.write(".config/opencode/opencode.json", "{oops");
        let err = apply_now(&home, &req).unwrap_err();
        assert!(err.message().contains("`~/.config/opencode/opencode.json`"));
        assert_eq!(
            home.read(".config/opencode/opencode.json").unwrap(),
            "{oops"
        );
    }

    #[test]
    fn a_jsonc_file_keeps_its_comments_order_and_trailing_commas() {
        let home = TempHome::new();
        let jsonc =
            "{\n  // my theme\n  \"theme\": \"dark\",\n  \"autoupdate\": false, /* keep */\n}\n";
        home.write(".config/opencode/opencode.json", jsonc);
        let applied = apply_now(&home, &request(ClientApp::OpenCode, Some("fast"))).unwrap();
        assert_eq!(applied.files.len(), 1);
        let text = home.read(".config/opencode/opencode.json").unwrap();
        for kept in ["// my theme", "/* keep */"] {
            assert!(text.contains(kept), "lost `{kept}` in:\n{text}");
        }
        // The user's keys stay first, in their order; SpanBay's keys follow.
        let at = |needle: &str| text.find(needle).unwrap();
        assert!(
            at("\"theme\"") < at("\"autoupdate\"") && at("\"autoupdate\"") < at("\"provider\"")
        );
        let found = inspect(&home.0).unwrap();
        assert_eq!(found.alias.as_deref(), Some("fast"));
        assert_eq!(found.base_url.as_deref(), Some("http://127.0.0.1:4141/v1"));
    }

    #[test]
    fn facts_fill_a_new_model_entry_but_never_the_users_values() {
        let home = TempHome::new();
        home.write(".config/opencode/opencode.json", EXISTING);
        let facts = ModelFacts {
            context_window: Some(128_000),
            max_output_tokens: Some(8_192),
            reasoning: Some(true),
            tool_call: Some(true),
            attachment: None,
        };
        for (alias, expected_context) in [("fast", 9), ("Acme/deep", 128_000)] {
            let mut req = request(ClientApp::OpenCode, Some(alias));
            req.facts = facts;
            let plan = preview(&home.0, &req).unwrap();
            let after: serde_json::Value = serde_json::from_str(&plan.files[0].after).unwrap();
            let entry = &after["provider"]["spanbay"]["models"][alias];
            assert_eq!(entry["limit"]["context"], expected_context, "{alias}");
            assert_eq!(entry["reasoning"], true, "{alias}");
            assert_eq!(entry["tool_call"], true, "{alias}");
            assert!(entry.get("attachment").is_none(), "{alias}");
        }
    }

    #[test]
    fn written_token_reads_the_api_key_of_our_provider() {
        let home = TempHome::new();
        assert_eq!(written_token(&home.0).unwrap(), None);
        // EXISTING has a `spanbay` provider without options.
        home.write(".config/opencode/opencode.json", EXISTING);
        assert_eq!(written_token(&home.0).unwrap(), None);
        let mut req = request(ClientApp::OpenCode, Some("fast"));
        req.token = "pd_oc_abcdefghijklmnop".to_owned();
        apply_now(&home, &req).unwrap();
        assert_eq!(
            written_token(&home.0).unwrap().as_deref(),
            Some("pd_oc_abcdefghijklmnop")
        );
        home.write(".config/opencode/opencode.json", "{oops");
        assert!(written_token(&home.0).is_err());
    }

    #[test]
    fn inspect_without_our_provider_or_with_a_foreign_model() {
        let home = TempHome::new();
        home.write(".config/opencode/opencode.json", EXISTING);
        // EXISTING has a `spanbay` provider without options, and a model of another provider.
        let found = inspect(&home.0).unwrap();
        assert!(found.base_url.is_none() && found.alias.is_none());
        home.write(
            ".config/opencode/opencode.json",
            r#"{"model":"anthropic/x"}"#,
        );
        assert!(inspect(&home.0).unwrap().base_url.is_none());
    }
}
