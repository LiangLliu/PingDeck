//! About, diagnostics, workspace counts and the settings registry.
//!
//! Single responsibility: read-only facts about this installation for Settings and the workspace
//! switcher. Nothing here writes user data. The diagnostics text is built from an allow-list: a
//! setting is shown by value only when it is a number, a switch or one of a fixed set of words;
//! every free text, list and object is reduced to "set", "empty" or a count, so a new setting
//! can never leak into a copied report by default.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::agent::sandbox::SandboxKind;
use crate::error::AppError;
use crate::platform::normalize_display_path;
use crate::secrets::redact_secrets;
use crate::settings::Settings;
use crate::settings::registry::{RegistryKind, SettingsRegistryDto, settings_registry};
use crate::workspace::WorkspaceId;
use crate::workspace::hierarchy::WorkspaceCounts;

use super::{AppState, command_error, ensure_workspace_exists};

/// Where the secret store keeps its values (the file-backed default; an OS credential store
/// would report its own name).
const SECRET_STORE_BACKEND: &str = "sqlite";

/// The reference the secret-store health probe writes and deletes again.
const SECRET_PROBE_REF: &str = "app.health_probe";

/// What only the shell knows: the app's own version and the webview in use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellInfo {
    /// The packaged app version.
    pub app_version: String,
    /// The webview engine version.
    pub webview_version: String,
}

/// Whether secrets can be read and written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretStoreState {
    /// Where the values live (`sqlite`: a file only the user can read).
    pub backend: String,
    /// A probe value was stored, read back and removed.
    pub ok: bool,
    /// Why the probe failed; absent when `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// How the sandbox self-test went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxSelftest {
    /// The sandbox ran a trivial command.
    Passed,
    /// The sandbox tool is installed but cannot apply its profile here.
    Failed,
    /// There is no sandbox on this platform, so nothing was tested.
    NotRun,
}

/// The shell sandbox of this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxInfo {
    /// The tier whose tool is installed.
    pub tier: SandboxKind,
    /// Whether that tier works.
    pub selftest: SandboxSelftest,
}

/// The About page: versions, places and what the machine can do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfoDto {
    /// The packaged app version.
    pub app_version: String,
    /// The version of the Rust core.
    pub core_version: String,
    /// The database schema version.
    pub schema_version: i64,
    /// The data folder, display-normalised; empty for an in-memory state.
    pub data_dir: String,
    /// The database file, display-normalised; empty for an in-memory state.
    pub db_path: String,
    /// The operating system family (`macos`, `linux`, `windows`).
    pub os: String,
    /// The CPU architecture (`aarch64`, `x86_64`).
    pub arch: String,
    /// The webview engine version.
    pub webview_version: String,
    /// Whether secrets can be stored.
    pub secret_store: SecretStoreState,
    /// The shell sandbox tier and its self-test.
    pub sandbox: SandboxInfo,
    /// The release notes page of the project; empty while the repository URL is not set in the
    /// workspace package metadata (`repository` in `Cargo.toml`).
    pub release_notes_url: String,
    /// The issue tracker of the project; empty like [`AppInfoDto::release_notes_url`].
    pub issue_tracker_url: String,
}

/// The project's repository URL as Cargo records it; empty while the workspace package sets none.
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// `path` below the repository URL `repository` (no URL: empty, so the UI shows no link).
fn link_under(repository: &str, path: &str) -> String {
    let base = repository.trim().trim_end_matches('/');
    if base.is_empty() {
        String::new()
    } else {
        format!("{base}/{path}")
    }
}

