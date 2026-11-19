//! Secret storage, masking and redaction.
//!
//! Single responsibility: keep every credential (provider keys, the gateway token, search keys,
//! MCP environment values) out of DTOs, settings rows and logs. Values live in the `secrets`
//! table of the `0600` database behind the [`SecretStore`] trait; everything else refers to them
//! by [`SecretRef`] and shows only a [`SecretHint`]. [`redact_secrets`] scrubs known values and
//! credential headers from text that is stored or shown (captures, probe messages, tool output).
//!
//! Locking rule: the SQLite impl (`pingdeck_store::secrets_sqlite`) shares the application store
//! called while a caller already holds `AppState::store()`.

mod redact;
#[cfg(test)]
mod tests;

pub use redact::{
    CLIENT_TOKEN_PREFIX, ELIDED, MIN_OPAQUE_SEGMENT_CHARS, MIN_REDACTION_CHARS, REDACT_HEADERS,
    REDACT_JSON_KEYS, REDACT_QUERY_KEYS, REDACTED, is_secret_header_name, is_token_shaped,
    mask_secrets, redact_json, redact_known_values, redact_secrets, redact_url_path,
};

use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// Secret reference the schema-1 master gateway token is parked under until migration v24 turns it
/// into a per-client gateway token (a hash) and deletes it; nothing else reads it.
pub const LEGACY_GATEWAY_TOKEN: &str = "ai.gateway.token";
/// Secret reference of the Tavily search API key.
pub const SEARCH_TAVILY: &str = "ai.search.tavily";
/// Secret reference of the Brave Search API key.
pub const SEARCH_BRAVE: &str = "ai.search.brave";

/// Stable logical key in the secret store (e.g. `"llm.key.k_123"`, `"ai.gateway.token"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SecretRef(pub String);

impl SecretRef {
    /// The secret of provider key `key_id`.
    #[must_use]
    pub fn llm_key(key_id: &str) -> Self {
        Self(format!("llm.key.{key_id}"))
    }

    /// The secret of environment variable `name` of MCP server `server_id`.
    #[must_use]
    pub fn mcp_env(server_id: &str, name: &str) -> Self {
        Self(format!("mcp.{server_id}.env.{name}"))
    }

    /// The secret of header `name` of MCP server `server_id`.
    #[must_use]
    pub fn mcp_header(server_id: &str, name: &str) -> Self {
        Self(format!("mcp.{server_id}.header.{name}"))
    }

    /// The reference as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The `{{secret:KEY}}` placeholder that stands for this secret inside stored config text.
    #[must_use]
    pub fn placeholder(&self) -> String {
        format!("{{{{secret:{}}}}}", self.0)
    }
}

/// Masked summary returned to the UI (`"sk-…9f2a"` or `"••••"`). Never contains the raw secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretHint {
    /// The reference this summarises.
    pub secret_ref: String,
    /// Whether a non-empty value is stored.
    pub has_secret: bool,
    /// Masked form; empty when nothing is stored.
    pub hint: String,
    /// When the value last changed, in Unix milliseconds (0 when nothing is stored).
    pub updated_at_ms: i64,
}

/// Pluggable secret storage; the SQLite impl lives in `pingdeck_store`.
pub trait SecretStore: Send + Sync {
    /// The stored value, if any.
    ///
    /// # Errors
    /// [`AppError::Database`] when the store cannot be read.
    fn get(&self, secret_ref: &str) -> Result<Option<String>, AppError>;

    /// Stores `plaintext` (replacing any value) and returns its masked hint.
    ///
    /// # Errors
    /// [`AppError::Database`] when the store cannot be written.
    fn put(&self, secret_ref: &str, plaintext: &str) -> Result<SecretHint, AppError>;

    /// Removes the value; returns whether it existed.
    ///
    /// # Errors
    /// [`AppError::Database`] when the store cannot be written.
    fn delete(&self, secret_ref: &str) -> Result<bool, AppError>;

    /// The masked summary of `secret_ref` (`has_secret == false` when nothing is stored).
    ///
    /// # Errors
    /// [`AppError::Database`] when the store cannot be read.
    fn hint(&self, secret_ref: &str) -> Result<SecretHint, AppError>;

    /// Every stored value of at least [`MIN_REDACTION_CHARS`] characters, for [`redact_secrets`].
    /// A store that cannot be read yields none (redaction is best effort; the header rules still
    /// apply).
    fn active_redaction_needles(&self) -> Vec<String>;
}

/// Masked form of a secret for display, empty for none: `••••` below 12 characters, the last four
/// (`…9f2a`) below 24, and the first three plus the last four (`sk-…9f2a`) from 24 on, so a hint
/// never gives away a large share of a short secret.
#[must_use]
pub fn mask_secret_hint(secret: &str) -> String {
    let secret = secret.trim();
    let length = secret.chars().count();
    if length == 0 {
        return String::new();
    }
    if length < 12 {
        return "••••".to_owned();
    }
    let tail: String = secret.chars().skip(length - 4).collect();
    if length < 24 {
        return format!("…{tail}");
    }
    let head: String = secret.chars().take(3).collect();
    format!("{head}…{tail}")
}

/// Replaces each `{{secret:KEY}}` in `text` with the stored value of `KEY`.
///
/// # Errors
/// [`AppError::Validation`] when a placeholder names a secret that is not stored (a missing
/// credential must stop the call, not send an empty one); [`AppError::Database`] on read failure.
pub fn resolve_placeholders(store: &dyn SecretStore, text: &str) -> Result<String, AppError> {
    const OPEN: &str = "{{secret:";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return Ok(out);
        };
        let key = &after[..end];
        let value = store.get(key)?.ok_or_else(|| {
            AppError::validation(format!("secret `{key}` is referenced but not stored"))
        })?;
        out.push_str(&value);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}
