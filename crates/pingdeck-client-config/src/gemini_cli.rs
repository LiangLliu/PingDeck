//! Gemini CLI writer: selects API-key auth and the model in `~/.gemini/settings.json`, and sets
//! the key and gateway base URL in `~/.gemini/.env`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::env::{dotenv_value, upsert_dotenv};
use super::inspect::Found;
use super::rewrite::{Swap, decide};
use super::{
    Edit, Settings, Writer, child_object, edit_json, non_blank, read_json_object, read_optional,
};
use pingdeck_base::error::AppError;

/// Gemini CLI's SDK appends `/v1beta` to the gateway root itself.
const ADDRESS_SUFFIX: &str = "";

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

/// The `.env` variable that holds the gateway token.
pub(super) const API_KEY_VAR: &str = "GEMINI_API_KEY";
/// The `.env` variable that holds the gateway root.
pub(super) const BASE_URL_VAR: &str = "GOOGLE_GEMINI_BASE_URL";
/// The variable that selects the model (PingDeck writes `model.name` instead, but a shell export
/// of it wins).
pub(super) const MODEL_VAR: &str = "GEMINI_MODEL";
/// The dotenv file beside `settings.json`.
const ENV_FILE_NAME: &str = ".env";
/// The JSON settings file.
const SETTINGS_FILE_NAME: &str = "settings.json";
/// The `security.auth.selectedType` that makes Gemini CLI use `GEMINI_API_KEY`.
const AUTH_TYPE: &str = "gemini-api-key";

fn dir(home: &Path) -> PathBuf {
    super::env::tool_dir(home, super::ClientApp::GeminiCli, home.join(".gemini"))
}

/// `settings.json` and `.env`, resolving the tool directory once.
fn paths(home: &Path) -> (PathBuf, PathBuf) {
    let dir = dir(home);
    (dir.join(SETTINGS_FILE_NAME), dir.join(ENV_FILE_NAME))
}

/// The `.env` PingDeck writes.
pub(super) fn env_path(home: &Path) -> PathBuf {
    paths(home).1
}

fn files(home: &Path) -> Vec<PathBuf> {
    let (settings, env_file) = paths(home);
    vec![settings, env_file]
}

fn render(home: &Path, settings: &Settings<'_>) -> Result<Vec<Edit>, AppError> {
    let (json_path, env_path) = paths(home);
    let json_before = read_optional(&json_path)?;
    let json_after = edit_json(home, &json_path, json_before.as_deref(), |root| {
        let auth = child_object(
            child_object(root, "security", &json_path)?,
            "auth",
            &json_path,
        )?;
        auth.insert("selectedType".to_owned(), AUTH_TYPE.into());
        if let Some(model) = settings.model {
            child_object(root, "model", &json_path)?.insert("name".to_owned(), Value::from(model));
        }
        Ok(())
    })?;

    let env_before = read_optional(&env_path)?;
    let env_after = upsert_dotenv(
        env_before.as_deref().unwrap_or(""),
        API_KEY_VAR,
        settings.token,
    );
    let env_after = upsert_dotenv(&env_after, BASE_URL_VAR, settings.gateway);

    Ok(vec![
        Edit {
            path: json_path,
            before: json_before,
            after: json_after,
        },
        Edit {
            path: env_path,
            before: env_before,
            after: env_after,
        },
    ])
}

/// Reads back what [`render`] writes: address and key from `.env`, model from `settings.json`.
fn inspect(home: &Path) -> Result<Found, AppError> {
    let (settings_file, env_file) = paths(home);
    let env = read_optional(&env_file)?.unwrap_or_default();
    let settings = read_json_object(home, &settings_file)?;
    let alias = settings
        .as_ref()
        .and_then(|root| root.get("model")?.get("name")?.as_str());
    Ok(Found::new(
        dotenv_value(&env, BASE_URL_VAR),
        dotenv_value(&env, API_KEY_VAR),
        alias,
    ))
}

/// The key currently in `.env`, trimmed; `None` when absent or blank.
fn written_token(home: &Path) -> Result<Option<String>, AppError> {
    let env = read_optional(&dir(home).join(ENV_FILE_NAME))?.unwrap_or_default();
    Ok(non_blank(dotenv_value(&env, API_KEY_VAR)).map(str::to_owned))
}

