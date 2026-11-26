//! Settings › 智能体 (Agent) group controller hosting all 8 real pages:
//! Defaults (`Agent`), Personalization, Project Instructions & Trust (`Memory`),
//! Subagents, Tools, Permissions, MCP Servers, and Skills.

mod actions;
mod actions_records;
mod mcp_sheet;
mod mcp_sheet_render;
mod page_defaults;
mod page_mcp;
mod page_memory;
mod page_permissions;
mod page_permissions_rules;
mod page_personalization;
mod page_skills;
mod page_subagents;
mod page_tools;
mod widgets;

use std::collections::BTreeMap;

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Task, Window, div, px,
};
use pingdeck_desktop::core::agent::mcp::{AgentMcpServerView, McpServerStatus, McpTrustState};
use pingdeck_desktop::core::agent::model::AgentFolderTrust;
use pingdeck_desktop::core::app::{
    AgentRoleStatus, AgentSkillEntry, AgentToolEntry, AiConfigDto, BlockedRule, PermissionRuleView,
    SandboxTestDto, SearchTestResultDto, SkillFolderDto, WorkspaceInfo,
    agent_folder_trust_list_impl, agent_permission_rule_views_impl, agent_roles_get_impl,
    agent_sandbox_test_impl, agent_skill_entries_impl, agent_tool_entries_impl,
    agent_workspace_info_impl, ai_config_get_impl, settings_get_impl,
};
use pingdeck_desktop::core::model::settings::Settings;
use pingdeck_desktop::state::SettingsSection;

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into};
use crate::ui::{InputEvent, InputState, TextareaState, Tone, v_col};

pub use mcp_sheet::McpSheet;
use widgets::{ModelChoice, collect_model_choices, flash_banner};

/// Enriched trusted-folder row combining DB trust record with live disk inspection.
#[derive(Debug, Clone)]
pub struct TrustedFolderRow {
    pub trust: AgentFolderTrust,
    pub info: Option<WorkspaceInfo>,
}

/// Shared stateful view covering all 8 pages of `SettingsGroup::Agent`.
pub struct AgentSettingsView {
    pub active: SettingsSection,
    pub settings: Settings,
    pub config: Option<AiConfigDto>,
    pub model_choices: Vec<ModelChoice>,
    pub roles: Vec<AgentRoleStatus>,
    pub tools: Vec<AgentToolEntry>,
    pub trusted_folders: Vec<TrustedFolderRow>,
    pub permission_rules: Vec<PermissionRuleView>,
    pub rules_page: usize,
    pub blocked_rules: Vec<BlockedRule>,
    pub skill_entries: Vec<AgentSkillEntry>,
    pub skill_folders: Vec<SkillFolderDto>,
    pub sandbox: Option<SandboxTestDto>,

    // UI expand/disclosure & transient feedback states
    pub limits_open: bool,
    pub flash: Option<(Tone, String)>,
    pub search_testing: bool,
    pub search_test_result: Option<SearchTestResultDto>,
    pub mcp_confirm_start_id: Option<String>,
    pub mcp_confirm_delete_id: Option<String>,
    pub mcp_restarting_id: Option<String>,
    pub mcp_log_open_id: Option<String>,
    pub mcp_logs: BTreeMap<String, Vec<String>>,

    // Stateful inputs synced with core settings
    pub compact_pct_input: Entity<InputState>,
    pub keep_turns_input: Entity<InputState>,
    pub max_steps_input: Entity<InputState>,
    pub max_runs_input: Entity<InputState>,
    pub cost_usd_input: Entity<InputState>,
    pub cost_cny_input: Entity<InputState>,
    pub max_tokens_input: Entity<InputState>,
    pub max_image_input: Entity<InputState>,
    pub max_file_input: Entity<InputState>,
    pub max_edge_input: Entity<InputState>,
    pub max_subagents_input: Entity<InputState>,
    pub approval_timeout_input: Entity<InputState>,
    pub shell_timeout_input: Entity<InputState>,
    pub blocked_cmd_input: Entity<InputState>,
    pub env_pass_input: Entity<InputState>,
    pub searxng_url_input: Entity<InputState>,
    pub tavily_key_input: Entity<InputState>,
    pub brave_key_input: Entity<InputState>,
    pub instructions_input: Entity<TextareaState>,
    pub instructions_dirty: bool,
    pub instructions_synced_text: String,
    pub(crate) pending_sync_inputs: bool,