/// Stores, reads back and removes a probe value, so a broken secret store is seen before a key
/// is typed into it. Call it without holding the store lock.
#[must_use]
pub fn secret_store_state(state: &AppState) -> SecretStoreState {
    let secrets = state.secrets();
    let probe = || -> Result<(), AppError> {
        secrets.put(SECRET_PROBE_REF, "probe")?;
        let read = secrets.get(SECRET_PROBE_REF);
        secrets.delete(SECRET_PROBE_REF)?;
        match read? {
            Some(value) if value == "probe" => Ok(()),
            _ => Err(AppError::internal(
                "the secret store returned another value than the one stored",
            )),
        }
    };
    match probe() {
        Ok(()) => SecretStoreState {
            backend: SECRET_STORE_BACKEND.to_owned(),
            ok: true,
            error: None,
        },
        Err(error) => SecretStoreState {
            backend: SECRET_STORE_BACKEND.to_owned(),
            ok: false,
            error: Some(error.message().to_owned()),
        },
    }
}

/// The sandbox tier installed here and whether it works. The first call runs the self-test (a
/// few milliseconds, blocking); the answer is cached for the process.
#[must_use]
pub fn sandbox_info() -> SandboxInfo {
    let tier = crate::agent::sandbox::detect_kind();
    let selftest = match (tier, crate::agent::sandbox::detect_checked_kind()) {
        (SandboxKind::None, _) => SandboxSelftest::NotRun,
        (installed, working) if installed == working => SandboxSelftest::Passed,
        _ => SandboxSelftest::Failed,
    };
    SandboxInfo { tier, selftest }
}

/// Versions, places, secret store and sandbox of this installation. Blocking: it writes and
/// removes one probe value and may run the sandbox self-test.
///
/// # Errors
/// [`AppError::Database`] when the schema version cannot be read.
pub fn app_info_impl(state: &AppState, shell: &ShellInfo) -> Result<AppInfoDto, AppError> {
    let schema_version = state.store().schema_version().map_err(command_error)?;
    let (data_dir, db_path) = state.data_dirs().map_or_else(
        || (String::new(), String::new()),
        |dirs| {
            (
                normalize_display_path(&dirs.root),
                normalize_display_path(&dirs.db),
            )
        },
    );
    Ok(AppInfoDto {
        app_version: shell.app_version.clone(),
        core_version: env!("CARGO_PKG_VERSION").to_owned(),
        schema_version,
        data_dir,
        db_path,
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        webview_version: shell.webview_version.clone(),
        secret_store: secret_store_state(state),
        sandbox: sandbox_info(),
        release_notes_url: link_under(REPOSITORY, "releases"),
        issue_tracker_url: link_under(REPOSITORY, "issues"),
    })
}

/// How a setting appears in the diagnostics: numbers, switches and enum words as they are;
/// text as "<set>" or "<empty>"; lists and objects as an entry count; unset as `null`.
fn diagnostic_value(kind: RegistryKind, value: &Value) -> Value {
    match (kind, value) {
        (_, Value::Null) => Value::Null,
        (
            RegistryKind::Bool
            | RegistryKind::Enum
            | RegistryKind::Int
            | RegistryKind::Decimal
            | RegistryKind::SizeBytes
            | RegistryKind::DurationMs
            | RegistryKind::DurationS,
            Value::Bool(_) | Value::Number(_) | Value::String(_),
        ) => value.clone(),
        (_, Value::String(text)) if text.is_empty() => Value::String("<empty>".to_owned()),
        (_, Value::String(_)) => Value::String("<set>".to_owned()),
        (_, Value::Array(items)) => Value::String(format!("<{} entries>", items.len())),
        (_, Value::Object(entries)) => Value::String(format!("<{} entries>", entries.len())),
        (_, Value::Bool(_) | Value::Number(_)) => Value::String("<set>".to_owned()),
    }
}

/// The settings as the diagnostics show them (see [`diagnostic_value`]), keyed like the
/// settings DTO. Every field of the registry appears; nothing outside it can.
fn diagnostic_settings(settings: &Settings, registry: &SettingsRegistryDto) -> Value {
    let all = serde_json::to_value(settings).unwrap_or(Value::Null);
    let mut root = Map::new();
    for field in &registry.fields {
        let Some(value) = all.pointer(&format!("/{}", field.id.replace('.', "/"))) else {
            continue;
        };
        let mut keys = field.id.split('.').peekable();
        let mut level = &mut root;
        while let Some(key) = keys.next() {
            if keys.peek().is_none() {
                level.insert(key.to_owned(), diagnostic_value(field.kind, value));
            } else {
                let next = level
                    .entry(key.to_owned())
                    .or_insert_with(|| Value::Object(Map::new()));
                let Value::Object(next) = next else { break };
                level = next;
            }
        }
    }
    Value::Object(root)
}

