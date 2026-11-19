//! Translation of the schema-1 `gateway`, `models` and `agent` settings rows into the schema-2
//! `ai` section.
//!
//! Single responsibility: one pure function, shared by the database migration (v22) and the
//! config-sync importer (an older export still carries the three sections), so both read the old
//! shape the same way. Credentials are not kept in the section any more: they come back as
//! `(secret_ref, plaintext)` pairs for the caller to store, and the section keeps only masked
//! hints. Values outside the schema-2 ranges are clamped so a stored section stays valid.

use serde_json::{Map, Value, json};

use pingdeck_base::secrets::{LEGACY_GATEWAY_TOKEN, SEARCH_BRAVE, SEARCH_TAVILY, mask_secret_hint};

/// The result of [`ai_from_v1_sections`].
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyAi {
    /// The schema-2 `ai` section as JSON (only the keys the old rows set; the rest default).
    pub ai: Value,
    /// Credentials the old rows held in plaintext, to be stored under these references.
    pub secrets: Vec<(&'static str, String)>,
}

/// Copies `from[key]` to `to[key]` when `accept` likes the value.
fn copy(
    from: &Map<String, Value>,
    key: &str,
    to: &mut Map<String, Value>,
    accept: fn(&Value) -> bool,
) {
    if let Some(value) = from.get(key).filter(|value| accept(value)) {
        to.insert(key.to_owned(), value.clone());
    }
}

/// Copies an integer `from[key]` to `to[key]`, clamped to `min..=max`.
fn copy_clamped(
    from: &Map<String, Value>,
    key: &str,
    to: &mut Map<String, Value>,
    min: u64,
    max: u64,
) {
    if let Some(number) = from.get(key).and_then(Value::as_u64) {
        to.insert(key.to_owned(), json!(number.clamp(min, max)));
    }
}

fn text(value: &Value) -> bool {
    value.is_string()
}

fn boolean(value: &Value) -> bool {
    value.is_boolean()
}

fn integer(value: &Value) -> bool {
    value.is_u64()
}

fn array(value: &Value) -> bool {
    value.is_array()
}

/// A non-empty secret string under `key`, if the old section held one.
fn secret(from: &Map<String, Value>, key: &str) -> Option<String> {
    from.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Builds the schema-2 `ai` section from the stored schema-1 sections (each `None` when its row
/// does not exist). The old default system prompt is dropped: schema 2 has no such setting.
#[must_use]
pub fn ai_from_v1_sections(
    gateway: Option<&Value>,
    models: Option<&Value>,
    agent: Option<&Value>,
) -> LegacyAi {
    let mut ai = Map::new();
    let mut secrets = Vec::new();

    if let Some(old) = gateway.and_then(Value::as_object) {
        let mut new = Map::new();
        copy(old, "enabled_on_start", &mut new, boolean);
        copy(old, "port", &mut new, integer);
        copy(old, "require_token", &mut new, boolean);
        copy_clamped(old, "max_retries", &mut new, 0, 5);
        copy_clamped(old, "first_byte_timeout_s", &mut new, 5, 600);
        copy_clamped(old, "idle_timeout_s", &mut new, 5, 600);
        copy_clamped(old, "circuit_failure_threshold", &mut new, 1, 50);
        copy_clamped(old, "circuit_open_s", &mut new, 5, 3600);
        // Schema 2 has no master token: the old one is handed back so migration v24 can turn it
        // into a per-client token (existing client configs keep working).
        if let Some(token) = secret(old, "token") {
            secrets.push((LEGACY_GATEWAY_TOKEN, token));
        }
        ai.insert("gateway".to_owned(), Value::Object(new));
    }

    if let Some(old) = models.and_then(Value::as_object) {
        let mut new = Map::new();
        copy(old, "catalog_auto_refresh", &mut new, boolean);
        copy(old, "default_test_prompt", &mut new, text);
        copy(old, "probe_concurrency", &mut new, integer);
        ai.insert("providers".to_owned(), Value::Object(new));
    }

    if let Some(old) = agent.and_then(Value::as_object) {
        let mut new = Map::new();
        if let Some(name) = old
            .get("default_target_model")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
        {
            new.insert(
                "default_target".to_owned(),
                json!({"kind": "named", "name": name}),
            );
        }
        // `tool_approval` is not carried: the permission mode is per chat now.
        copy(old, "max_tool_iterations", &mut new, integer);
        copy(old, "skill_dirs", &mut new, array);
        ai.insert("agent".to_owned(), Value::Object(new));

        let mut search = Map::new();
        if let Some(backend) = old.get("search_backend").filter(|value| text(value)) {
            search.insert("backend".to_owned(), backend.clone());
        }
        copy(old, "searxng_base_url", &mut search, text);
        for (key, hint_key, secret_ref) in [
            ("tavily_api_key", "tavily_key_hint", SEARCH_TAVILY),
            ("brave_api_key", "brave_key_hint", SEARCH_BRAVE),
        ] {
            if let Some(value) = secret(old, key) {
                search.insert(hint_key.to_owned(), json!(mask_secret_hint(&value)));
                secrets.push((secret_ref, value));
            }
        }
        ai.insert("search".to_owned(), Value::Object(search));
    }

    LegacyAi {
        ai: Value::Object(ai),
        secrets,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_old_key_lands_in_its_new_place_and_credentials_leave_the_section() {
        let gateway = json!({
            "enabled_on_start": true, "port": 5000, "require_token": false,
            "token": "pdk_0123456789abcdef0123456789abcdef", "max_retries": 9,
            "first_byte_timeout_s": 1, "idle_timeout_s": 3600,
            "circuit_failure_threshold": 100, "circuit_open_s": 1
        });
        let models = json!({"catalog_auto_refresh": true, "default_test_prompt": "ping", "probe_concurrency": 8});
        let agent = json!({
            "default_target_model": "fast", "default_system_prompt": "be brief",
            "tool_approval": "auto_read_only", "max_tool_iterations": 7,
            "search_backend": "tavily", "tavily_api_key": "tvly-abcdefghijklmnop",
            "brave_api_key": "", "searxng_base_url": "https://s.example",
            "skill_dirs": ["~/skills"]
        });
        let out = ai_from_v1_sections(Some(&gateway), Some(&models), Some(&agent));
        assert_eq!(
            out.ai,
            json!({
                "gateway": {
                    "enabled_on_start": true, "port": 5000, "require_token": false,
                    "max_retries": 5, "first_byte_timeout_s": 5, "idle_timeout_s": 600,
                    "circuit_failure_threshold": 50, "circuit_open_s": 5
                },
                "providers": {"catalog_auto_refresh": true, "default_test_prompt": "ping", "probe_concurrency": 8},
                "agent": {
                    "default_target": {"kind": "named", "name": "fast"},
                    "max_tool_iterations": 7,
                    "skill_dirs": ["~/skills"]
                },
                "search": {
                    "backend": "tavily", "searxng_base_url": "https://s.example",
                    "tavily_key_hint": "…mnop"
                }
            })
        );
        assert_eq!(
            out.secrets,
            vec![
                (
                    LEGACY_GATEWAY_TOKEN,
                    "pdk_0123456789abcdef0123456789abcdef".to_owned()
                ),
                (SEARCH_TAVILY, "tvly-abcdefghijklmnop".to_owned()),
            ]
        );
        assert!(!out.ai.to_string().contains("be brief"));
        assert!(!out.ai.to_string().contains("tvly-abcdefghijklmnop"));
    }

    #[test]
    fn absent_rows_and_malformed_values_yield_nothing() {
        let out = ai_from_v1_sections(None, None, None);
        assert_eq!(out.ai, json!({}));
        assert!(out.secrets.is_empty());
        let junk = json!({"port": "x", "token": 5, "tool_approval": 1});
        let out = ai_from_v1_sections(Some(&junk), None, Some(&junk));
        assert_eq!(out.ai["gateway"], json!({}));
        assert!(out.secrets.is_empty());
    }
}
