//! Gateway client DTOs, Base62 CSPRNG token minting and constant-time token matching.

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use spanbay_base::crypto::{constant_time_eq, fill_random, sha256_hex};
use spanbay_base::error::AppError;
use spanbay_base::secrets::CLIENT_TOKEN_PREFIX;

/// Most client records, active and revoked.
pub const MAX_CLIENTS: usize = 64;
/// Most characters of a client name (after trimming).
pub const MAX_NAME_CHARS: usize = 40;
/// Most allow-list entries, aliases and models together; bounds the stored row.
pub const MAX_ALLOW_ENTRIES: usize = 256;
/// Most characters of one allow-list entry (after trimming).
const MAX_ENTRY_CHARS: usize = 200;
/// `last_used_ms` is written at most this often per client.
pub const TOUCH_INTERVAL_MS: i64 = 30_000;
/// The id of the client migrated from the legacy gateway token.
pub const MIGRATED_CLIENT_ID: &str = "gwc_default";

/// Characters of the random part of a token (`32 * log2(62)` = 190 bits).
const TOKEN_RANDOM_CHARS: usize = 32;
/// Characters of the random part shown as the token's last four.
const TOKEN_LAST_CHARS: usize = 4;
const BASE62: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
/// Largest multiple of 62 that fits a byte; bytes at or above it are rejected (no modulo bias).
const BASE62_ACCEPT_BELOW: u8 = 248;
const _: () = assert!(BASE62_ACCEPT_BELOW as usize == 256 / BASE62.len() * BASE62.len());

/// A gateway client record (the token itself is never part of it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayClient {
    /// `cl_` and 12 hex characters (`gwc_default` for the client migrated from the legacy token).
    pub id: String,
    /// Display name, unique case-insensitively among all records.
    pub name: String,
    /// Display prefix of the token, e.g. `pd_cc_` (legacy: the first characters of the old token).
    pub token_prefix: String,
    /// Last four characters of the token (empty for a legacy client).
    pub token_last4: String,
    /// `true`: every alias and `provider/model` id is allowed.
    pub allow_all: bool,
    /// Alias names allowed when `allow_all` is `false`.
    pub allowed_aliases: Vec<String>,
    /// `provider_id/model_id` ids allowed when `allow_all` is `false`.
    pub allowed_models: Vec<String>,
    /// Unix ms.
    pub created_at_ms: i64,
    /// When the token last authenticated a request (Unix ms; written at most every 30 s).
    pub last_used_ms: Option<i64>,
    /// Set when revoked; the row stays for log attribution.
    pub revoked_at_ms: Option<i64>,
    /// The client made from the legacy single gateway token ([`MIGRATED_CLIENT_ID`]); a client the
    /// user named "Default" is not.
    #[serde(default)]
    pub migrated: bool,
    /// The model a tool's config was last connected with (`provider_id/model`).
    #[serde(default)]
    pub default_model: Option<String>,
    /// A model name without a provider that resolves to nothing is served by `default_model`
    /// (Claude Code's background tier).
    #[serde(default)]
    pub unknown_model_to_default: bool,
    /// Optional per-client limits (rate, concurrency, spend, tokens, expiry); all off by default.
    #[serde(default)]
    pub limits: ClientLimits,
}

/// Most requests per minute a client limit may name.
pub const MAX_LIMIT_RPM: u32 = 100_000;
/// Most concurrent requests a client limit may name.
pub const MAX_LIMIT_CONCURRENT: u32 = 1_024;

