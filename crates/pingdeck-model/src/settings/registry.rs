//! The settings registry: the one table of ranges, allowed values and flags the Settings UI reads.
//!
//! Single responsibility: describe every settings field for validation on the UI side
//! (`settings_registry_get`). Defaults are not written down here: they are read from
//! [`Settings::default`], so they cannot drift. The ranges mirror the validators in `sections`;
//! the tests probe each validator at both bounds and fail when the two disagree, and when a
//! settings field has no row (or a row names no field). The constants at the end are the fixed
//! retention and failover facts the Settings pages show read-only.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::limits::{CAPTURE_MAX_BYTES, CAPTURE_MAX_DAYS, LOG_MAX_DAYS, LOG_MAX_ROWS};
use super::policy::local_only_fields;
use super::{Settings, SettingsSection};

/// Version of the registry's shape; bumped when a property changes meaning.
pub const REGISTRY_VERSION: u32 = 1;

/// Longest secret value `settings_secret_set` accepts, in bytes (a drift test pins it).
const SECRET_MAX_BYTES: i64 = 4096;

pub const KIB: i64 = 1024;
pub const MIB: i64 = 1024 * KIB;

/// What a field holds, for the control the UI renders and the unit it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryKind {
    /// A switch.
    Bool,
    /// One of `options`.
    Enum,
    /// A whole number in `min..=max`.
    Int,
    /// A decimal number (`min` is exclusive when `exclusive_min`); `null` turns the limit off.
    Decimal,
    /// A size in bytes.
    SizeBytes,
    /// A duration in milliseconds.
    DurationMs,
    /// A duration in seconds.
    DurationS,
    /// Free text (`null` or empty when unset).
    Text,
    /// A list of strings bounded by `max_items` and `max_len` per entry.
    List,
    /// A keyed object; `max_items` bounds its entries and `max_len` each value.
    Map,
}

/// A rule tying a field to another one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossRule {
    /// The other field's dotted id.
    pub other: String,
    /// `ne`: the values must differ; `le`: this value must not exceed the other.
    pub rule: CrossRuleKind,
}

/// How a [`CrossRule`] compares two fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossRuleKind {
    /// The two values must differ.
    Ne,
    /// This value must not exceed the other one.
    Le,
}

/// One settings field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryField {
    /// Dotted path in the settings DTO, `section.field` (`ai.gateway.port`).
    pub id: String,
    /// What the field holds.
    pub kind: RegistryKind,
    /// The value of a fresh profile (read from [`Settings::default`]).
    pub default: Value,
    /// Smallest allowed number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    /// Largest allowed number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    /// `min` itself is not allowed (a cost cap must be above 0).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exclusive_min: bool,
    /// A number below `min` that also means "off" (balance refresh: 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off: Option<i64>,
    /// Fewest bytes of a text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_len: Option<u32>,
    /// Longest text, list entry or map value, in bytes (`ai.agent.default_workspace` counts
    /// characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_len: Option<u32>,
    /// Most list or map entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u32>,
    /// The allowed values of an enum, as stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// The only numbers allowed, when the setting is a choice of numbers (days), as stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<i64>>,
    /// The value is never taken from a sync import (this machine decides).
    pub local_only: bool,
    /// Changing the value restarts the local gateway (running requests drain first).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restart_required: bool,
    /// Rules against other fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cross: Vec<CrossRule>,
}

/// A fixed fact about retention or failover, shown read-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RegistryConstant {
    /// A count, a number of days or of megabytes.
    Number(i64),
    /// A set of words (the failover status codes).
    List(Vec<String>),
}

/// What `settings_registry_get` returns: every field and the read-only constants. Contains no
/// secret and no user data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsRegistryDto {
    /// [`REGISTRY_VERSION`].
    pub version: u32,
    /// Every settings field, section by section in the order of the settings DTO.
    pub fields: Vec<RegistryField>,
    /// `log.max_rows`, `log.max_days`, `capture.max_days`, `capture.max_mb`,
    /// `failover.status_codes` and `secret.max_bytes`.
    pub constants: BTreeMap<String, RegistryConstant>,
}