/// Replaces the key in `.env` when it is the token with hash `old_sha256`; `settings.json` never
/// holds a token. `_home` keeps the signature every writer's `retoken` shares.
fn retoken(
    _home: &Path,
    path: &Path,
    text: &str,
    old_sha256: &str,
    new_token: &str,
) -> Result<Swap, AppError> {
    if path.file_name().is_none_or(|name| name != ENV_FILE_NAME) {
        return Ok(Swap::Nothing);
    }
    decide(dotenv_value(text, API_KEY_VAR), old_sha256, new_token)
        .into_swap(|| Ok(upsert_dotenv(text, API_KEY_VAR, new_token)))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::super::test_util::{TempHome, apply_now, request};
    use super::super::{ClientApp, preview};
    use super::*;

    const SETTINGS: &str = r#"{"theme":"GitHub","security":{"auth":{"enforcedType":"x"},"folderTrust":{"enabled":true}},"model":{"maxSessionTurns":5,"name":"gemini-2.5-pro"}}"#;
    const ENV: &str = "# keys\nFOO=bar\nexport GEMINI_API_KEY=old\nGOOGLE_CLOUD_PROJECT=p\n";

    #[test]
    fn merges_into_rich_config_and_backs_up() {
        let home = TempHome::new();
        home.write(".gemini/settings.json", SETTINGS);
        home.write(".gemini/.env", ENV);
        let req = request(ClientApp::GeminiCli, Some("fast"));

        let plan = preview(&home.0, &req).unwrap();
        let settings: serde_json::Value = serde_json::from_str(&plan.files[0].after).unwrap();
        assert_eq!(settings["theme"], "GitHub");
        assert_eq!(
            settings["security"]["auth"]["selectedType"],
            "gemini-api-key"
        );
        assert_eq!(settings["security"]["auth"]["enforcedType"], "x");
        assert_eq!(settings["security"]["folderTrust"]["enabled"], true);
        assert_eq!(settings["model"]["name"], "fast");
        assert_eq!(settings["model"]["maxSessionTurns"], 5);

        assert_eq!(
            plan.files[1].after,
            "# keys\nFOO=bar\nGEMINI_API_KEY=<gateway token>\nGOOGLE_CLOUD_PROJECT=p\nGOOGLE_GEMINI_BASE_URL=http://127.0.0.1:4141\n"
        );
        // The diff is for the screen: the key that is replaced shows only as its mask.
        assert!(plan.files[1].diff.contains(&format!(
            "-export GEMINI_API_KEY={}",
            pingdeck_base::secrets::REDACTED
        )));
        assert!(
            plan.files[1]
                .diff
                .contains("+GEMINI_API_KEY=<gateway token>")
        );

        let applied = apply_now(&home, &req).unwrap();
        assert_eq!(applied.files.len(), 2);
        assert_eq!(
            fs::read_to_string(applied.files[0].backup_path.clone().unwrap()).unwrap(),
            SETTINGS
        );
        assert_eq!(
            fs::read_to_string(applied.files[1].backup_path.clone().unwrap()).unwrap(),
            ENV
        );
        assert!(apply_now(&home, &req).unwrap().files.is_empty());
    }

    #[test]
    fn invalid_settings_block_all_writes() {
        let home = TempHome::new();
        home.write(".gemini/settings.json", "nope");
        let err = apply_now(&home, &request(ClientApp::GeminiCli, None)).unwrap_err();
        assert!(err.message().contains("settings.json"));
        assert!(home.read(".gemini/.env").is_none());
        assert_eq!(home.read(".gemini/settings.json").unwrap(), "nope");
    }

    #[test]
    fn commented_settings_are_edited_in_place() {
        let home = TempHome::new();
        let jsonc = "{\n  // theme\n  \"theme\": \"GitHub\",\n}\n";
        home.write(".gemini/settings.json", jsonc);
        apply_now(&home, &request(ClientApp::GeminiCli, Some("fast"))).unwrap();
        let text = home.read(".gemini/settings.json").unwrap();
        assert!(text.contains("// theme"), "{text}");
        let found = inspect(&home.0).unwrap();
        assert_eq!(found.alias.as_deref(), Some("fast"));
    }

    #[test]
    fn retoken_leaves_settings_json_alone() {
        let home = TempHome::new();
        let path = home.0.join(".gemini/settings.json");
        let swap = retoken(&home.0, &path, SETTINGS, "old", "new").unwrap();
        assert!(matches!(swap, Swap::Nothing));
    }

    #[test]
    fn written_token_reads_the_env_key_trimmed() {
        let home = TempHome::new();
        assert_eq!(written_token(&home.0).unwrap(), None);
        home.write(".gemini/.env", "FOO=bar\nGEMINI_API_KEY=\n");
        assert_eq!(written_token(&home.0).unwrap(), None);
        home.write(".gemini/.env", "export GEMINI_API_KEY=\"  spaced  \"\n");
        assert_eq!(written_token(&home.0).unwrap().as_deref(), Some("spaced"));
        let mut req = request(ClientApp::GeminiCli, Some("fast"));
        req.token = "pd_gm_abcdefghijklmnop".to_owned();
        apply_now(&home, &req).unwrap();
        assert_eq!(
            written_token(&home.0).unwrap().as_deref(),
            Some("pd_gm_abcdefghijklmnop")
        );
    }

    #[test]
    fn inspect_of_a_foreign_config_keeps_the_other_keys_out() {
        let home = TempHome::new();
        home.write(".gemini/settings.json", SETTINGS);
        home.write(".gemini/.env", ENV);
        let found = inspect(&home.0).unwrap();
        assert!(found.base_url.is_none());
        assert_eq!(found.alias.as_deref(), Some("gemini-2.5-pro"));
        assert!(found.token_sha256.is_some());
    }
}