/// Per-client gateway limits. Every field is optional; `None` means no limit. Spend is counted in
/// `budget_currency` (the currency of the price that priced each call): calls priced in another
/// currency, or without a price, count toward the token limits only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientLimits {
    /// Requests per rolling minute; over it the gateway answers 429 with `Retry-After`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpm: Option<u32>,
    /// Requests in flight at once; over it the gateway answers 429.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<u32>,
    /// Spend per local day, in millionths of `budget_currency`; over it the gateway answers 402.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_budget_micro: Option<i64>,
    /// Spend per local calendar month, in millionths of `budget_currency`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly_budget_micro: Option<i64>,
    /// ISO code the budgets are in; empty means `USD`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub budget_currency: String,
    /// Input plus output tokens per local day (also for models without a known price).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_tokens: Option<u64>,
    /// Input plus output tokens per local calendar month.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly_tokens: Option<u64>,
    /// When the token stops working (Unix ms); a request after it gets 401.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
}

impl ClientLimits {
    /// Whether no limit is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Whether a spend or token limit is set (the gateway then reads the client's usage).
    #[must_use]
    pub const fn counts_usage(&self) -> bool {
        self.daily_budget_micro.is_some()
            || self.monthly_budget_micro.is_some()
            || self.daily_tokens.is_some()
            || self.monthly_tokens.is_some()
    }

    /// The currency the budgets are in.
    #[must_use]
    pub fn currency(&self) -> &str {
        if self.budget_currency.is_empty() {
            "USD"
        } else {
            &self.budget_currency
        }
    }

    /// Whether the token has expired at `now_ms`.
    #[must_use]
    pub fn expired(&self, now_ms: i64) -> bool {
        self.expires_at_ms.is_some_and(|at| now_ms >= at)
    }
}

/// What the client's limits editor saves: the limits and the unknown-name mapping switch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientPolicyInput {
    /// The limits; an empty value turns every limit off.
    #[serde(default)]
    pub limits: ClientLimits,
    /// New value of `unknown_model_to_default`; `None` keeps it.
    #[serde(default)]
    pub unknown_model_to_default: Option<bool>,
}

/// Validates `limits` before persistence: positive counts within bounds, a three-letter
/// currency, non-negative budgets.
///
/// # Errors
/// [`AppError::Validation`] naming the first field out of range.
pub fn check_client_limits(limits: &ClientLimits) -> Result<ClientLimits, AppError> {
    let mut checked = limits.clone();
    if checked
        .rpm
        .is_some_and(|rpm| rpm == 0 || rpm > MAX_LIMIT_RPM)
    {
        return Err(AppError::validation(format!(
            "Requests per minute must be 1-{MAX_LIMIT_RPM}"
        )));
    }
    if checked
        .max_concurrent
        .is_some_and(|max| max == 0 || max > MAX_LIMIT_CONCURRENT)
    {
        return Err(AppError::validation(format!(
            "Concurrent requests must be 1-{MAX_LIMIT_CONCURRENT}"
        )));
    }
    if [checked.daily_budget_micro, checked.monthly_budget_micro]
        .iter()
        .flatten()
        .any(|budget| *budget < 0)
    {
        return Err(AppError::validation("A budget cannot be negative"));
    }
    if [checked.daily_tokens, checked.monthly_tokens]
        .iter()
        .flatten()
        .any(|tokens| *tokens == 0)
    {
        return Err(AppError::validation("A token limit must be at least 1"));
    }
    let currency = checked.budget_currency.trim().to_ascii_uppercase();
    if !currency.is_empty()
        && (currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()))
    {
        return Err(AppError::validation(
            "The budget currency must be a three-letter code such as USD",
        ));
    }
    checked.budget_currency = if currency == "USD" {
        String::new()
    } else {
        currency
    };
    Ok(checked)
}

impl GatewayClient {
    /// Whether the client may call `requested`, an alias name or a `provider/model` id.
    /// `allow_all` allows everything, direct ids included.
    #[must_use]
    pub fn allows(&self, requested: &str) -> bool {
        self.allow_all
            || self
                .allowed_aliases
                .iter()
                .chain(&self.allowed_models)
                .any(|allowed| allowed == requested)
    }