/// A table row: the id, the kind and what constrains it. Everything else is derived.
struct Row {
    id: &'static str,
    kind: RegistryKind,
    min: Option<i64>,
    max: Option<i64>,
    exclusive_min: bool,
    off: Option<i64>,
    min_len: Option<u32>,
    max_len: Option<u32>,
    max_items: Option<u32>,
    options: &'static [&'static str],
    values: &'static [i64],
    restart_required: bool,
    cross: &'static [(&'static str, CrossRuleKind)],
}

impl Row {
    const fn new(id: &'static str, kind: RegistryKind) -> Self {
        Self {
            id,
            kind,
            min: None,
            max: None,
            exclusive_min: false,
            off: None,
            min_len: None,
            max_len: None,
            max_items: None,
            options: &[],
            values: &[],
            restart_required: false,
            cross: &[],
        }
    }

    const fn range(mut self, min: i64, max: i64) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        self
    }

    const fn off(mut self, off: i64) -> Self {
        self.off = Some(off);
        self
    }

    const fn text_len(mut self, min: u32, max: u32) -> Self {
        self.min_len = Some(min);
        self.max_len = Some(max);
        self
    }

    const fn max_text(mut self, max: u32) -> Self {
        self.max_len = Some(max);
        self
    }

    /// A lower bound that is not itself allowed (and no upper bound).
    const fn above(mut self, min: i64) -> Self {
        self.min = Some(min);
        self.exclusive_min = true;
        self
    }

    const fn items(mut self, max_items: u32, max_len: u32) -> Self {
        self.max_items = Some(max_items);
        self.max_len = Some(max_len);
        self
    }

    const fn restarts_gateway(mut self) -> Self {
        self.restart_required = true;
        self
    }

    const fn cross(mut self, cross: &'static [(&'static str, CrossRuleKind)]) -> Self {
        self.cross = cross;
        self
    }

    /// A number that may only be one of `values` (a choice of days), inside the range.
    const fn values(mut self, values: &'static [i64]) -> Self {
        self.values = values;
        self
    }
}