    _subscriptions: Vec<Subscription>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl AgentSettingsView {
    pub fn new(initial: SettingsSection, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = settings_get_impl(runtime(cx).state());
        let ag = &settings.ai.agent;
        let search = &settings.ai.search;

        let mk_val =
            |val: String, ph: &'static str, window: &mut Window, cx: &mut Context<Self>| {
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(ph)
                        .default_value(val)
                })
            };
        let mk_ph = |ph: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(ph))
        };

        let compact_pct_input = mk_val(ag.compact_threshold_pct.to_string(), "85", window, cx);
        let keep_turns_input = mk_val(ag.keep_recent_turns.to_string(), "2", window, cx);
        let max_steps_input = mk_val(ag.max_tool_iterations.to_string(), "20", window, cx);
        let max_runs_input = mk_val(ag.max_concurrent_runs.to_string(), "3", window, cx);
        let cost_usd_input = mk_val(
            ag.cost_cap_usd.map(|v| format!("{v}")).unwrap_or_default(),
            "Off",
            window,
            cx,
        );
        let cost_cny_input = mk_val(
            ag.cost_cap_cny.map(|v| format!("{v}")).unwrap_or_default(),
            "Off",
            window,
            cx,
        );
        let max_tokens_input = mk_val(
            ag.max_run_tokens.map(|v| v.to_string()).unwrap_or_default(),
            "Off",
            window,
            cx,
        );
        let max_image_input = mk_val(ag.max_image_mb.to_string(), "10", window, cx);
        let max_file_input = mk_val(ag.max_file_mb.to_string(), "20", window, cx);
        let max_edge_input = mk_val(ag.image_max_edge_px.to_string(), "2048", window, cx);
        let max_subagents_input = mk_val(ag.max_subagents.to_string(), "3", window, cx);
        let approval_timeout_input = mk_val(ag.approval_timeout_s.to_string(), "600", window, cx);
        let shell_timeout_input = mk_val(ag.shell_timeout_s.to_string(), "120", window, cx);
        let blocked_cmd_input = mk_ph("git push *", window, cx);
        let env_pass_input = mk_ph("NODE_ENV", window, cx);
        let searxng_url_input = mk_val(
            search.searxng_base_url.clone(),
            "http://127.0.0.1:8080",
            window,
            cx,
        );
        let tavily_key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("tvly-…")
                .masked(true)
        });
        let brave_key_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("BSA…").masked(true));

        let init_instr = ag.global_instructions.clone();
        let instructions_input = cx.new(|cx| {
            let ph = crate::ui::tr(cx).settings().agent_persona_placeholder();
            TextareaState::new(window, cx)
                .placeholder(ph)
                .default_value(init_instr.clone())
        });

        let mut subs = Vec::new();
        macro_rules! on_commit {
            ($ent:expr, $method:ident) => {
                subs.push(cx.subscribe(&$ent, |this, _, ev: &InputEvent, cx| {
                    if matches!(ev, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        this.$method(cx);
                    }
                }));
            };
        }
        on_commit!(compact_pct_input, commit_compact_pct);
        on_commit!(keep_turns_input, commit_keep_turns);
        on_commit!(max_steps_input, commit_max_steps);
        on_commit!(max_runs_input, commit_max_runs);
        on_commit!(cost_usd_input, commit_cost_usd);
        on_commit!(cost_cny_input, commit_cost_cny);
        on_commit!(max_tokens_input, commit_max_tokens);
        on_commit!(max_image_input, commit_max_image);
        on_commit!(max_file_input, commit_max_file);
        on_commit!(max_edge_input, commit_max_edge);
        on_commit!(max_subagents_input, commit_max_subagents);
        on_commit!(approval_timeout_input, commit_approval_timeout);
        on_commit!(shell_timeout_input, commit_shell_timeout);
        on_commit!(searxng_url_input, commit_searxng_url);

        subs.push(cx.subscribe_in(
            &blocked_cmd_input,
            window,
            |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.add_blocked_command(window, cx);
                }
            },
        ));
        subs.push(cx.subscribe_in(
            &env_pass_input,
            window,
            |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.add_shell_env(window, cx);
                }
            },
        ));
        subs.push(
            cx.subscribe(&instructions_input, |this, state, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    let current = state.read(cx).value().to_string();
                    this.instructions_dirty = current != this.instructions_synced_text;
                    cx.notify();
                }
            }),
        );

        let mut view = Self {
            active: initial,
            settings,
            config: None,
            model_choices: Vec::new(),
            roles: Vec::new(),
            tools: Vec::new(),
            trusted_folders: Vec::new(),
            permission_rules: Vec::new(),
            rules_page: 0,
            blocked_rules: Vec::new(),
            skill_entries: Vec::new(),
            skill_folders: Vec::new(),
            sandbox: None,
            limits_open: false,
            flash: None,
            search_testing: false,
            search_test_result: None,
            mcp_confirm_start_id: None,
            mcp_confirm_delete_id: None,
            mcp_restarting_id: None,
            mcp_log_open_id: None,
            mcp_logs: BTreeMap::new(),
            compact_pct_input,
            keep_turns_input,
            max_steps_input,
            max_runs_input,
            cost_usd_input,
            cost_cny_input,
            max_tokens_input,
            max_image_input,
            max_file_input,
            max_edge_input,
            max_subagents_input,
            approval_timeout_input,
            shell_timeout_input,
            blocked_cmd_input,
            env_pass_input,
            searxng_url_input,
            tavily_key_input,
            brave_key_input,
            instructions_input,
            instructions_dirty: false,
            instructions_synced_text: init_instr,
            pending_sync_inputs: false,
            _subscriptions: subs,
            _tasks: Vec::new(),
        };
        view.reload_all(cx);
        view
    }

    /// Synchronizes local state & schedules input refresh after external settings updates
    /// (such as Reset All, Undo Reset, or Folder Sync import).
    pub fn sync_from_external_settings(&mut self, next: Settings, cx: &mut Context<Self>) {
        if self.settings != next {
            self.settings = next;
            self.instructions_dirty = false;
            self.pending_sync_inputs = true;
            cx.notify();
        }
    }

    /// Switches the active sub-page inside `SettingsGroup::Agent` and refreshes dynamic data.
    pub fn set_section(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        let changed = self.active != section;
        self.active = section;
        if changed {
            self.flash = None;
            self.mcp_confirm_start_id = None;
            self.mcp_confirm_delete_id = None;
            self.rules_page = 0;
        }
        self.reload_all(cx);
    }

    /// Counts enabled MCP servers requiring attention (auth, review, drifted tools, or error).
    #[must_use]
    pub fn mcp_attention_count(&self) -> usize {
        self.config
            .as_ref()
            .map_or(0, |c| count_mcp_attention(&c.mcp_servers))
    }

    /// Reloads the aggregated AI snapshot, roles, tools, trusted folders, rules, skills, and sandbox.
    pub fn reload_all(&mut self, cx: &mut Context<Self>) {
        let async_task = spawn_async_into(
            cx,
            |state| async move {
                let cfg = ai_config_get_impl(&state).await.ok();
                let tools = agent_tool_entries_impl(&state, None).await.ok();
                (cfg, tools)
            },
            |this, (cfg, tools), cx| {
                if let Some(c) = cfg {
                    this.model_choices = collect_model_choices(&c.providers, &c.models);
                    this.blocked_rules = c.blocked.clone();
                    this.skill_folders = c.skill_folders.clone();
                    if tools.is_none() {
                        this.tools = c.tools.clone();
                    }
                    this.config = Some(c);
                }
                if let Some(t) = tools {
                    this.tools = t;
                }
                cx.notify();
            },
        );
        self._tasks.push(async_task);

        let block_task = spawn_blocking_into(
            cx,
            |state| {
                let roles = agent_roles_get_impl(state).unwrap_or_default();
                let trusts = agent_folder_trust_list_impl(state).unwrap_or_default();
                let trusted_folders: Vec<TrustedFolderRow> = trusts
                    .into_iter()
                    .map(|trust| {
                        let info = agent_workspace_info_impl(state, &trust.path).ok();
                        TrustedFolderRow { trust, info }
                    })
                    .collect();
                let rules = agent_permission_rule_views_impl(state, None, None).unwrap_or_default();
                let skills = agent_skill_entries_impl(state, None).unwrap_or_default();
                let sandbox = agent_sandbox_test_impl();
                (roles, trusted_folders, rules, skills, sandbox)
            },
            |this, (roles, trusted_folders, rules, skills, sandbox), cx| {
                // Read authoritative settings on the UI thread after slow blocking work (such as
                // `agent_sandbox_test_impl`) completes so a pre-edit snapshot can never clobber
                // concurrent settings commits.
                let latest = settings_get_impl(runtime(cx).state());
                if this.settings != latest {
                    this.settings = latest;
                    this.pending_sync_inputs = true;
                }
                this.roles = roles;
                this.trusted_folders = trusted_folders;
                this.permission_rules = rules;
                this.skill_entries = skills;
                this.sandbox = Some(sandbox);
                cx.notify();
            },
        );
        self._tasks.push(block_task);
    }

    pub(super) fn sync_inputs_from_settings(
        &mut self,
        window: &mut Window,
        cx: &mut gpui_kit::App,
    ) {
        let ag = &self.settings.ai.agent;
        let search = &self.settings.ai.search;
        let set_i =
            |ent: &Entity<InputState>, v: String, w: &mut Window, cx: &mut gpui_kit::App| {
                ent.update(cx, |s, cx| s.set_value(v, w, cx));
            };
        set_i(
            &self.compact_pct_input,
            ag.compact_threshold_pct.to_string(),
            window,
            cx,
        );
        set_i(
            &self.keep_turns_input,
            ag.keep_recent_turns.to_string(),
            window,
            cx,
        );
        set_i(
            &self.max_steps_input,
            ag.max_tool_iterations.to_string(),
            window,
            cx,
        );
        set_i(
            &self.max_runs_input,
            ag.max_concurrent_runs.to_string(),
            window,
            cx,
        );
        set_i(
            &self.cost_usd_input,
            ag.cost_cap_usd.map(|v| format!("{v}")).unwrap_or_default(),
            window,
            cx,
        );
        set_i(
            &self.cost_cny_input,
            ag.cost_cap_cny.map(|v| format!("{v}")).unwrap_or_default(),
            window,
            cx,
        );
        set_i(
            &self.max_tokens_input,
            ag.max_run_tokens.map(|v| v.to_string()).unwrap_or_default(),
            window,
            cx,
        );
        set_i(
            &self.max_image_input,
            ag.max_image_mb.to_string(),
            window,
            cx,
        );
        set_i(&self.max_file_input, ag.max_file_mb.to_string(), window, cx);
        set_i(
            &self.max_edge_input,
            ag.image_max_edge_px.to_string(),
            window,
            cx,
        );
        set_i(
            &self.max_subagents_input,
            ag.max_subagents.to_string(),
            window,
            cx,
        );
        set_i(
            &self.approval_timeout_input,
            ag.approval_timeout_s.to_string(),
            window,
            cx,
        );
        set_i(
            &self.shell_timeout_input,
            ag.shell_timeout_s.to_string(),
            window,
            cx,
        );
        set_i(
            &self.searxng_url_input,
            search.searxng_base_url.clone(),
            window,
            cx,
        );
        if !self.instructions_dirty {
            let txt = ag.global_instructions.clone();
            self.instructions_synced_text = txt.clone();
            self.instructions_input
                .update(cx, |s, cx| s.set_value(txt, window, cx));
        }
    }
}

