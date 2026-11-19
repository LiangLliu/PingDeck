//! The typed settings sections, their defaults (today's hard-coded behaviour) and validation.
//!
//! Single responsibility: describe the shape and the legal range of every setting. Persistence,
//! patching and change events live in the parent module.

use std::collections::BTreeMap;
use std::fmt::Display;

use serde::{Deserialize, Serialize};

use super::limits::CaptureMode;
use crate::agent::SubagentRole;
use crate::agent_target::AgentTarget;
use crate::models::{HttpVersionPreference, MAX_REDIRECT_HOPS, NetworkField, NetworkOptions};
use pingdeck_base::error::{AppError, ErrorCode};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

/// Seconds an unattended tool approval waits by default (a stored `0` reads as this).
const AI_APPROVAL_TIMEOUT_DEFAULT_S: u32 = 600;

/// Characters `ai.agent.global_instructions` may hold once trimmed.
const AI_GLOBAL_INSTRUCTIONS_MAX_CHARS: usize = 4000;

/// Whether `value` is a plain `host` or `host:port` (a bracketed IPv6 literal may carry a port
/// too): no whitespace, control characters, scheme, path, user info or empty parts.
fn is_host_port(value: &str) -> bool {
    let (host, port) = match value.strip_prefix('[') {
        Some(rest) => match rest.split_once(']') {
            Some((host, "")) => (host, None),
            Some((host, tail)) => (host, tail.strip_prefix(':')),
            None => return false,
        },
        None => match value.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (value, None),
        },
    };
    let host_ok = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'));
    host_ok && port.is_none_or(|port| port.parse::<u16>().is_ok_and(|port| port != 0))
}

/// A list of at most `max_items` entries, each 1 to `max_bytes` bytes without control characters.
fn string_list(
    field: &str,
    list: &[String],
    max_items: usize,
    max_bytes: usize,
) -> Result<(), AppError> {
    if list.len() > max_items {
        return Err(AppError::validation(format!(
            "{field} must hold at most {max_items} entries"
        )));
    }
    if list.iter().any(|item| {
        item.trim().is_empty() || item.len() > max_bytes || item.chars().any(char::is_control)
    }) {
        return Err(AppError::validation(format!(
            "{field} entries must be 1-{max_bytes} bytes without control characters"
        )));
    }
    Ok(())
}

/// Bounds of `mock.log_capacity`; a stored value outside them is clamped on load.
const MOCK_LOG_CAPACITY_MIN: u32 = 10;
const MOCK_LOG_CAPACITY_MAX: u32 = 5_000;

/// Rejects `value` outside `min..=max`, naming the dotted field.
fn within<T: PartialOrd + Display + Copy>(
    field: &str,
    value: T,
    min: T,
    max: T,
) -> Result<(), AppError> {
    if value < min || value > max {
        return Err(AppError::validation(format!(
            "{field} must be between {min} and {max}"
        )));
    }
    Ok(())
}

/// Rejects a non-empty value that is not an absolute URL with one of `schemes`.
fn url_with_scheme(field: &str, value: &str, schemes: &[&str]) -> Result<(), AppError> {
    if value.is_empty() {
        return Ok(());
    }
    match url::Url::parse(value) {
        Ok(url) if schemes.contains(&url.scheme()) && url.has_host() => Ok(()),
        _ => Err(AppError::validation(format!(
            "{field} must be a URL starting with {}",
            schemes.join("://, ") + "://"
        ))),
    }
}

/// UI theme.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follow the operating system.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

/// UI language.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    /// Simplified Chinese.
    #[default]
    #[serde(rename = "zh-CN")]
    ZhCn,
    /// English.
    #[serde(rename = "en")]
    En,
}

/// Interface density.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    /// Default spacing.
    #[default]
    Comfortable,
    /// Tighter rows and padding.
    Compact,
}

/// Whether UI animation is reduced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReduceMotion {
    /// Follow the operating system.
    #[default]
    System,
    /// Always reduce motion.
    On,
    /// Never reduce motion.
    Off,
}

/// How the sidebar is shown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarMode {
    /// Stored default only: the shell's sidebar follows the user's toggle/⌘B choice
    /// (a UI preference), never the window width.
    #[default]
    Auto,
    /// Always expanded.
    Expanded,
    /// Always the icon rail.
    Rail,
}

/// Look and feel: theme, fonts, density, motion and sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    /// Colour theme.
    pub theme: Theme,
    /// Interface font size in px.
    pub ui_font_size: u8,
    /// Monospace (editor, response) font size in px.
    pub mono_font_size: u8,
    /// Interface density.
    pub density: Density,
    /// Reduced-motion preference.
    pub reduce_motion: ReduceMotion,
    /// Sidebar presentation.
    pub sidebar: SidebarMode,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            ui_font_size: 13,
            mono_font_size: 12,
            density: Density::default(),
            reduce_motion: ReduceMotion::default(),
            sidebar: SidebarMode::default(),
        }
    }
}

impl AppearanceSettings {
    fn validate(&self) -> Result<(), AppError> {
        within("appearance.ui_font_size", self.ui_font_size, 10, 26)?;
        within("appearance.mono_font_size", self.mono_font_size, 10, 24)
    }
}

/// Language and start-up behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralSettings {
    /// Interface language.
    pub language: Language,
    /// Reopen the tabs and workspace of the last session on launch.
    pub restore_session: bool,
    /// Look for a new version when the app starts.
    pub check_updates_on_launch: bool,
    /// Ask the OS for attention (dock bounce) on approval-needed and on a run over 30 s that ends
    /// while the window is not focused; the dock badge count always follows pending approvals.
    pub dock_bounce: bool,
    /// Which events raise a system notification while the window is not focused (`off` also
    /// stops the bounce). Off the frozen registry; left out of the serialized settings while it is
    /// the default.
    #[serde(skip_serializing_if = "NotifyWhenAway::is_default")]
    pub notify_when_away: NotifyWhenAway,
}

