//! Native slide-over `Sheet` state and core actions for adding/editing an MCP server or
//! importing `mcpServers` JSON.

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, PathPromptOptions, Render, SharedString, Task,
    Window,
};
use pingdeck_desktop::core::agent::mcp::McpTransportConfig;
use pingdeck_desktop::core::agent::model::{AgentMcpServer, AgentMcpTestResult};
use pingdeck_desktop::core::app::{
    agent_mcp_server_save_impl, agent_mcp_server_test_impl, settings_update_impl,
};
use pingdeck_desktop::core::model::settings::SettingsSection as CoreSection;

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::{InputState, TextareaState, Tone, WindowExt as _, palette};

use super::AgentSettingsView;
use super::mcp_sheet_render::{
    parse_kv_lines, parse_mcp_servers_json, render_form_mode, render_json_mode, serialize_mcp_json,
};

enum SheetMode {
    Form { editing_id: String },
    Json { existing: Vec<AgentMcpServer> },
}

pub struct McpSheet {
    parent: Entity<AgentSettingsView>,
    mode: SheetMode,
    pub(super) is_http: bool,
    pub(super) trusted: bool,
    pub(super) enabled: bool,
    pub(super) name_input: Entity<InputState>,
    pub(super) command_input: Entity<InputState>,
    pub(super) args_input: Entity<TextareaState>,
    pub(super) cwd_input: Entity<InputState>,
    pub(super) env_input: Entity<TextareaState>,
    pub(super) url_input: Entity<InputState>,
    pub(super) headers_input: Entity<TextareaState>,
    pub(super) timeout_input: Entity<InputState>,
    pub(super) json_input: Entity<TextareaState>,
    pub(super) testing: bool,
    pub(super) saving: bool,
    pub(super) needs_stdio_confirm: bool,
    pub(super) test_result: Option<AgentMcpTestResult>,
    pub(super) error_msg: Option<String>,
    pending_close: bool,
    _tasks: Vec<Task<()>>,
}

impl McpSheet {
    pub fn for_new(
        parent: Entity<AgentSettingsView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build_form(parent, None, None, window, cx)
    }

    pub fn for_edit(
        parent: Entity<AgentSettingsView>,
        server: AgentMcpServer,
        timeout_s: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build_form(parent, Some(server), timeout_s, window, cx)
    }

    pub fn for_json(
        parent: Entity<AgentSettingsView>,
        existing: Vec<AgentMcpServer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut s = Self::build_form(parent, None, None, window, cx);
        let sample = serialize_mcp_json(&existing);
        s.json_input
            .update(cx, |st, cx| st.set_value(sample, window, cx));
        s.mode = SheetMode::Json { existing };
        s
    }

    fn build_form(
        parent: Entity<AgentSettingsView>,
        server: Option<AgentMcpServer>,
        timeout_s: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editing_id = server.as_ref().map(|s| s.id.clone()).unwrap_or_default();
        let (is_http, name, cmd, args, cwd, env_txt, url, hdr_txt, trusted, enabled) =
            if let Some(srv) = server {
                match srv.transport {
                    McpTransportConfig::Stdio {
                        command,
                        args,
                        env,
                        cwd,
                        ..
                    } => {
                        let env_lines = env
                            .into_iter()
                            .map(|(k, v)| format!("{k}={v}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        (
                            false,
                            srv.name,
                            command,
                            args.join("\n"),
                            cwd.unwrap_or_default(),
                            env_lines,
                            String::new(),
                            String::new(),
                            srv.trusted,
                            srv.enabled,
                        )
                    }
                    McpTransportConfig::Http { url, headers } => {
                        let hdr_lines = headers
                            .into_iter()
                            .map(|(k, v)| format!("{k}: {v}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        (
                            true,
                            srv.name,
                            String::new(),
                            String::new(),
                            String::new(),
                            String::new(),
                            url,
                            hdr_lines,
                            srv.trusted,
                            srv.enabled,
                        )
                    }
                }
            } else {
                (
                    false,
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    false,
                    true,
                )
            };

        let mk_in = |val: String, ph: &'static str, w: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(w, cx).placeholder(ph).default_value(val))
        };
        let mk_ta = |val: String, ph: &'static str, w: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| TextareaState::new(w, cx).placeholder(ph).default_value(val))
        };

        Self {
            parent,
            mode: SheetMode::Form { editing_id },
            is_http,
            trusted,
            enabled,
            name_input: mk_in(name, "postgres-readonly", window, cx),
            command_input: mk_in(cmd, "npx", window, cx),
            args_input: mk_ta(
                args,
                "-y\n@modelcontextprotocol/server-filesystem\n/path",
                window,
                cx,
            ),
            cwd_input: mk_in(cwd, "/absolute/working/dir (optional)", window, cx),
            env_input: mk_ta(env_txt, "KEY=VALUE (one per line)", window, cx),
            url_input: mk_in(url, "https://api.example.com/mcp", window, cx),
            headers_input: mk_ta(
                hdr_txt,
                "Authorization: Bearer … (one per line)",
                window,
                cx,
            ),
            timeout_input: mk_in(
                timeout_s.map(|v| v.to_string()).unwrap_or_default(),
                "120",
                window,
                cx,
            ),
            json_input: mk_ta(String::new(), "{\n  \"mcpServers\": {}\n}", window, cx),
            testing: false,
            saving: false,
            needs_stdio_confirm: false,
            test_result: None,
            error_msg: None,
            pending_close: false,
            _tasks: Vec::new(),
        }
    }