    /// The model to serve `requested` with instead: `default_model` when the client maps unknown
    /// names and `requested` is a bare name `resolves` does not know.
    #[must_use]
    pub fn unknown_model_target(
        &self,
        requested: &str,
        resolves: &dyn Fn(&str) -> bool,
    ) -> Option<&str> {
        let requested = requested.trim();
        self.default_model.as_deref().filter(|_| {
            self.unknown_model_to_default
                && !requested.is_empty()
                && !requested.contains('/')
                && !resolves(requested)
        })
    }

    /// Whether the client is not revoked.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.revoked_at_ms.is_none()
    }
}

/// What the user chooses when creating or editing a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInput {
    /// Display name (trimmed, 1-40 characters, unique case-insensitively).
    pub name: String,
    pub allow_all: bool,
    /// Alias names allowed when `allow_all` is `false`.
    #[serde(default)]
    pub allowed_aliases: Vec<String>,
    /// `provider/model` ids allowed when `allow_all` is `false`.
    #[serde(default)]
    pub allowed_models: Vec<String>,
}

/// A client with its fresh plaintext token, returned once by create and rotate.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientWithToken {
    pub client: GatewayClient,
    /// The full token; only its SHA-256 is stored.
    pub token: String,
}

impl fmt::Debug for ClientWithToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientWithToken")
            .field("client", &self.client)
            .field("token", &"[redacted]")
            .finish()
    }
}

/// One client with the SHA-256 of its token: what the gateway caches to authenticate requests
/// without a query each (refresh it after a client changed). `PartialEq` compares the hash in
/// variable time: use it for cache diffs only, never to authenticate (see [`match_token`]).
#[derive(Clone, PartialEq, Eq)]
pub struct ClientHashEntry {
    /// Lower-case hex SHA-256 of the token.
    pub token_sha256: String,
    pub client: GatewayClient,
}

impl fmt::Debug for ClientHashEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientHashEntry")
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

/// The outcome of looking a presented token up.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "a short-lived answer moved straight into the request's identity; boxing would add \
              an allocation per authenticated request"
)]
pub enum TokenLookup {
    Active(GatewayClient),
    /// A known token of a revoked client.
    Revoked,
    /// No client has this token.
    Unknown,
}

/// `chars` characters of `[A-Za-z0-9]` drawn from `fill` by rejection sampling: a byte of 248 or
/// more is dropped, so every character has exactly the same probability.
fn base62_from(
    chars: usize,
    fill: &mut dyn FnMut(&mut [u8]) -> Result<(), AppError>,
) -> Result<String, AppError> {
    let mut out = String::with_capacity(chars);
    let mut bytes = [0_u8; 64];
    while out.len() < chars {
        fill(&mut bytes)?;
        for &byte in bytes.iter().filter(|byte| **byte < BASE62_ACCEPT_BELOW) {
            if out.len() == chars {
                break;
            }
            out.push(char::from(BASE62[usize::from(byte) % BASE62.len()]));
        }
    }
    Ok(out)
}

/// The recognisable tag of a token (`cc` for Claude Code, `cx` Codex, `zd` Zed, else the first
/// two ASCII letters of the name padded with `x`).
#[must_use]
pub fn token_tag(name: &str) -> String {
    let first_word = name.split_whitespace().next().unwrap_or("");
    match first_word.to_ascii_lowercase().as_str() {
        "claude" => "cc".to_owned(),
        "codex" => "cx".to_owned(),
        "zed" => "zd".to_owned(),
        _ => {
            let mut tag: String = name
                .chars()
                .filter(char::is_ascii_alphabetic)
                .take(2)
                .map(|letter| letter.to_ascii_lowercase())
                .collect();
            while tag.len() < 2 {
                tag.push('x');
            }
            tag
        }
    }
}

/// A freshly minted token that no store holds yet (see [`mint_token`]). `Debug` hides the token.
pub struct MintedToken {
    /// The full token; hand it to the files first, then to `commit_rotation`.
    pub token: String,
    prefix: String,
    last4: String,
}