/// The serialized word of a plain enum (`stopped`, `seatbelt`).
fn word(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The text "Copy diagnostics" puts on the clipboard: versions, OS, secret store
/// state, sandbox, the number of enabled providers (no names), the gateway state and the
/// settings with every free text, list and path reduced to a flag or a count, then the recent
/// warnings and errors of this process (when, level, area and a scrubbed message, see
/// [`diagnostic_problems`]). Contains no key,
/// token, URL, host or provider name; any stored secret value is scrubbed from the text as a
/// last line of defence.
///
/// # Errors
/// [`AppError::Database`] when the providers cannot be read.
pub fn app_diagnostics_impl(state: &AppState, shell: &ShellInfo) -> Result<String, AppError> {
    let info = app_info_impl(state, shell)?;
    let enabled_providers = state
        .with_llm_connection(crate::llm::store::list_providers)?
        .iter()
        .filter(|provider| provider.enabled)
        .count();
    let gateway = state.llm().gateway.status(state.llm());
    let gateway_state = word(&gateway.state);
    let secret_store = if info.secret_store.ok {
        "ok"
    } else {
        "unavailable"
    };
    let (tier, selftest) = (word(&info.sandbox.tier), word(&info.sandbox.selftest));
    let settings = diagnostic_settings(&state.settings(), &settings_registry());
    let settings = serde_json::to_string_pretty(&settings).map_err(command_error)?;
    let text = format!(
        "SpanBay {} (core {}, schema {})\n\
         OS: {} {}\n\
         Webview: {}\n\
         Secret store: {} ({secret_store})\n\
         Sandbox: {tier} (self-test {selftest})\n\
         Enabled providers: {enabled_providers}\n\
         Gateway: {gateway_state}{}\n\
         Settings:\n{settings}\n\
         {problems}",
        info.app_version,
        info.core_version,
        info.schema_version,
        info.os,
        info.arch,
        info.webview_version,
        info.secret_store.backend,
        if gateway.lan { ", local network" } else { "" },
        problems = diagnostic_problems(&crate::logging::recent_problems()),
    );
    Ok(redact_secrets(
        &text,
        &state.secrets().active_redaction_needles(),
    ))
}

/// Longest message a diagnostics problem line keeps (characters, after scrubbing).
const PROBLEM_MESSAGE_CHARS: usize = 160;

/// What the diagnostics replace inside a log message, in order: URLs, e-mail addresses, IP
/// addresses, host names (and anything shaped like one, file names included), quoted text and
/// absolute paths. `None` when a pattern fails to build: the messages are then left out.
static PROBLEM_SCRUBS: std::sync::LazyLock<Option<Vec<(regex::Regex, &'static str)>>> =
    std::sync::LazyLock::new(|| {
        [
            (r"[A-Za-z][A-Za-z0-9+.-]*://\S+", "<url>"),
            (r"[\w.+-]+@[\w-]+(?:\.[\w-]+)+", "<email>"),
            (r"\[[0-9A-Fa-f:.]+\](?::\d+)?", "<ip>"),
            (r"\b\d{1,3}(?:\.\d{1,3}){3}(?::\d+)?\b", "<ip>"),
            (r"\b(?:[A-Za-z0-9-]+\.)+[A-Za-z]{2,}(?::\d+)?\b", "<host>"),
            (r#""[^"]*"|'[^']*'|`[^`]*`"#, "<text>"),
            (r"(?:[A-Za-z]:)?(?:[/\\][^\s/\\]+){2,}[/\\]?", "<path>"),
        ]
        .into_iter()
        .map(|(pattern, with)| regex::Regex::new(pattern).ok().map(|re| (re, with)))
        .collect()
    });

/// The "Recent problems" block: each recent warning or error as `ts LEVEL target message`, the
/// message scrubbed by [`PROBLEM_SCRUBS`] and cut to [`PROBLEM_MESSAGE_CHARS`]; the full text
/// stays in the log file only.
fn diagnostic_problems(lines: &[String]) -> String {
    if lines.is_empty() {
        return "Recent problems: none\n".to_owned();
    }
    let mut out = String::from("Recent problems (oldest first; full text in logs/spanbay.log):\n");
    for line in lines {
        let mut parts = line.splitn(4, ' ');
        let (ts, level, target) = (
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default(),
        );
        let message = match PROBLEM_SCRUBS.as_ref() {
            Some(scrubs) => {
                let mut text = parts.next().unwrap_or_default().to_owned();
                for (pattern, with) in scrubs {
                    text = pattern.replace_all(&text, *with).into_owned();
                }
                // What the patterns could not place still never reaches the report.
                text = text.replace("://", "<url>").replace('@', "<at>");
                match text.char_indices().nth(PROBLEM_MESSAGE_CHARS) {
                    Some((cut, _)) => format!("{}…", &text[..cut]),
                    None => text,
                }
            }
            None => String::new(),
        };
        out.push_str(&format!("  {ts} {level} {target} {message}\n"));
    }
    out
}

/// How much `workspace_id` holds, for the delete confirm.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown workspace, [`AppError::Validation`] for a malformed id.
pub fn workspace_counts_impl(
    state: &AppState,
    workspace_id: &str,
) -> Result<WorkspaceCounts, AppError> {
    let store = state.store();
    ensure_workspace_exists(&store, workspace_id)?;
    let id = WorkspaceId::new(workspace_id).map_err(command_error)?;
    store.workspace_counts(id.as_str()).map_err(command_error)
}

/// The ranges, allowed values, defaults and flags of every setting, and the read-only retention
/// and failover constants. Contains no stored value.
#[must_use]
pub fn settings_registry_get_impl() -> SettingsRegistryDto {
    settings_registry()
}

#[cfg(test)]
mod link_tests {
    use super::link_under;

    #[test]
    fn project_links_hang_below_the_repository_url_and_vanish_without_one() {
        assert_eq!(link_under("", "releases"), "");
        assert_eq!(link_under("  ", "issues"), "");
        assert_eq!(
            link_under("https://example.test/spanbay/", "releases"),
            "https://example.test/spanbay/releases"
        );
        assert_eq!(
            link_under("https://example.test/spanbay", "issues"),
            "https://example.test/spanbay/issues"
        );
    }
}

#[cfg(test)]
mod problem_tests {
    use super::diagnostic_problems;

    #[test]
    fn recent_problems_keep_where_and_when_but_no_address_path_or_quoted_text() {
        let text = diagnostic_problems(&[
            "2026-01-01T00:00:00.000Z ERROR execution.history failed to send to \
             https://api.example.test/v1?x=1 via 10.0.0.7:8080 for ops@example.test in \
             /Users/alice/project/file.json named \"Prod relay\""
                .to_owned(),
            format!(
                "2026-01-01T00:00:01.000Z WARN webview.render {}",
                "y".repeat(400)
            ),
        ]);
        assert!(text.contains("2026-01-01T00:00:00.000Z ERROR execution.history failed to send"));
        for leaked in [
            "api.example.test",
            "10.0.0.7",
            "ops@",
            "alice",
            "Prod relay",
        ] {
            assert!(!text.contains(leaked), "{leaked} in {text}");
        }
        let long = text.lines().last().unwrap_or_default();
        assert!(long.ends_with('…') && long.chars().count() < 260, "{long}");
        assert_eq!(diagnostic_problems(&[]), "Recent problems: none\n");
    }
}