/// What raises a system notification while the window is not focused (`general.notify_when_away`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyWhenAway {
    /// Never notify, and never bounce the dock; the badge still counts approvals.
    Off,
    /// A chat run waits on the user: an approval, a question, a plan or a cost pause. The
    /// default: a finished run notifies only when the user opts in.
    #[default]
    Gates,
    /// Also a run of 30 s or more that completed or failed.
    GatesAndFinished,
}

impl NotifyWhenAway {
    /// Whether this is the default (so the settings leave it out).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            language: Language::default(),
            restore_session: true,
            check_updates_on_launch: false,
            dock_bounce: true,
            notify_when_away: NotifyWhenAway::default(),
        }
    }
}

impl GeneralSettings {
    #[allow(clippy::unused_self, clippy::unnecessary_wraps)] // every section validates alike
    fn validate(&self) -> Result<(), AppError> {
        Ok(())
    }
}

/// Code editor and response viewer behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    /// Spaces inserted by Tab.
    pub tab_size: u8,
    /// Wrap long lines in editors.
    pub word_wrap: bool,
    /// Largest request text (bytes) the editor syntax-highlights.
    pub editor_highlight_limit_bytes: u64,
    /// Largest response text (bytes) the viewer syntax-highlights.
    pub viewer_highlight_limit_bytes: u64,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            tab_size: 2,
            word_wrap: false,
            editor_highlight_limit_bytes: MIB,
            viewer_highlight_limit_bytes: 512 * KIB,
        }
    }
}

impl EditorSettings {
    fn validate(&self) -> Result<(), AppError> {
        within("editor.tab_size", self.tab_size, 1, 8)?;
        within(
            "editor.editor_highlight_limit_bytes",
            self.editor_highlight_limit_bytes,
            KIB,
            64 * MIB,
        )?;
        within(
            "editor.viewer_highlight_limit_bytes",
            self.viewer_highlight_limit_bytes,
            KIB,
            64 * MIB,
        )
    }
}

/// Global transport defaults for new requests (each request may override them) and for the
/// OAuth token fetch and `pm.sendRequest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkSettings {
    /// Request timeout in milliseconds.
    pub timeout_ms: u64,
    /// Follow redirects.
    pub follow_redirects: bool,
    /// Redirect hops to follow.
    pub max_redirects: u32,
    /// Verify TLS certificates.
    pub verify_tls: bool,
    /// Proxy: empty follows the system proxy, `direct` uses none, else a URL. In memory it carries the password; every
    /// serialization (the stored row, a DTO, an export) writes `http://user:****@host`: the
    /// password lives in the secret store and `app::settings_service` joins them (see
    /// [`crate::proxy_url`]).
    #[serde(serialize_with = "pingdeck_base::proxy_url::serialize_masked")]
    pub proxy_url: String,
    /// Hosts that bypass the proxy (`*.local` matches a suffix).
    pub no_proxy: Vec<String>,
    /// Absolute path of a PEM bundle of extra trusted CAs; empty means none.
    pub ca_bundle_path: String,
    /// Preferred HTTP version.
    pub http_version: HttpVersionPreference,
    /// Retries after a transport failure.
    pub retry_count: u8,
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            timeout_ms: 30_000,
            follow_redirects: true,
            max_redirects: 10,
            verify_tls: true,
            proxy_url: String::new(),
            no_proxy: ["localhost", "127.0.0.1", "::1", "*.local"]
                .map(str::to_owned)
                .into(),
            ca_bundle_path: String::new(),
            http_version: HttpVersionPreference::Auto,
            retry_count: 0,
        }
    }
}

impl From<&NetworkSettings> for NetworkOptions {
    /// The options a new request starts with: every field with a global default follows Settings
    /// (the copied values only show what it is today), the per-request-only flags keep their defaults.
    /// The proxy is not copied, so a global proxy password never lands in the request.
    fn from(settings: &NetworkSettings) -> Self {
        Self {
            timeout_ms: settings.timeout_ms,
            follow_redirects: settings.follow_redirects,
            retry_count: settings.retry_count,
            verify_tls: settings.verify_tls,
            http_version: settings.http_version,
            max_redirects: settings.max_redirects,
            inherit: NetworkField::ALL.to_vec(),
            ..Self::default()
        }
    }
}

impl NetworkSettings {
    /// Replaces every field `options` inherits with its current global value (at send time).
    pub fn apply_inherited(&self, options: &mut NetworkOptions) {
        for field in &options.inherit {
            match field {
                NetworkField::TimeoutMs => options.timeout_ms = self.timeout_ms,
                NetworkField::RetryCount => options.retry_count = self.retry_count,
                NetworkField::HttpVersion => options.http_version = self.http_version,
                NetworkField::FollowRedirects => options.follow_redirects = self.follow_redirects,
                NetworkField::MaxRedirects => options.max_redirects = self.max_redirects,
                NetworkField::VerifyTls => options.verify_tls = self.verify_tls,
                NetworkField::ProxyUrl => {
                    options.proxy_url =
                        (!self.proxy_url.is_empty()).then(|| self.proxy_url.clone());
                }
                NetworkField::Unknown => {}
            }
        }
    }
}

impl NetworkSettings {
    fn validate(&self) -> Result<(), AppError> {
        within("network.timeout_ms", self.timeout_ms, 1, 3_600_000)?;
        within(
            "network.max_redirects",
            self.max_redirects,
            0,
            MAX_REDIRECT_HOPS,
        )?;
        within("network.retry_count", self.retry_count, 0, 10)?;
        // Empty follows the system proxy, `direct` uses none; anything else is a URL.
        if self.proxy_url != pingdeck_base::proxy_url::DIRECT {
            url_with_scheme(
                "network.proxy_url",
                &self.proxy_url,
                &["http", "https", "socks5", "socks5h"],
            )?;
        }
        string_list("network.no_proxy", &self.no_proxy, 64, 255)?;
        if !self.ca_bundle_path.is_empty()
            && (self.ca_bundle_path.len() > 1024
                || self.ca_bundle_path.chars().any(char::is_control)
                || !std::path::Path::new(&self.ca_bundle_path).is_absolute())
        {
            return Err(AppError::validation(
                "network.ca_bundle_path must be an absolute path of at most 1024 bytes without control characters",
            ));
        }
        Ok(())
    }
}