    pub fn sheet_title(&self, cx: &gpui_kit::App) -> SharedString {
        match &self.mode {
            SheetMode::Form { editing_id } if editing_id.is_empty() => {
                crate::ui::tr(cx).settings().tools_form_title_new().into()
            }
            SheetMode::Form { .. } => crate::ui::tr(cx)
                .settings()
                .mcp_sheet_edit_mcp_server()
                .into(),
            SheetMode::Json { .. } => crate::ui::tr(cx)
                .settings()
                .mcp_sheet_edit_mcp_servers_json()
                .into(),
        }
    }

    fn build_server(&self, cx: &gpui_kit::App) -> Result<AgentMcpServer, String> {
        let id = match &self.mode {
            SheetMode::Form { editing_id } => editing_id.clone(),
            SheetMode::Json { .. } => String::new(),
        };
        let name = self.name_input.read(cx).value().trim().to_owned();
        if name.is_empty() {
            return Err(crate::ui::tr(cx).settings().agent_mcp_err_name().to_owned());
        }
        let transport = if self.is_http {
            let url = self.url_input.read(cx).value().trim().to_owned();
            let headers = parse_kv_lines(&self.headers_input.read(cx).value(), ':');
            McpTransportConfig::Http { url, headers }
        } else {
            let command = self.command_input.read(cx).value().trim().to_owned();
            let args: Vec<String> = self
                .args_input
                .read(cx)
                .value()
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect();
            let env = parse_kv_lines(&self.env_input.read(cx).value(), '=');
            let cwd_raw = self.cwd_input.read(cx).value().trim().to_owned();
            let cwd = (!cwd_raw.is_empty()).then_some(cwd_raw);
            McpTransportConfig::Stdio {
                command,
                args,
                env,
                env_allow: Vec::new(),
                cwd,
            }
        };
        Ok(AgentMcpServer {
            id,
            name,
            transport,
            enabled: self.enabled,
            trusted: self.trusted,
            updated_at_ms: 0,
        })
    }

