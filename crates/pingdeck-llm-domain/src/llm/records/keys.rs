//! Provider key DTOs and runtime/test/listing states.

use serde::{Deserialize, Serialize};

use crate::llm::balance_state::StoredBalance;
use crate::llm::failure::{FailureKind, FailureScope};
use crate::llm::wire::Protocol;

/// One credential of a provider, as the UI sees it: the secret itself is write-only and lives in
/// the secret store under [`pingdeck_base::secrets::SecretRef::llm_key`]; only a masked hint is
/// shown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderKey {
    pub id: String,
    pub provider_id: String,
    pub name: String,
    /// Masked form of the secret (`sk-…9f2a`); empty when none is stored.
    #[serde(default)]
    pub secret_hint: String,
    pub enabled: bool,
    #[serde(default)]
    pub position: i64,
    #[serde(default)]
    pub created_at_ms: i64,
    /// Last change; the version stamp `save_key_guarded` checks.
    #[serde(default)]
    pub updated_at_ms: i64,
    /// The account id some gateways need beside the key (`New-Api-User`), empty when unused.
    #[serde(default)]
    pub api_user_id: Option<String>,
    /// The last good balance report and the last failure of a refresh. Read-only here: only
    /// `set_key_balance` writes it.
    #[serde(default)]
    pub balance: Option<StoredBalance>,
    /// When the balance was last attempted, success or failure (throttles refreshes).
    #[serde(default)]
    pub balance_attempt_ms: Option<i64>,
    /// Whether a management (console) token is stored. Read-only: `set_key_management_token`
    /// writes it, and the token is never returned.
    #[serde(default)]
    pub has_management_token: bool,
    /// Masked form of the management token; empty when none is stored.
    #[serde(default)]
    pub management_token_hint: String,
    /// The outcome of the last "Test key"; `None` when never tested. Server-owned: cleared when
    /// the secret changes; no writer is wired yet.
    #[serde(default)]
    pub last_test: Option<KeyTest>,
    /// Free-form notes (at most 200 characters); a save with `None` keeps the stored ones. Every
    /// loaded key has them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Until when (Unix ms) the key rests after a rejection or an exhausted account, else until
    /// the soonest rate-limit rest of one of its models ends (the key still serves the others);
    /// only while it does. Runtime state of this session, never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_until_ms: Option<i64>,
    /// HTTP status of the key's last failed call this session (absent for transport errors).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_status: Option<u16>,
    /// One redacted line about the key's last failed call this session; never the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// When the last failed call happened (Unix ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_ms: Option<i64>,
    /// When the key was last called: this session's last call, else the request log's newest row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_ms: Option<i64>,
    /// The group the user filed the key under (relays such as sub2api and new-api bind each key
    /// to a group), display only; a save with `None` keeps the stored one. Every loaded key has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_name: Option<String>,
    /// The protocols the user limited the key to; absent when not limited. A save with `None`
    /// keeps the stored limit, `Some([])` removes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocols: Option<Vec<Protocol>>,
    /// The native protocol the key's model list suggests (read-only, a preference, never a limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_protocol: Option<Protocol>,
    /// When the key's own model list was last read (read-only); absent: never, so the key may
    /// call every model of the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models_listed_at_ms: Option<i64>,
    /// How many models the key's own list returned (read-only), while it was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_count: Option<u32>,
    /// The key's relay group (read-only): a stable hash of the models its own list
    /// returned, equal for keys that list the same models; absent while the list was never read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_key: Option<String>,
    /// The key's run state this session (read-only, in memory).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<KeyRuntime>,
    /// Where reading the key's own model list stands (read-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models_listing: Option<ModelsListing>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyRunState {
    Ready,
    /// Resting after a failure until `until_ms`.
    Cooling,
    /// Some model's circuit on this key is open.
    CircuitOpen,
    Disabled,
}

/// A key's run state with the failure behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyRuntime {
    pub state: KeyRunState,
    /// What the last failure was, when one happened this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<FailureKind>,
    /// What the last failure was about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<FailureScope>,
    /// When the cooldown or the earliest open circuit ends, Unix ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelsListingState {
    /// Never read: the key may call every model.
    Unread,
    /// A read is under way (after the key was added or its value replaced).
    Reading,
    Listed,
    /// The last read failed; an earlier list (if any) still counts.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsListing {
    pub state: ModelsListingState,
    /// When the list was last read, Unix ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<i64>,
    /// The error code of a failed read (`model_list_unauthorized`, …), when it had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyVerdict {
    Accepted,
    /// The provider answered 429: the key works but is limited right now.
    RateLimited,
    /// The provider rejected the key, or the test could not reach it (see the message).
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyTest {
    pub verdict: KeyVerdict,
    pub at_ms: i64,
    /// The HTTP status, when the provider answered.
    #[serde(default)]
    pub status: Option<u16>,
    /// One line in words; never the key.
    #[serde(default)]
    pub message: String,
}

impl Default for ProviderKey {
    /// An empty enabled key (callers fill what they know).
    fn default() -> Self {
        Self {
            id: String::new(),
            provider_id: String::new(),
            name: String::new(),
            secret_hint: String::new(),
            enabled: true,
            position: 0,
            created_at_ms: 0,
            updated_at_ms: 0,
            api_user_id: None,
            balance: None,
            balance_attempt_ms: None,
            has_management_token: false,
            management_token_hint: String::new(),
            last_test: None,
            notes: None,
            cooldown_until_ms: None,
            last_error_status: None,
            last_error: None,
            last_error_ms: None,
            last_used_ms: None,
            group_name: None,
            protocols: None,
            preferred_protocol: None,
            models_listed_at_ms: None,
            model_count: None,
            group_key: None,
            runtime: None,
            models_listing: None,
        }
    }
}