/// Stored data and response size thresholds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DataSettings {
    /// History entries kept per workspace.
    pub history_retention: u32,
    /// Largest response body (bytes) read; the rest is cut.
    pub response_body_cap_bytes: u64,
    /// Bodies up to this size travel inline; larger ones are fetched by handle.
    pub inline_threshold_bytes: u64,
    /// Preview size (bytes) returned for a body above the inline threshold.
    pub preview_bytes: u64,
    /// Chats not used for this many days are deleted at launch and daily (pinned chats and chats
    /// with a running or waiting run are kept); `0` is Off.
    pub chat_retention_days: u16,
}

impl Default for DataSettings {
    fn default() -> Self {
        Self {
            history_retention: 2000,
            response_body_cap_bytes: 10 * MIB,
            inline_threshold_bytes: MIB,
            preview_bytes: 64 * KIB,
            chat_retention_days: 0,
        }
    }
}

impl DataSettings {
    /// [`Self::response_body_cap_bytes`] as an in-memory size, clamped to the
    /// [`pingdeck_response::bodies::MAX_STORED_BYTES`] budget of the body store so one response cannot pin
    /// more memory than the store admits.
    #[must_use]
    pub fn response_body_cap(&self) -> usize {
        usize::try_from(self.response_body_cap_bytes)
            .unwrap_or(usize::MAX)
            .min(pingdeck_response::bodies::MAX_STORED_BYTES)
    }

    fn validate(&self) -> Result<(), AppError> {
        within(
            "data.history_retention",
            self.history_retention,
            10,
            100_000,
        )?;
        within(
            "data.response_body_cap_bytes",
            self.response_body_cap_bytes,
            KIB,
            512 * MIB,
        )?;
        within(
            "data.inline_threshold_bytes",
            self.inline_threshold_bytes,
            KIB,
            64 * MIB,
        )?;
        within("data.preview_bytes", self.preview_bytes, KIB, 64 * MIB)?;
        if ![0, 30, 90, 180, 365].contains(&self.chat_retention_days) {
            return Err(AppError::validation(
                "data.chat_retention_days must be 0 (off), 30, 90, 180 or 365",
            ));
        }
        if self.preview_bytes > self.inline_threshold_bytes {
            return Err(AppError::validation(
                "data.preview_bytes must not exceed data.inline_threshold_bytes",
            ));
        }
        Ok(())
    }
}

/// Mock server defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MockSettings {
    /// First port tried for a new mock server.
    pub first_port: u16,
    /// Latency added to a new mock server's responses, in milliseconds.
    pub default_latency_ms: u64,
    /// Whether a new mock server answers with CORS headers.
    pub cors_default: bool,
    /// Transactions kept in a running mock server's log.
    pub log_capacity: u32,
    /// Host names (`host` or `host:port`) the Host guard of every mock server accepts besides
    /// loopback, such as `10.0.2.2` (Android emulator) or `host.docker.internal`. The listener
    /// still binds loopback only; omitted while empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra_allowed_hosts: Vec<String>,
}

impl Default for MockSettings {
    fn default() -> Self {
        Self {
            first_port: 3001,
            default_latency_ms: 0,
            cors_default: true,
            log_capacity: 200,
            extra_allowed_hosts: Vec::new(),
        }
    }
}

impl MockSettings {
    fn validate(&self) -> Result<(), AppError> {
        within("mock.first_port", self.first_port, 1024, u16::MAX)?;
        within(
            "mock.default_latency_ms",
            self.default_latency_ms,
            0,
            60_000,
        )?;
        within(
            "mock.log_capacity",
            self.log_capacity,
            MOCK_LOG_CAPACITY_MIN,
            MOCK_LOG_CAPACITY_MAX,
        )?;
        string_list(
            "mock.extra_allowed_hosts",
            &self.extra_allowed_hosts,
            16,
            253,
        )?;
        match self
            .extra_allowed_hosts
            .iter()
            .find(|host| !is_host_port(host))
        {
            Some(host) => Err(AppError::validation(format!(
                "mock.extra_allowed_hosts entries must be host or host:port: {}",
                host.chars()
                    .filter(|c| !c.is_control())
                    .take(80)
                    .collect::<String>()
            ))),
            None => Ok(()),
        }
    }
}

/// The AI service: local gateway, provider tooling, chat/agent harness and web search.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSettings {
    /// The local LLM gateway.
    pub gateway: AiGatewaySettings,
    /// Provider catalog and connectivity tests.
    pub providers: AiProvidersSettings,
    /// The chat/agent harness.
    pub agent: AiAgentSettings,
    /// Web search for the agent.
    pub search: AiSearchSettings,
}

impl AiSettings {
    fn validate(&self) -> Result<(), AppError> {
        self.gateway.validate()?;
        self.providers.validate()?;
        self.agent.validate()?;
        self.search.validate()
    }

    /// The fields only the secret use-case writes: `(dotted name, value)`.
    pub(crate) fn read_only_hints(&self) -> [(&'static str, &str); 2] {
        [
            ("ai.search.tavily_key_hint", &self.search.tavily_key_hint),
            ("ai.search.brave_key_hint", &self.search.brave_key_hint),
        ]
    }

    /// Takes the read-only hints from `from`, leaving every other field as it is.
    pub fn keep_hints_of(&mut self, from: &Self) {
        self.search.tavily_key_hint = from.search.tavily_key_hint.clone();
        self.search.brave_key_hint = from.search.brave_key_hint.clone();
    }
}

/// The local LLM gateway. Callers authenticate with per-client tokens (`llm_gateway_clients`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiGatewaySettings {
    /// Start the gateway when the app starts.
    pub enabled_on_start: bool,
    /// Listen on all interfaces (`0.0.0.0`) instead of loopback; needs `require_token`.
    #[serde(alias = "bind_lan")]
    pub serve_lan: bool,
    /// Host names (besides loopback, the machine name and its LAN address) the Host guard accepts.
    pub extra_allowed_hosts: Vec<String>,
    /// Port the gateway listens on.
    pub port: u16,
    /// Require a client token on every call.
    pub require_token: bool,
    /// Upstream retries per call.
    pub max_retries: u8,
    /// Seconds to wait for the first upstream byte.
    pub first_byte_timeout_s: u32,
    /// Seconds a stream may stay silent.
    pub idle_timeout_s: u32,
    /// Consecutive failures that open a route's circuit.
    pub circuit_failure_threshold: u8,
    /// Seconds the failures are counted over.
    pub circuit_window_s: u32,
    /// Seconds an open circuit rests before a probe call.
    pub circuit_open_s: u32,
    /// Longest rest, in seconds, an open circuit backs off to.
    pub circuit_open_max_s: u32,
    /// Which gateway exchanges are captured for debugging.
    pub capture_mode: CaptureMode,
}

