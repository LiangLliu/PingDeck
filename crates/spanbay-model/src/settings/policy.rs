//! Which settings fields stay local, and which settings are secret-valued.

use super::SettingsSection;

/// The fields of `section` (dotted, relative to it) that never leave this device: a sync import
/// never takes them from files and an export never writes them. They widen what the machine runs or
/// exposes, name local folders, rules or addresses, delete data, or describe local secrets:
/// language, the start-up switches and
/// the away-from-app signals, TLS verification, the proxy URL, bypass list and CA bundle, the
/// history, chat and usage retention, the gateway's LAN binding, port, start-up switch, extra
/// allowed hosts, `require_token` and capture mode, the agent's default permission mode, default
/// folder, write confinement, safety rules, extra shell variables, skill folders, spend caps,
/// concurrency and tool-iteration limits, the SearXNG address and the DuckDuckGo switch, the search
/// key hints, and the script sandbox limits. Everything else is synced.
#[must_use]
pub fn local_only_fields(section: SettingsSection) -> &'static [&'static str] {
    match section {
        SettingsSection::General => &[
            "language",
            "restore_session",
            "check_updates_on_launch",
            "dock_bounce",
            "notify_when_away",
        ],
        SettingsSection::Network => &["verify_tls", "no_proxy", "proxy_url", "ca_bundle_path"],
        SettingsSection::Data => &["history_retention", "chat_retention_days"],
        SettingsSection::Ai => &[
            "gateway.enabled_on_start",
            "gateway.port",
            "gateway.serve_lan",
            "gateway.extra_allowed_hosts",
            "gateway.require_token",
            "gateway.capture_mode",
            "providers.usage_retention_days",
            "agent.default_permission_mode",
            "agent.sandbox_auto_run",
            "agent.max_run_tokens",
            "agent.default_workspace",
            "agent.sandbox_writes_to_workspace",
            "agent.blocked_commands",
            "agent.shell_env_passthrough",
            "agent.skill_dirs",
            "agent.cost_cap_usd",
            "agent.cost_cap_cny",
            "agent.max_concurrent_runs",
            "agent.max_tool_iterations",
            "search.searxng_base_url",
            "search.use_duckduckgo",
            "search.tavily_key_hint",
            "search.brave_key_hint",
        ],
        SettingsSection::Mock => &["extra_allowed_hosts"],
        SettingsSection::Advanced => &[
            "script_loop_iteration_limit",
            "script_recursion_limit",
            "script_max_send_requests",
        ],
        _ => &[],
    }
}

/// Settings fields (with their section) a patch may name that the frozen registry sample does not
/// list, each classified: `true` stays on this device (and is in [`local_only_fields`]), `false`
/// is synced. A new unregistered field must be added here, so it is never synced by accident.
pub const UNREGISTERED_FIELDS: &[(&str, bool)] = &[
    ("ai.agent.role_models", false),
    ("ai.agent.fallbacks", false),
    ("ai.agent.global_instructions", false),
    ("ai.agent.default_permission_mode", true),
    ("ai.agent.wrap_up_last_step", false),
    ("ai.agent.prompt_cache", false),
    ("ai.agent.sandbox_auto_run", true),
    ("ai.agent.max_run_tokens", true),
    ("ai.agent.mcp_call_timeout_s", false),
    ("general.notify_when_away", true),
    ("mock.extra_allowed_hosts", true),
];

/// A search credential the settings keep a hint of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsSecret {
    /// The Tavily search API key (`ai.search.tavily`).
    SearchTavily,
    /// The Brave Search API key (`ai.search.brave`).
    SearchBrave,
}