/// Counts MCP servers needing user action (for sidebar badge).
#[must_use]
pub fn count_mcp_attention(servers: &[AgentMcpServerView]) -> usize {
    servers
        .iter()
        .filter(|v| {
            v.server.enabled
                && (matches!(
                    v.status,
                    McpServerStatus::NeedsAuth
                        | McpServerStatus::NeedsReview
                        | McpServerStatus::Error { .. }
                ) || v.trust_state == Some(McpTrustState::NeedsReview))
        })
        .count()
}

impl Render for AgentSettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_sync_inputs {
            self.pending_sync_inputs = false;
            self.sync_inputs_from_settings(window, cx);
        }
        let lang = crate::ui::active_language(cx);

        let mut root = v_col().w_full().gap(px(18.0)).pb(px(28.0));

        if let Some((tone, msg)) = self.flash.clone() {
            root = root.child(flash_banner(
                tone,
                &msg,
                cx.listener(|this, _, _, cx| {
                    this.flash = None;
                    cx.notify();
                }),
                cx,
            ));
        }

        let page: gpui_kit::AnyElement = match self.active {
            SettingsSection::Agent => {
                page_defaults::render_defaults(self, lang, cx).into_any_element()
            }
            SettingsSection::Personalization => {
                page_personalization::render_personalization(self, lang, cx).into_any_element()
            }
            SettingsSection::Memory => {
                page_memory::render_memory(self, lang, cx).into_any_element()
            }
            SettingsSection::Subagents => {
                page_subagents::render_subagents(self, lang, cx).into_any_element()
            }
            SettingsSection::Tools => page_tools::render_tools(self, lang, cx).into_any_element(),
            SettingsSection::Permissions => {
                page_permissions::render_permissions(self, lang, cx).into_any_element()
            }
            SettingsSection::Mcp => page_mcp::render_mcp(self, lang, window, cx).into_any_element(),
            SettingsSection::Skills => {
                page_skills::render_skills(self, lang, cx).into_any_element()
            }
            _ => div().into_any_element(),
        };

        root.child(page)
    }
}