impl Default for AiGatewaySettings {
    fn default() -> Self {
        Self {
            enabled_on_start: false,
            serve_lan: false,
            extra_allowed_hosts: Vec::new(),
            port: 4141,
            require_token: true,
            max_retries: 4,
            first_byte_timeout_s: 30,
            idle_timeout_s: 60,
            circuit_failure_threshold: 3,
            circuit_window_s: 30,
            circuit_open_s: 30,
            circuit_open_max_s: 300,
            capture_mode: CaptureMode::default(),
        }
    }
}

impl AiGatewaySettings {
    fn validate(&self) -> Result<(), AppError> {
        within("ai.gateway.port", self.port, 1024, u16::MAX)?;
        within("ai.gateway.max_retries", self.max_retries, 0, 10)?;
        within(
            "ai.gateway.first_byte_timeout_s",
            self.first_byte_timeout_s,
            1,
            600,
        )?;
        within("ai.gateway.idle_timeout_s", self.idle_timeout_s, 1, 3600)?;
        within(
            "ai.gateway.circuit_failure_threshold",
            self.circuit_failure_threshold,
            1,
            100,
        )?;
        within(
            "ai.gateway.circuit_window_s",
            self.circuit_window_s,
            1,
            3600,
        )?;
        within("ai.gateway.circuit_open_s", self.circuit_open_s, 5, 3600)?;
        within(
            "ai.gateway.circuit_open_max_s",
            self.circuit_open_max_s,
            5,
            86_400,
        )?;
        string_list(
            "ai.gateway.extra_allowed_hosts",
            &self.extra_allowed_hosts,
            16,
            253,
        )?;
        if let Some(host) = self
            .extra_allowed_hosts
            .iter()
            .find(|host| !is_host_port(host))
        {
            return Err(AppError::validation(format!(
                "ai.gateway.extra_allowed_hosts entries must be host or host:port: {}",
                host.chars()
                    .filter(|c| !c.is_control())
                    .take(80)
                    .collect::<String>()
            )));
        }
        if self.serve_lan && !self.require_token {
            return Err(AppError::validation(
                "ai.gateway.serve_lan requires ai.gateway.require_token",
            ));
        }
        if self.forces_token() {
            return Err(AppError::validation(
                "ai.gateway.extra_allowed_hosts requires ai.gateway.require_token",
            )
            .coded(ErrorCode::GatewayTokenRequired, None));
        }
        Ok(())
    }

    /// Extra allowed hosts (a tunnel, Docker) reach the gateway from off this machine, so
    /// they need tokens; `true` when they are set while `require_token` is off.
    #[must_use]
    pub fn forces_token(&self) -> bool {
        !self.require_token && !self.extra_allowed_hosts.is_empty()
    }

    /// Whether a call must present a client token: `require_token`, or any extra allowed host.
    #[must_use]
    pub fn token_required(&self) -> bool {
        self.require_token || !self.extra_allowed_hosts.is_empty()
    }
}

/// Provider catalog and connectivity tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiProvidersSettings {
    /// Sync the model catalog from models.dev in the background (first check 90 s after launch,
    /// then when the sync interval has passed). Off by default: no outbound call unasked.
    pub catalog_auto_refresh: bool,
    /// Prompt sent by a model test.
    pub default_test_prompt: String,
    /// Models probed in parallel.
    pub probe_concurrency: u8,
    /// Seconds one probe may take.
    pub probe_timeout_s: u32,
    /// Minutes between automatic balance refreshes; `0` refreshes only on request.
    pub balance_refresh_min: u16,
    /// Days of per-call usage rows kept.
    pub usage_retention_days: u16,
}

impl Default for AiProvidersSettings {
    fn default() -> Self {
        Self {
            catalog_auto_refresh: false,
            default_test_prompt: "Reply with the single word: pong".to_owned(),
            probe_concurrency: 3,
            probe_timeout_s: 30,
            balance_refresh_min: 0,
            usage_retention_days: 365,
        }
    }
}

impl AiProvidersSettings {
    fn validate(&self) -> Result<(), AppError> {
        within(
            "ai.providers.probe_concurrency",
            self.probe_concurrency,
            1,
            16,
        )?;
        within("ai.providers.probe_timeout_s", self.probe_timeout_s, 5, 300)?;
        within(
            "ai.providers.usage_retention_days",
            self.usage_retention_days,
            30,
            3650,
        )?;
        if self.default_test_prompt.trim().is_empty() || self.default_test_prompt.len() > 2000 {
            return Err(AppError::validation(
                "ai.providers.default_test_prompt must be 1 to 2000 bytes of non-blank text",
            ));
        }
        if self.balance_refresh_min != 0 {
            within(
                "ai.providers.balance_refresh_min",
                self.balance_refresh_min,
                5,
                1440,
            )?;
        }
        Ok(())
    }
}

/// When the agent runs a tool without asking.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolApproval {
    /// Ask before every tool call.
    Ask,
    /// Run tools annotated read-only; ask for the rest.
    #[default]
    AutoReadOnly,
    /// Run every tool.
    AutoAll,
}