    pub(super) fn run_test(&mut self, cx: &mut Context<Self>) {
        if self.testing {
            return;
        }
        let server = match self.build_server(cx) {
            Ok(s) => s,
            Err(e) => {
                self.error_msg = Some(e);
                cx.notify();
                return;
            }
        };
        self.testing = true;
        self.test_result = None;
        self.error_msg = None;
        cx.notify();
        let task = spawn_async_into(
            cx,
            move |state| async move { agent_mcp_server_test_impl(&state, server).await },
            |this, res, cx| {
                this.testing = false;
                match res {
                    Ok(r) => this.test_result = Some(r),
                    Err(err) => this.error_msg = Some(err.message().to_owned()),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn save_form(&mut self, confirmed: bool, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let server = match self.build_server(cx) {
            Ok(s) => s,
            Err(e) => {
                self.error_msg = Some(e);
                cx.notify();
                return;
            }
        };
        let timeout_raw = self.timeout_input.read(cx).value().trim().to_owned();
        let timeout_opt: Option<u32> = if timeout_raw.is_empty() {
            None
        } else {
            match timeout_raw.parse::<u32>() {
                Ok(v) if (5..=3600).contains(&v) => Some(v),
                _ => {
                    self.error_msg = Some(
                        crate::ui::tr(cx)
                            .settings()
                            .agent_mcp_err_timeout()
                            .to_owned(),
                    );
                    cx.notify();
                    return;
                }
            }
        };

        self.saving = true;
        self.error_msg = None;
        cx.notify();
        let parent = self.parent.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let saved = agent_mcp_server_save_impl(state, server, confirmed)?;
                let sid = saved.id.clone();
                let patch = match timeout_opt {
                    Some(secs) => {
                        serde_json::json!({ "agent": { "mcp_call_timeout_s": { sid: secs } } })
                    }
                    None => {
                        serde_json::json!({ "agent": { "mcp_call_timeout_s": { sid: null } } })
                    }
                };
                let _ = settings_update_impl(state, CoreSection::Ai, &patch);
                Ok::<AgentMcpServer, pingdeck_desktop::core::error::AppError>(saved)
            },
            move |this, res, cx| {
                this.saving = false;
                match res {
                    Ok(saved) => {
                        this.needs_stdio_confirm = false;
                        this.pending_close = true;
                        parent.update(cx, |p, cx| {
                            let msg = crate::ui::Tr(p.settings.general.language)
                                .settings()
                                .mcp_sheet_saved_mcp_server(saved.name);
                            p.flash = Some((Tone::Success, msg));
                            p.reload_all(cx);
                        });
                    }
                    Err(pingdeck_desktop::core::error::AppError::ConfirmationRequired {
                        ..
                    }) => {
                        this.needs_stdio_confirm = true;
                    }
                    Err(err) => {
                        this.error_msg = Some(err.message().to_owned());
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn import_json(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let raw = self.json_input.read(cx).value().to_string();
        let existing = match &self.mode {
            SheetMode::Json { existing } => existing.clone(),
            SheetMode::Form { .. } => return,
        };
        let parsed = match parse_mcp_servers_json(&raw, &existing, crate::ui::active_language(cx)) {
            Ok(list) => list,
            Err(msg) => {
                self.error_msg = Some(msg);
                cx.notify();
                return;
            }
        };
        if parsed.is_empty() {
            self.error_msg = Some(
                crate::ui::tr(cx)
                    .settings()
                    .agent_mcp_err_no_entries()
                    .to_owned(),
            );
            cx.notify();
            return;
        }
        self.saving = true;
        self.error_msg = None;
        cx.notify();
        let parent = self.parent.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let mut imported = 0usize;
                for srv in parsed {
                    agent_mcp_server_save_impl(state, srv, false)?;
                    imported += 1;
                }
                Ok::<usize, pingdeck_desktop::core::error::AppError>(imported)
            },
            move |this, res, cx| {
                this.saving = false;
                match res {
                    Ok(count) => {
                        this.pending_close = true;
                        parent.update(cx, |p, cx| {
                            let msg = crate::ui::Tr(p.settings.general.language)
                                .settings()
                                .mcp_sheet_imported_mcp_server_s_disabled(count);
                            p.flash = Some((Tone::Success, msg));
                            p.reload_all(cx);
                        });
                    }
                    Err(err) => {
                        this.error_msg = Some(err.message().to_owned());
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn pick_cwd(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let win_h = window.window_handle();
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Working directory".into()),
        });
        let task = cx.spawn(async move |weak, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(dir) = paths.into_iter().next()
            {
                let s = dir.to_string_lossy().into_owned();
                let _ = weak.update(cx, |this, cx| {
                    let _ = win_h.update(cx, |_, window, cx| {
                        this.cwd_input
                            .update(cx, |st, cx| st.set_value(s, window, cx));
                    });
                });
            }
        });
        self._tasks.push(task);
    }
}

impl Render for McpSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_close {
            self.pending_close = false;
            window.close_sheet(cx);
        }
        let p = palette(cx);

        match &self.mode {
            SheetMode::Json { .. } => render_json_mode(self, &p, cx).into_any_element(),
            SheetMode::Form { .. } => render_form_mode(self, &p, cx).into_any_element(),
        }
    }
}
