#![forbid(unsafe_code)]

//! Writers that point AI coding tools at the local SpanBay gateway.
//!
//! Single responsibility: compute (`preview`), write (`apply`), check (`inspect`), undo
//! (`restore`) and re-token (`rewrite_token`) the user-level config of four tools so they talk to
//! the gateway, merging into whatever the user already has; plus the copyable blocks of the
//! snippet-only clients. Everything is pure over an explicit `home` directory (and, for backups,
//! a data folder) so tests run against temp dirs. The caller owns the database: [`apply`]
//! returns [`AppliedFile`] entries and the caller persists the ledger rows
//! (`llm_client_applied`).
//!
//! Keys written, verified against the vendors' current docs (2026-11-07). `GW` is the gateway
//! URL without a trailing slash, `TOKEN` the client's token, `ALIAS` the optional gateway model
//! id (alias or `provider/model`). `alias: None` (or blank) never touches any model key.
//!
//! | Tool | File | Keys |
//! |------|------|------|
//! | Claude Code | `~/.claude/settings.json` | `env.ANTHROPIC_BASE_URL = GW` (root, no `/v1`), `env.ANTHROPIC_AUTH_TOKEN = TOKEN`, `env.ANTHROPIC_MODEL`, `env.ANTHROPIC_DEFAULT_OPUS_MODEL`, `env.ANTHROPIC_DEFAULT_SONNET_MODEL`, `env.ANTHROPIC_DEFAULT_HAIKU_MODEL` = `ALIAS` (`ANTHROPIC_SMALL_FAST_MODEL` is deprecated and not written) |
//! | Codex CLI | `~/.codex/config.toml` | `model_provider = "spanbay"`, `model = ALIAS`, `[model_providers.spanbay]` with `name`, `base_url = GW/v1`, `wire_api = "responses"`, `experimental_bearer_token = TOKEN` (a stale `env_key` there is removed); with `ALIAS` also `model_context_window`, `model_max_output_tokens`, `model_supports_reasoning_summaries` from [`ModelFacts`] (an unknown fact removes its key) |
//! | OpenCode | `~/.config/opencode/opencode.json` | `provider.spanbay = {npm: "@ai-sdk/openai-compatible", name, options{baseURL: GW/v1, apiKey: TOKEN}, models{ALIAS: {name, limit{context, output}, reasoning, tool_call, attachment}}}` (facts fill only what the entry lacks), `model = "spanbay/ALIAS"` |
//! | Gemini CLI | `~/.gemini/settings.json`, `~/.gemini/.env` | `security.auth.selectedType = "gemini-api-key"`, `model.name = ALIAS`; `.env`: `GEMINI_API_KEY = TOKEN`, `GOOGLE_GEMINI_BASE_URL = GW` (root; the SDK appends `/v1beta`) |
//!
//! Sources: <https://code.claude.com/docs/en/env-vars>,
//! <https://developers.openai.com/codex/config-reference> and
//! <https://developers.openai.com/codex/config-advanced>, <https://opencode.ai/docs/providers/>
//! and <https://opencode.ai/docs/config/>,
//! <https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/configuration.md>.
//!
//! Decisions and limits:
//! - Codex gets the token through `experimental_bearer_token` in `config.toml`, so
//!   `~/.codex/auth.json` (which may hold the user's ChatGPT login) is never touched.
//! - OpenCode uses the `@ai-sdk/openai-compatible` provider, i.e. `/v1/chat/completions`; only
//!   `opencode.json` is handled, a sibling `opencode.jsonc` is not.
//! - JSON files are JSONC (comments, trailing commas) and are edited in place: only the values
//!   SpanBay sets change, so key order, comments and indentation stay. A new file is written with
//!   a 2-space indent. A file with any other syntax error is rejected with its line number, never
//!   overwritten. The Codex TOML keeps comments and order too.
//! - File writers always write the loopback address; only snippets may show LAN addresses.
//! - [`apply`] returns only the files it wrote; a second identical apply returns no files. A
//!   JSON file whose parsed content is already right is left byte for byte as it is.
//! - Backups go to `<data folder>/backups/<tool>/`, never beside the tool's config. Backups and
//!   the kept originals hold the user's own config, credentials included (plaintext, owner-only);
//!   [`prune_kept`] bounds them and [`purge_kept`] deletes them on request.