impl ToolApproval {
    /// Whether this is the default mode (so the settings leave it out).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Which web search backend the agent's `web_search` tool falls back to when the target model
/// has no native search.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchBackend {
    /// The first usable backend: a configured key or URL, else DuckDuckGo. Schema-2 rows stored
    /// `none` or `provider_native`; both read as this.
    #[default]
    #[serde(alias = "none", alias = "provider_native")]
    Auto,
    /// DuckDuckGo (no key).
    #[serde(rename = "duckduckgo")]
    DuckDuckGo,
    /// Brave Search (API key).
    Brave,
    /// Tavily (API key).
    Tavily,
    /// A SearXNG instance (base URL, no key).
    Searxng,
}

/// The reasoning effort a new session starts with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffortOrAuto {
    /// Leave it to the model's default.
    #[default]
    Auto,
    /// Least thinking the provider offers.
    Minimal,
    /// Low.
    Low,
    /// Medium.
    Medium,
    /// High.
    High,
    /// Maximum.
    Max,
}

/// The chat/agent harness.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiAgentSettings {
    /// Target a new session starts with.
    pub default_target: Option<AgentTarget>,
    /// Reasoning effort a new session starts with.
    pub default_effort: ReasoningEffortOrAuto,
    /// Workspace a new session starts in; `None` means no workspace.
    pub default_workspace: Option<String>,
    /// Name new sessions from their first exchange.
    pub auto_title: bool,
    /// Seconds an unattended tool approval waits before the call ends as timed out.
    pub approval_timeout_s: u32,
    /// Model calls per user turn before the runner stops.
    pub max_tool_iterations: u32,
    /// Agent runs executing at once.
    pub max_concurrent_runs: u8,
    /// Sub-agents one run may start (their nesting depth is fixed at 1).
    pub max_subagents: u8,
    /// Largest image attachment in megabytes.
    pub max_image_mb: u8,
    /// Largest file attachment in megabytes.
    pub max_file_mb: u8,
    /// Longest edge, in pixels, an attached image is downscaled to (at most 2,048: the core refuses
    /// an image above 2,250 px).
    pub image_max_edge_px: u16,
    /// Compact the conversation automatically when the context fills.
    pub auto_compact: bool,
    /// Context fill, in percent, that triggers automatic compaction.
    pub compact_threshold_pct: u8,
    /// Newest user turns compaction leaves untouched.
    pub keep_recent_turns: u8,
    /// Stop a run before its next model call once it has cost this many US dollars; `None` is off.
    pub cost_cap_usd: Option<f64>,
    /// Stop a run before its next model call once it has cost this many yuan; `None` is off.
    pub cost_cap_cny: Option<f64>,
    /// Confine `run_shell` writes to the workspace.
    pub sandbox_writes_to_workspace: bool,
    /// Seconds a shell command may run.
    pub shell_timeout_s: u32,
    /// The user's own command patterns `run_shell` refuses (`*` matches any text), added to the
    /// pinned rules the permission layer owns; a setting can never remove a pinned rule.
    pub blocked_commands: Vec<String>,
    /// Environment variables `run_shell` inherits besides the safe set.
    pub shell_env_passthrough: Vec<String>,
    /// Directories scanned for skills (`~` expands to the home directory).
    pub skill_dirs: Vec<String>,
    /// The models `task` sub-agents may run on, by role (model references in id form
    /// `provider_id/model`). Left out of the serialized settings while no role is set.
    #[serde(skip_serializing_if = "RoleModels::is_empty")]
    pub role_models: RoleModels,
    /// The fallback chain of each agent usage site (model references in id form
    /// `provider_id/model`, at most five each), tried in order after the site's own model. Only
    /// in-app agent calls use them. Left out of the serialized settings while every list is empty.
    #[serde(skip_serializing_if = "AgentFallbacks::is_empty")]
    pub fallbacks: AgentFallbacks,
    /// The user's instructions every chat's prompt carries before the chat's own (at most 4,000
    /// characters once trimmed). Left out of the serialized settings while empty.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub global_instructions: String,
    /// The permission mode a new session starts with when it names none. Left out of the
    /// serialized settings while it is the default.
    #[serde(skip_serializing_if = "ToolApproval::is_default")]
    pub default_permission_mode: ToolApproval,
    /// The last step of a run forbids tool calls and asks the model for a summary. Left
    /// out of the serialized settings while on.
    #[serde(skip_serializing_if = "is_on")]
    pub wrap_up_last_step: bool,
    /// Prompt-cache hints on agent calls: `auto` marks targets the catalogue says cache,
    /// `off` sends none anywhere. Left out of the serialized settings while `auto`.
    #[serde(skip_serializing_if = "PromptCacheMode::is_default")]
    pub prompt_cache: PromptCacheMode,
    /// In Auto-approve reads, a shell command runs without asking when the sandbox self-test
    /// passed, it gets no network, nothing forces an Ask, its risk is not High and no Ask or Block
    /// rule matches. Off by default; stays on this device. Left out while off.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub sandbox_auto_run: bool,
    /// Pause a run when it and its sub-agents have used this many tokens (input and output), as
    /// at the cost cap; off when absent. Stays on this device, so a sync cannot turn it off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_run_tokens: Option<u64>,
    /// Seconds one tool call of an MCP server may go without a word from the server, by server
    /// id (5 to 3,600); a server not listed gets 120 s. Each progress notification of the call
    /// restarts it. Left out of the serialized settings while empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp_call_timeout_s: BTreeMap<String, u32>,
}

/// Whether agent calls carry prompt-cache hints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheMode {
    /// On for targets whose catalogue entry supports caching (official Anthropic among them).
    #[default]
    Auto,
    /// Never.
    Off,
}

impl PromptCacheMode {
    /// Whether this is the default (so the settings leave it out).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `skip_serializing_if` passes a reference
const fn is_on(value: &bool) -> bool {
    *value
}

/// The model of each sub-agent role (`ai.agent.role_models`); a role without one is not offered.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoleModels {
    /// Quick, cheap work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fast: Option<String>,
    /// Hard reasoning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deep: Option<String>,
    /// Reading images.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<String>,
}