const fn flag(id: &'static str) -> Row {
    Row::new(id, RegistryKind::Bool)
}
const fn int(id: &'static str, min: i64, max: i64) -> Row {
    Row::new(id, RegistryKind::Int).range(min, max)
}
const fn bytes(id: &'static str, min: i64, max: i64) -> Row {
    Row::new(id, RegistryKind::SizeBytes).range(min, max)
}
const fn choice(id: &'static str, options: &'static [&'static str]) -> Row {
    let mut row = Row::new(id, RegistryKind::Enum);
    row.options = options;
    row
}
const fn text(id: &'static str) -> Row {
    Row::new(id, RegistryKind::Text)
}
const fn list(id: &'static str, max_items: u32, max_len: u32) -> Row {
    Row::new(id, RegistryKind::List).items(max_items, max_len)
}

/// Every settings field, section by section in the order of [`Settings`].
const TABLE: &[Row] = &[
    choice("general.language", &["zh-CN", "en"]),
    flag("general.restore_session"),
    flag("general.check_updates_on_launch"),
    flag("general.dock_bounce"),
    choice("appearance.theme", &["system", "light", "dark"]),
    int("appearance.ui_font_size", 10, 26),
    int("appearance.mono_font_size", 10, 24),
    choice("appearance.density", &["comfortable", "compact"]),
    choice("appearance.reduce_motion", &["system", "on", "off"]),
    choice("appearance.sidebar", &["auto", "expanded", "rail"]),
    int("editor.tab_size", 1, 8),
    flag("editor.word_wrap"),
    bytes("editor.editor_highlight_limit_bytes", KIB, 64 * MIB),
    bytes("editor.viewer_highlight_limit_bytes", KIB, 64 * MIB),
    Row::new("network.timeout_ms", RegistryKind::DurationMs).range(1, 3_600_000),
    flag("network.follow_redirects"),
    int("network.max_redirects", 0, 50),
    flag("network.verify_tls"),
    text("network.proxy_url"),
    list("network.no_proxy", 64, 255),
    text("network.ca_bundle_path").max_text(1024),
    choice(
        "network.http_version",
        &["Auto", "Http10", "Http11", "Http2"],
    ),
    int("network.retry_count", 0, 10),
    int("data.history_retention", 10, 100_000),
    bytes("data.response_body_cap_bytes", KIB, 512 * MIB),
    bytes("data.inline_threshold_bytes", KIB, 64 * MIB),
    bytes("data.preview_bytes", KIB, 64 * MIB)
        .cross(&[("data.inline_threshold_bytes", CrossRuleKind::Le)]),
    Row::new("data.chat_retention_days", RegistryKind::Int)
        .range(0, 365)
        .values(&[0, 30, 90, 180, 365]),
    int("mock.first_port", 1024, 65_535).cross(&[("ai.gateway.port", CrossRuleKind::Ne)]),
    Row::new("mock.default_latency_ms", RegistryKind::DurationMs).range(0, 60_000),
    flag("mock.cors_default"),
    int("mock.log_capacity", 10, 5_000),
    flag("ai.gateway.enabled_on_start"),
    flag("ai.gateway.serve_lan").restarts_gateway(),
    list("ai.gateway.extra_allowed_hosts", 16, 253),
    int("ai.gateway.port", 1024, 65_535)
        .restarts_gateway()
        .cross(&[("mock.first_port", CrossRuleKind::Ne)]),
    flag("ai.gateway.require_token"),
    int("ai.gateway.max_retries", 0, 10),
    Row::new("ai.gateway.first_byte_timeout_s", RegistryKind::DurationS).range(1, 600),
    Row::new("ai.gateway.idle_timeout_s", RegistryKind::DurationS).range(1, 3_600),
    int("ai.gateway.circuit_failure_threshold", 1, 100),
    Row::new("ai.gateway.circuit_window_s", RegistryKind::DurationS).range(1, 3_600),
    Row::new("ai.gateway.circuit_open_s", RegistryKind::DurationS).range(5, 3_600),
    Row::new("ai.gateway.circuit_open_max_s", RegistryKind::DurationS).range(5, 86_400),
    choice("ai.gateway.capture_mode", &["off", "errors_only", "all"]),
    flag("ai.providers.catalog_auto_refresh"),
    text("ai.providers.default_test_prompt").text_len(1, 2000),
    int("ai.providers.probe_concurrency", 1, 16),
    Row::new("ai.providers.probe_timeout_s", RegistryKind::DurationS).range(5, 300),
    int("ai.providers.balance_refresh_min", 5, 1_440).off(0),
    int("ai.providers.usage_retention_days", 30, 3_650),
    Row::new("ai.agent.default_target", RegistryKind::Map),
    choice(
        "ai.agent.default_effort",
        &["auto", "minimal", "low", "medium", "high", "max"],
    ),
    text("ai.agent.default_workspace").text_len(1, 200),
    flag("ai.agent.auto_title"),
    Row::new("ai.agent.approval_timeout_s", RegistryKind::DurationS).range(60, 3_600),
    int("ai.agent.max_tool_iterations", 1, 100),
    int("ai.agent.max_concurrent_runs", 1, 8),
    int("ai.agent.max_subagents", 1, 8),
    int("ai.agent.max_image_mb", 1, 20),
    int("ai.agent.max_file_mb", 1, 100),
    int("ai.agent.image_max_edge_px", 512, 2_048),
    flag("ai.agent.auto_compact"),
    int("ai.agent.compact_threshold_pct", 50, 95),
    int("ai.agent.keep_recent_turns", 1, 20),
    Row::new("ai.agent.cost_cap_usd", RegistryKind::Decimal).above(0),
    Row::new("ai.agent.cost_cap_cny", RegistryKind::Decimal).above(0),
    flag("ai.agent.sandbox_writes_to_workspace"),
    Row::new("ai.agent.shell_timeout_s", RegistryKind::DurationS).range(1, 3_600),
    list("ai.agent.blocked_commands", 64, 256),
    list("ai.agent.shell_env_passthrough", 64, 128),
    list("ai.agent.skill_dirs", 32, 1024),
    choice(
        "ai.search.backend",
        &["auto", "duckduckgo", "brave", "tavily", "searxng"],
    ),
    text("ai.search.searxng_base_url"),
    flag("ai.search.use_duckduckgo"),
    text("ai.search.tavily_key_hint"),
    text("ai.search.brave_key_hint"),
    Row::new("shortcuts.overrides", RegistryKind::Map).items(200, 64),
    text("sync.directory"),
    flag("sync.include_secrets"),
    flag("sync.auto_export"),
    int("advanced.script_loop_iteration_limit", 1_000, 100_000_000),
    int("advanced.script_recursion_limit", 16, 4_096),
    int("advanced.script_max_send_requests", 0, 1_000),
    int("advanced.console_max_entries", 10, 100_000),
    bytes("advanced.console_max_message_bytes", 256, 4 * MIB),
    int("advanced.max_test_results", 10, 100_000),
];

/// The settings section a dotted id belongs to, and the id relative to it.
pub fn split_id(id: &str) -> Option<(SettingsSection, &str)> {
    let (head, rest) = id.split_once('.')?;
    let section = SettingsSection::ALL
        .iter()
        .copied()
        .find(|section| section.as_str() == head)?;
    Some((section, rest))
}

/// Whether a sync import leaves `id` alone: the listed fields of `local_only_fields`, and the
/// whole `sync` section (never synced).
fn is_local_only(id: &str) -> bool {
    split_id(id).is_some_and(|(section, rest)| {
        section == SettingsSection::Sync || local_only_fields(section).contains(&rest)
    })
}

impl Row {
    fn field(&self, defaults: &Value) -> RegistryField {
        let pointer = format!("/{}", self.id.replace('.', "/"));
        RegistryField {
            id: self.id.to_owned(),
            kind: self.kind,
            default: defaults.pointer(&pointer).cloned().unwrap_or(Value::Null),
            min: self.min,
            max: self.max,
            exclusive_min: self.exclusive_min,
            off: self.off,
            min_len: self.min_len,
            max_len: self.max_len,
            max_items: self.max_items,
            values: (!self.values.is_empty()).then(|| self.values.to_vec()),
            options: (!self.options.is_empty()).then(|| {
                self.options
                    .iter()
                    .map(|option| (*option).to_owned())
                    .collect()
            }),
            local_only: is_local_only(self.id),
            restart_required: self.restart_required,
            cross: self
                .cross
                .iter()
                .map(|(other, rule)| CrossRule {
                    other: (*other).to_owned(),
                    rule: *rule,
                })
                .collect(),
        }
    }
}

/// The registry: every settings field with its range, allowed values and flags, plus the
/// read-only retention and failover constants.
#[must_use]
pub fn settings_registry() -> SettingsRegistryDto {
    let defaults = serde_json::to_value(Settings::default()).unwrap_or(Value::Null);
    let number = |value: i64| RegistryConstant::Number(value);
    let constants = [
        ("log.max_rows", number(i64::from(LOG_MAX_ROWS))),
        ("log.max_days", number(i64::from(LOG_MAX_DAYS))),
        ("capture.max_days", number(i64::from(CAPTURE_MAX_DAYS))),
        (
            "capture.max_mb",
            number(i64::try_from(CAPTURE_MAX_BYTES / (1024 * 1024)).unwrap_or(i64::MAX)),
        ),
        (
            "failover.status_codes",
            RegistryConstant::List(["401", "402", "403", "429"].map(str::to_owned).into()),
        ),
        ("secret.max_bytes", number(SECRET_MAX_BYTES)),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect();
    SettingsRegistryDto {
        version: REGISTRY_VERSION,
        fields: TABLE.iter().map(|row| row.field(&defaults)).collect(),
        constants,
    }
}