use std::{
    fmt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use spanbay_base::error::AppError;

mod backup;
mod claude_code;
mod clock;
mod codex;
mod detect;
mod document;
mod env;
mod fsio;
mod gemini_cli;
mod inspect;
mod mask;
mod opencode;
mod plan;
mod restore;
mod rewrite;
mod snippets;
mod write;

pub use backup::{KeptCopy, KeptKind, kept_copies, prune_kept, purge_kept};
pub use clock::Clock;
pub use detect::{
    BrokenLink, ConfigState, DetectEnv, DetectedClient, DetectedSnippetClient, detect_all,
    detect_snippet_clients,
};
pub use env::{override_source, recent_project_dirs};
pub use inspect::{
    ClientStatus, Drift, InspectContext, Inspection, NO_REQUEST_AFTER_MS, StatusFacts,
    client_status, config_models, inspect,
};
pub use mask::pending_token;
pub use plan::preview;
pub use restore::{RestoreEnv, RestoreOutcome, RevokedToken, restore, restore_original};
pub use rewrite::{RewriteReport, rewrite_model, rewrite_token};
pub use snippets::{
    NOTE_TITLE, SnippetBlock, SnippetClient, SnippetRequest, TOKEN_PLACEHOLDER, WARNING_TITLE,
    client_snippets, code_samples,
};
pub use write::{AppliedFile, ApplyResult, apply};

// The shared helpers, under the names the per-tool modules use (`super::X`).
use document::{
    child_object, display, display_in_home, edit_json, parse_json_object, parse_toml,
    read_json_object,
};
use fsio::read_optional;
use inspect::Found;
use plan::{Edit, Settings, check_gateway_url, check_token, non_blank, plan_edits};
use rewrite::Swap;

/// A supported AI coding tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClientApp {
    /// Anthropic Claude Code.
    #[serde(rename = "claude_code")]
    ClaudeCode,
    /// OpenAI Codex CLI.
    #[serde(rename = "codex")]
    Codex,
    /// OpenCode.
    #[serde(rename = "opencode")]
    OpenCode,
    /// Google Gemini CLI.
    #[serde(rename = "gemini_cli")]
    GeminiCli,
}

impl ClientApp {
    /// Every supported app, in display order.
    pub const ALL: [Self; 4] = [
        Self::ClaudeCode,
        Self::Codex,
        Self::OpenCode,
        Self::GeminiCli,
    ];

    /// Stable identifier (matches the serde name).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude_code",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::GeminiCli => "gemini_cli",
        }
    }

    /// The tool's product name.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
            Self::GeminiCli => "Gemini CLI",
        }
    }

    /// Name of the tool's command-line executable.
    const fn executable(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::GeminiCli => "gemini",
        }
    }

    /// Parses the identifier produced by [`ClientApp::as_str`].
    ///
    /// # Errors
    /// [`AppError::validation`] for an unknown identifier.
    pub fn parse(value: &str) -> Result<Self, AppError> {
        Self::ALL
            .into_iter()
            .find(|app| app.as_str() == value)
            .ok_or_else(|| AppError::validation(format!("unknown client app `{value}`")))
    }

    /// The tool's writer: the one place that maps a tool to its module.
    const fn writer(self) -> &'static Writer {
        match self {
            Self::ClaudeCode => &claude_code::WRITER,
            Self::Codex => &codex::WRITER,
            Self::OpenCode => &opencode::WRITER,
            Self::GeminiCli => &gemini_cli::WRITER,
        }
    }

    /// The tool's config directory under `home`.
    fn dir(self, home: &Path) -> PathBuf {
        (self.writer().dir)(home)
    }

    /// Every file this writer manages (existing or not).
    fn files(self, home: &Path) -> Vec<PathBuf> {
        (self.writer().files)(home)
    }
}