impl RoleModels {
    /// Whether no role has a model.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.fast.is_none() && self.deep.is_none() && self.vision.is_none()
    }

    /// The model reference of `role`, if it has one.
    #[must_use]
    pub fn get(&self, role: SubagentRole) -> Option<&str> {
        match role {
            SubagentRole::Fast => self.fast.as_deref(),
            SubagentRole::Deep => self.deep.as_deref(),
            SubagentRole::Vision => self.vision.as_deref(),
        }
    }

    /// The roles that have a model, in display order, with their references.
    #[must_use]
    pub fn configured(&self) -> Vec<(SubagentRole, &str)> {
        SubagentRole::ALL
            .into_iter()
            .filter_map(|role| self.get(role).map(|model| (role, model)))
            .collect()
    }
}

/// The fallback chain of each agent usage site (`ai.agent.fallbacks`): the main agent (and
/// sub-agents without a role) and each sub-agent role.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentFallbacks {
    /// The main agent, its compaction and role-less sub-agents.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub main: Vec<String>,
    /// The `fast` role.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fast: Vec<String>,
    /// The `deep` role.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deep: Vec<String>,
    /// The `vision` role.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vision: Vec<String>,
}

/// Fallbacks one agent usage site may list.
pub const AGENT_FALLBACKS_MAX: usize = 5;

impl AgentFallbacks {
    /// Whether no site has a chain.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.main.is_empty()
            && self.fast.is_empty()
            && self.deep.is_empty()
            && self.vision.is_empty()
    }

    /// The chain of a run with `role` (`None`: the main site).
    #[must_use]
    pub fn for_role(&self, role: Option<SubagentRole>) -> &[String] {
        match role {
            None => &self.main,
            Some(SubagentRole::Fast) => &self.fast,
            Some(SubagentRole::Deep) => &self.deep,
            Some(SubagentRole::Vision) => &self.vision,
        }
    }

    /// Every site with its settings name (`main`, `fast`, `deep`, `vision`) and chain.
    #[must_use]
    pub fn sites(&self) -> [(&'static str, &[String]); 4] {
        [
            ("main", &self.main),
            ("fast", &self.fast),
            ("deep", &self.deep),
            ("vision", &self.vision),
        ]
    }

    /// Every site's chain, mutably (to drop entries).
    pub fn lists_mut(&mut self) -> [&mut Vec<String>; 4] {
        [
            &mut self.main,
            &mut self.fast,
            &mut self.deep,
            &mut self.vision,
        ]
    }
}

impl Default for AiAgentSettings {
    fn default() -> Self {
        Self {
            default_target: None,
            default_effort: ReasoningEffortOrAuto::default(),
            default_workspace: None,
            auto_title: true,
            approval_timeout_s: AI_APPROVAL_TIMEOUT_DEFAULT_S,
            max_tool_iterations: 20,
            max_concurrent_runs: 3,
            max_subagents: 3,
            max_image_mb: 10,
            max_file_mb: 20,
            image_max_edge_px: 2048,
            auto_compact: true,
            compact_threshold_pct: 85,
            keep_recent_turns: 2,
            cost_cap_usd: None,
            cost_cap_cny: None,
            sandbox_writes_to_workspace: true,
            shell_timeout_s: 120,
            blocked_commands: Vec::new(),
            shell_env_passthrough: Vec::new(),
            skill_dirs: vec!["~/.claude/skills".to_owned(), "~/.agents/skills".to_owned()],
            role_models: RoleModels::default(),
            fallbacks: AgentFallbacks::default(),
            global_instructions: String::new(),
            default_permission_mode: ToolApproval::default(),
            wrap_up_last_step: true,
            sandbox_auto_run: false,
            prompt_cache: PromptCacheMode::Auto,
            max_run_tokens: None,
            mcp_call_timeout_s: BTreeMap::new(),
        }
    }
}

/// The patterns an earlier build stored as the default of `ai.agent.blocked_commands`.
const LEGACY_DEFAULT_BLOCKED: [&str; 6] = [
    "rm -rf /",
    "sudo *",
    "curl * | sh",
    "mkfs*",
    "dd if=*",
    ":(){*",
];

/// A short single-line identifier: 1 to 200 characters without control characters.
fn short_name(field: &str, value: &str) -> Result<(), AppError> {
    if value.trim().is_empty() || value.chars().count() > 200 || value.chars().any(char::is_control)
    {
        return Err(AppError::validation(format!(
            "{field} must be 1-200 characters without control characters"
        )));
    }
    Ok(())
}