impl MintedToken {
    /// Display prefix (`pd_<tag>_`).
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Last four characters of the random token body.
    #[must_use]
    pub fn last4(&self) -> &str {
        &self.last4
    }
}

impl fmt::Debug for MintedToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MintedToken")
            .field("token", &"[redacted]")
            .finish_non_exhaustive()
    }
}

/// A new token for the client called `name`, not stored anywhere: the first step of a rotation
/// that writes files before it commits (`commit_rotation`).
///
/// # Errors
/// [`AppError::Internal`] without OS randomness.
pub fn mint_token(name: &str) -> Result<MintedToken, AppError> {
    let random = base62_from(TOKEN_RANDOM_CHARS, &mut fill_random)?;
    let prefix = format!("{CLIENT_TOKEN_PREFIX}{}_", token_tag(name));
    let last4 = random
        .chars()
        .skip(TOKEN_RANDOM_CHARS - TOKEN_LAST_CHARS)
        .collect();
    Ok(MintedToken {
        token: format!("{prefix}{random}"),
        prefix,
        last4,
    })
}

/// A validated [`ClientInput`]: trimmed name and cleaned lists.
pub struct CheckedClientInput {
    pub name: String,
    pub allow_all: bool,
    pub aliases: Vec<String>,
    pub models: Vec<String>,
}

/// The trimmed, non-empty entries in first-seen order without duplicates. Empty and duplicate
/// entries are skipped before an entry is checked.
fn clean_list(entries: &[String]) -> Result<Vec<String>, AppError> {
    let mut seen: HashSet<&str> = HashSet::with_capacity(entries.len());
    let mut out: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry.trim();
        if entry.is_empty() || !seen.insert(entry) {
            continue;
        }
        if entry.chars().count() > MAX_ENTRY_CHARS || entry.chars().any(char::is_control) {
            return Err(AppError::validation(
                "An allow-list entry is too long or contains control characters",
            ));
        }
        out.push(entry.to_owned());
    }
    Ok(out)
}

/// Validates and normalizes `input` before persistence.
///
/// # Errors
/// [`AppError::Validation`] on invalid name, control chars, or empty/oversized allow lists.
pub fn check_client_input(input: &ClientInput) -> Result<CheckedClientInput, AppError> {
    let name = input.name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(AppError::validation(format!(
            "A client name must be 1-{MAX_NAME_CHARS} characters without control characters"
        )));
    }
    let aliases = clean_list(&input.allowed_aliases)?;
    let models = clean_list(&input.allowed_models)?;
    if !input.allow_all && aliases.is_empty() && models.is_empty() {
        return Err(AppError::validation(
            "Pick at least one alias or model, or choose All",
        ));
    }
    if aliases.len() + models.len() > MAX_ALLOW_ENTRIES {
        return Err(AppError::validation(format!(
            "A client allow-list holds at most {MAX_ALLOW_ENTRIES} entries"
        )));
    }
    Ok(CheckedClientInput {
        name: name.to_owned(),
        allow_all: input.allow_all,
        aliases,
        models,
    })
}

/// Looks `presented` up among `entries`: its SHA-256 is computed once and compared with every
/// hash in constant time, without leaving the loop early; a revoked match is reported as such.
#[must_use]
pub fn match_token(entries: &[ClientHashEntry], presented: &str) -> TokenLookup {
    let digest = sha256_hex(presented.as_bytes());
    let mut found: Option<&ClientHashEntry> = None;
    for entry in entries {
        if constant_time_eq(entry.token_sha256.as_bytes(), digest.as_bytes()) {
            found = Some(entry);
        }
    }
    match found {
        Some(entry) if entry.client.is_active() => TokenLookup::Active(entry.client.clone()),
        Some(_) => TokenLookup::Revoked,
        None => TokenLookup::Unknown,
    }
}

#[cfg(test)]
mod tests;