/// What each per-tool module provides; every module exports one as `WRITER`.
struct Writer {
    /// Path SpanBay appends to the gateway root in the address it writes (`/v1`, or nothing
    /// for a tool that appends its own paths).
    address_suffix: &'static str,
    /// The tool's config directory under `home`.
    dir: fn(&Path) -> PathBuf,
    /// Every file the writer manages (existing or not).
    files: fn(&Path) -> Vec<PathBuf>,
    /// The edits that point the tool at the gateway.
    render: fn(&Path, &Settings<'_>) -> Result<Vec<Edit>, AppError>,
    /// Reads back what `render` writes.
    inspect: fn(&Path) -> Result<Found, AppError>,
    /// The token currently in the config, trimmed; `None` when absent or blank.
    written_token: fn(&Path) -> Result<Option<String>, AppError>,
    /// Swaps the token in one managed file: `(home, path, text, old_sha256, new_token)`.
    retoken: fn(&Path, &Path, &str, &str, &str) -> Result<Swap, AppError>,
}

/// What to configure and where it should point. Never serialised; `Debug` hides the token.
#[derive(Clone, PartialEq)]
pub struct ClientConfigRequest {
    /// Tool to configure.
    pub app: ClientApp,
    /// Name of the gateway client the token belongs to; echoed in the plan and the result so the
    /// caller can match them to the client record.
    pub client_name: String,
    /// Gateway base URL, e.g. `http://127.0.0.1:4141`.
    pub gateway_url: String,
    /// Token the tool presents to the gateway. For a preview made before the client exists use
    /// [`pending_token`]; such a token must never be applied.
    pub token: String,
    /// Gateway model id (alias or `provider/model`); `None` leaves the tool's model untouched.
    pub alias: Option<String>,
    /// Claude Code only: the alias for background tasks (`ANTHROPIC_DEFAULT_HAIKU_MODEL`);
    /// `None` uses `alias`. Other tools ignore it.
    pub fast_alias: Option<String>,
    /// What the gateway knows about `alias`; Codex and OpenCode get it as capability hints.
    pub facts: ModelFacts,
}

/// What the gateway knows about a model, written as capability hints by the tools that cannot
/// tell a `provider/model` name's family: Codex (`model_context_window`,
/// `model_max_output_tokens`, `model_supports_reasoning_summaries`) and OpenCode (the model
/// entry's `limit`, `reasoning`, `tool_call` and `attachment`). `None` is unknown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModelFacts {
    /// Context window in tokens.
    pub context_window: Option<u64>,
    /// Largest output in tokens.
    pub max_output_tokens: Option<u64>,
    /// Produces reasoning output.
    pub reasoning: Option<bool>,
    /// Calls tools.
    pub tool_call: Option<bool>,
    /// Accepts image input.
    pub attachment: Option<bool>,
}

impl fmt::Debug for ClientConfigRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConfigRequest")
            .field("app", &self.app)
            .field("client_name", &self.client_name)
            .field("gateway_url", &self.gateway_url)
            .field("token", &"<redacted>")
            .field("alias", &self.alias)
            .field("fast_alias", &self.fast_alias)
            .field("facts", &self.facts)
            .finish()
    }
}

/// The planned change to one file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilePlan {
    /// Absolute display path.
    pub path: String,
    /// Current content (token masked), `None` when the file does not exist.
    pub before: Option<String>,
    /// Content after the merge (token masked).
    pub after: String,
    /// `true` when `after` differs from `before`.
    pub changed: bool,
    /// `true` when the file does not exist yet (tag "New file"); an unchanged file is tagged
    /// "No changes".
    pub is_new: bool,
    /// Unified diff (`--- a/<path>` / `+++ b/<path>`, token masked), empty when unchanged.
    pub diff: String,
    /// SHA-256 (lower-case hex) of the current file, `None` for a new file. [`apply`] refuses a
    /// write when the file no longer has this hash (it changed after the preview).
    pub base_sha256: Option<String>,
    /// Where the write really lands when a symbolic link (the file, or a folder on the way) leads
    /// elsewhere, e.g. into a dotfiles repository; `None` when it lands at `path`.
    pub real_path: Option<String>,
    /// The Git work tree that holds the file the write lands in, `None` outside one: the token
    /// written there could be committed by accident.
    pub git_work_tree: Option<String>,
}

/// The planned changes for one tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientConfigPlan {
    /// The tool.
    pub app: ClientApp,
    /// The gateway client the plan is for.
    pub client_name: String,
    /// One entry per managed file (changed or not).
    pub files: Vec<FilePlan>,
}

/// Resolves the user's home directory from `$HOME` (`%USERPROFILE%` on Windows).
///
/// # Errors
/// [`AppError::internal`] when neither variable is set to a non-empty value.
pub fn home_dir() -> Result<PathBuf, AppError> {
    ["HOME", "USERPROFILE"]
        .into_iter()
        .filter_map(std::env::var_os)
        .find(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| AppError::internal("unable to determine the home directory"))
}

/// The token currently in `app`'s config file (the field its writer sets), trimmed; `None` when
/// the file or field is absent or blank. For core-only use, such as sending one loopback test
/// request with the client's own token: it is never serialised and must not be logged.
///
/// # Errors
/// [`AppError::validation`] for a file that cannot be parsed; [`AppError::io`] when it cannot be
/// read.
pub fn read_written_token(home: &Path, app: ClientApp) -> Result<Option<String>, AppError> {
    (app.writer().written_token)(home)
}

#[cfg(test)]
mod test_util;
#[cfg(test)]
mod tests;