impl AiAgentSettings {
    fn validate(&self) -> Result<(), AppError> {
        within(
            "ai.agent.approval_timeout_s",
            self.approval_timeout_s,
            60,
            3600,
        )?;
        within(
            "ai.agent.max_tool_iterations",
            self.max_tool_iterations,
            1,
            100,
        )?;
        within(
            "ai.agent.max_concurrent_runs",
            self.max_concurrent_runs,
            1,
            8,
        )?;
        within("ai.agent.max_subagents", self.max_subagents, 1, 8)?;
        if let Some(tokens) = self.max_run_tokens {
            within("ai.agent.max_run_tokens", tokens, 1_000, 1_000_000_000)?;
        }
        for (server, seconds) in &self.mcp_call_timeout_s {
            within(
                &format!("ai.agent.mcp_call_timeout_s.{server}"),
                *seconds,
                5,
                3_600,
            )?;
        }
        within("ai.agent.max_image_mb", self.max_image_mb, 1, 20)?;
        within("ai.agent.max_file_mb", self.max_file_mb, 1, 100)?;
        within(
            "ai.agent.image_max_edge_px",
            self.image_max_edge_px,
            512,
            2048,
        )?;
        within(
            "ai.agent.compact_threshold_pct",
            self.compact_threshold_pct,
            50,
            95,
        )?;
        within("ai.agent.keep_recent_turns", self.keep_recent_turns, 1, 20)?;
        for (field, cap) in [
            ("ai.agent.cost_cap_usd", self.cost_cap_usd),
            ("ai.agent.cost_cap_cny", self.cost_cap_cny),
        ] {
            if cap.is_some_and(|cap| !cap.is_finite() || cap <= 0.0) {
                return Err(AppError::validation(format!(
                    "{field} must be a number above 0, or unset to turn the limit off"
                )));
            }
        }
        within("ai.agent.shell_timeout_s", self.shell_timeout_s, 1, 3600)?;
        string_list("ai.agent.blocked_commands", &self.blocked_commands, 64, 256)?;
        string_list(
            "ai.agent.shell_env_passthrough",
            &self.shell_env_passthrough,
            64,
            128,
        )?;
        if (self.shell_env_passthrough.iter())
            .any(|name| name.contains(|c: char| c == '=' || c.is_whitespace()))
        {
            return Err(AppError::validation(
                "ai.agent.shell_env_passthrough entries must be variable names",
            ));
        }
        if let Some(workspace) = &self.default_workspace {
            short_name("ai.agent.default_workspace", workspace)?;
        }
        if self.skill_dirs.len() > 32 {
            return Err(AppError::validation(
                "ai.agent.skill_dirs must hold at most 32 paths",
            ));
        }
        self.skill_dirs.iter().try_for_each(|dir| skill_dir(dir))?;
        match &self.default_target {
            None => {}
            Some(AgentTarget::Named { name }) => {
                short_name("ai.agent.default_target.name", name)?;
            }
            Some(AgentTarget::Pinned {
                provider_id,
                key_id,
                model,
            }) => {
                short_name("ai.agent.default_target.provider_id", provider_id)?;
                short_name("ai.agent.default_target.model", model)?;
                if let Some(key_id) = key_id {
                    short_name("ai.agent.default_target.key_id", key_id)?;
                }
            }
        }
        for (role, model) in self.role_models.configured() {
            let field = format!("ai.agent.role_models.{}", role.as_str());
            short_name(&field, model)?;
            if !model.trim().contains('/') {
                return Err(AppError::validation(format!(
                    "{field} must be a model reference `provider/model`"
                )));
            }
        }
        for (site, chain) in self.fallbacks.sites() {
            let field = format!("ai.agent.fallbacks.{site}");
            if chain.len() > AGENT_FALLBACKS_MAX {
                return Err(AppError::validation(format!(
                    "{field} must hold at most {AGENT_FALLBACKS_MAX} models"
                )));
            }
            for (index, model) in chain.iter().enumerate() {
                short_name(&field, model)?;
                if !model.trim().contains('/') {
                    return Err(AppError::validation(format!(
                        "{field} must hold model references `provider/model`"
                    )));
                }
                if chain[..index].contains(model) {
                    return Err(AppError::validation(format!("{field} lists {model} twice")));
                }
            }
        }
        if self.global_instructions.trim().chars().count() > AI_GLOBAL_INSTRUCTIONS_MAX_CHARS {
            return Err(AppError::validation(format!(
                "ai.agent.global_instructions must be at most {AI_GLOBAL_INSTRUCTIONS_MAX_CHARS} characters"
            )));
        }
        Ok(())
    }
}

/// Web search for the agent. The Tavily and Brave keys live in the secret store
/// (`ai.search.tavily`, `ai.search.brave`); only their masked hints are settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSearchSettings {
    /// Fallback web search backend.
    pub backend: SearchBackend,
    /// SearXNG base URL.
    pub searxng_base_url: String,
    /// Use DuckDuckGo when no search key or URL is set (default off).
    pub use_duckduckgo: bool,
    /// Masked hint of the stored Tavily key; written only by the secret use-case.
    pub tavily_key_hint: String,
    /// Masked hint of the stored Brave key; written only by the secret use-case.
    pub brave_key_hint: String,
}

impl AiSearchSettings {
    fn validate(&self) -> Result<(), AppError> {
        url_with_scheme(
            "ai.search.searxng_base_url",
            &self.searxng_base_url,
            &["http", "https"],
        )
    }
}

/// An `ai.agent.skill_dirs` entry: absolute or `~`-prefixed, no `..` component, no control
/// characters, at most 1024 bytes.
fn skill_dir(dir: &str) -> Result<(), AppError> {
    let walks_up = dir.split(['/', '\\']).any(|part| part == "..");
    let anchored = dir.starts_with('~') || std::path::Path::new(dir).is_absolute();
    if dir.len() > 1024 || dir.chars().any(char::is_control) || walks_up || !anchored {
        return Err(AppError::validation(format!(
            "ai.agent.skill_dirs entries must be absolute or start with ~, have no `..` and be at most 1024 bytes without control characters: {}",
            dir.chars()
                .filter(|c| !c.is_control())
                .take(80)
                .collect::<String>()
        )));
    }
    Ok(())
}

/// Keyboard shortcut overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShortcutsSettings {
    /// Command id to key combination; commands absent here keep their default.
    pub overrides: BTreeMap<String, String>,
}

impl ShortcutsSettings {
    fn validate(&self) -> Result<(), AppError> {
        if self.overrides.len() > 200 {
            return Err(AppError::validation(
                "shortcuts.overrides must hold at most 200 entries",
            ));
        }
        for (command, keys) in &self.overrides {
            if command.trim().is_empty() || keys.trim().is_empty() || keys.len() > 64 {
                return Err(AppError::validation(format!(
                    "shortcuts.overrides[{command:?}] needs a command id and a key combination up to 64 bytes"
                )));
            }
        }
        Ok(())
    }
}

/// Config-file sync.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncSettings {
    /// Absolute directory exported to and imported from; empty means unset.
    pub directory: String,
    /// Export secret values (environment secrets, provider keys) as well.
    pub include_secrets: bool,
    /// Export after every config change.
    pub auto_export: bool,
}

impl SyncSettings {
    fn validate(&self) -> Result<(), AppError> {
        if !self.directory.is_empty() && !std::path::Path::new(&self.directory).is_absolute() {
            return Err(AppError::validation(
                "sync.directory must be an absolute path",
            ));
        }
        Ok(())
    }
}

/// Script sandbox and console limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvancedSettings {
    /// Loop iterations a script may run before it is stopped.
    pub script_loop_iteration_limit: u64,
    /// Call depth a script may reach.
    pub script_recursion_limit: usize,
    /// `pm.sendRequest` calls per script.
    pub script_max_send_requests: u32,
    /// Console entries kept per run.
    pub console_max_entries: u32,
    /// Bytes kept of one console message or script error.
    pub console_max_message_bytes: u32,
    /// `pm.test` results kept per run.
    pub max_test_results: u32,
}

impl Default for AdvancedSettings {
    fn default() -> Self {
        Self {
            script_loop_iteration_limit: 5_000_000,
            script_recursion_limit: 256,
            script_max_send_requests: 50,
            console_max_entries: 1_000,
            console_max_message_bytes: 64 * 1024,
            max_test_results: 1_000,
        }
    }
}

impl AdvancedSettings {
    fn validate(&self) -> Result<(), AppError> {
        within(
            "advanced.script_loop_iteration_limit",
            self.script_loop_iteration_limit,
            1_000,
            100_000_000,
        )?;
        within(
            "advanced.script_recursion_limit",
            self.script_recursion_limit,
            16,
            4096,
        )?;
        within(
            "advanced.script_max_send_requests",
            self.script_max_send_requests,
            0,
            1000,
        )?;
        within(
            "advanced.console_max_entries",
            self.console_max_entries,
            10,
            100_000,
        )?;
        within(
            "advanced.console_max_message_bytes",
            self.console_max_message_bytes,
            256,
            4 * 1024 * 1024,
        )?;
        within(
            "advanced.max_test_results",
            self.max_test_results,
            10,
            100_000,
        )
    }
}

/// Validation shared by [`super::Settings`]: one `validate` per section type.
pub(super) trait Validate {
    /// Fails with [`AppError::Validation`] naming the offending field.
    fn check(&self) -> Result<(), AppError>;

    /// Pulls a value that a former build allowed back into range; applied to stored rows only,
    /// before [`Self::check`], so such a row keeps its other fields.
    fn normalize_stored(&mut self) {}

    /// Adds to `schema` (the serialized defaults a patch is checked against) the fields the
    /// defaults leave out because they serialize only when set, so a patch may name them.
    fn extend_patch_schema(_schema: &mut serde_json::Value) {}
}

impl Validate for MockSettings {
    fn check(&self) -> Result<(), AppError> {
        self.validate()
    }

    fn normalize_stored(&mut self) {
        self.log_capacity = self
            .log_capacity
            .clamp(MOCK_LOG_CAPACITY_MIN, MOCK_LOG_CAPACITY_MAX);
    }

    fn extend_patch_schema(schema: &mut serde_json::Value) {
        if let Some(mock) = schema.as_object_mut() {
            mock.insert("extra_allowed_hosts".to_owned(), serde_json::json!([]));
        }
    }
}

impl Validate for AiSettings {
    fn check(&self) -> Result<(), AppError> {
        self.validate()
    }

    fn extend_patch_schema(schema: &mut serde_json::Value) {
        if let Some(agent) = schema
            .get_mut("agent")
            .and_then(serde_json::Value::as_object_mut)
        {
            agent.insert(
                "role_models".to_owned(),
                serde_json::json!({"fast": null, "deep": null, "vision": null}),
            );
            agent.insert(
                "fallbacks".to_owned(),
                serde_json::json!({"main": [], "fast": [], "deep": [], "vision": []}),
            );
            agent.insert("global_instructions".to_owned(), serde_json::json!(""));
            agent.insert(
                "default_permission_mode".to_owned(),
                serde_json::json!("auto_read_only"),
            );
            agent.insert("wrap_up_last_step".to_owned(), serde_json::json!(true));
            agent.insert("prompt_cache".to_owned(), serde_json::json!("auto"));
            agent.insert("sandbox_auto_run".to_owned(), serde_json::json!(false));
            agent.insert("max_run_tokens".to_owned(), serde_json::Value::Null);
            // A map keyed by MCP server id: an empty object lets any key through.
            agent.insert("mcp_call_timeout_s".to_owned(), serde_json::json!({}));
        }
    }

    fn normalize_stored(&mut self) {
        // The six built-in patterns used to be stored as if the user had typed them; they are
        // pinned rules now.
        self.agent
            .blocked_commands
            .retain(|pattern| !LEGACY_DEFAULT_BLOCKED.contains(&pattern.as_str()));
        // The approval timeout once allowed 0 ("never") and 30..60; both read as a valid value.
        let timeout = &mut self.agent.approval_timeout_s;
        *timeout = if *timeout == 0 {
            AI_APPROVAL_TIMEOUT_DEFAULT_S
        } else {
            (*timeout).clamp(60, 3600)
        };
        // Ranges narrowed to the PRD's (sub-agents 1..8, image edge 512..2,048): old rows clamp.
        self.agent.max_subagents = self.agent.max_subagents.clamp(1, 8);
        self.agent.image_max_edge_px = self.agent.image_max_edge_px.clamp(512, 2048);
        // A row written before extra hosts forced tokens reads as tokens on.
        if self.gateway.forces_token() {
            self.gateway.require_token = true;
        }
    }
}

impl Validate for GeneralSettings {
    fn check(&self) -> Result<(), AppError> {
        self.validate()
    }

    fn extend_patch_schema(schema: &mut serde_json::Value) {
        if let Some(general) = schema.as_object_mut() {
            general.insert("notify_when_away".to_owned(), serde_json::json!("gates"));
        }
    }
}

macro_rules! validated {
    ($($ty:ty),+ $(,)?) => {$(
        impl Validate for $ty {
            fn check(&self) -> Result<(), AppError> {
                self.validate()
            }
        }
    )+};
}

validated!(
    AppearanceSettings,
    EditorSettings,
    NetworkSettings,
    DataSettings,
    ShortcutsSettings,
    SyncSettings,
    AdvancedSettings,
);
