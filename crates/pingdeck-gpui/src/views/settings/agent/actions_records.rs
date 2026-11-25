//! Record-level actions for `AgentSettingsView`: trusted folders, tool policies,
//! permission rules, safety rules, shell env passthrough, MCP servers, and skills.

use gpui_kit::{
    AppContext as _, Context, ParentElement as _, PathPromptOptions, SharedString, Window, px,
};
use pingdeck_desktop::core::agent::model::AgentMcpServer;
use pingdeck_desktop::core::agent::permissions::PermissionAction;
use pingdeck_desktop::core::app::{
    agent_folder_trust_revoke_impl, agent_folder_trust_set_impl, agent_mcp_server_delete_impl,
    agent_mcp_server_log_impl, agent_mcp_server_restart_impl, agent_mcp_server_save_impl,
    agent_mcp_tool_review_impl, agent_permission_rule_delete_impl, agent_skill_import_impl,
    agent_skill_remove_impl, agent_skill_set_enabled_impl, agent_tool_policy_set_impl,
    reveal_path_impl,
};

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::{Tone, WindowExt as _};

use super::AgentSettingsView;
use super::mcp_sheet::McpSheet;

impl AgentSettingsView {
    // ── Trusted Folders (Memory) ─────────────────────────────────────────────

    pub(super) fn pick_and_trust_folder(&mut self, cx: &mut Context<Self>) {
        let zh = self.zh();
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(SharedString::from(if zh {
                "选择要信任的项目文件夹"
            } else {
                "Choose a project folder to trust"
            })),
        });
        let task = cx.spawn(async move |weak, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(dir) = paths.into_iter().next()
            {
                let path_str = dir.to_string_lossy().into_owned();
                let _ = weak.update(cx, |this, cx| this.trust_folder(path_str, cx));
            }
        });
        self._tasks.push(task);
    }

    pub(super) fn trust_folder(&mut self, path: String, cx: &mut Context<Self>) {
        let zh = self.zh();
        let display = path.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_folder_trust_set_impl(state, &path, true),
            move |this, res, cx| match res {
                Ok(_) => {
                    this.flash = Some((
                        Tone::Success,
                        if zh {
                            format!("已信任 {display}")
                        } else {
                            format!("Trusted {display}")
                        },
                    ));
                    this.reload_all(cx);
                }
                Err(err) => {
                    this.flash = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn revoke_folder_trust(&mut self, path: String, cx: &mut Context<Self>) {
        let zh = self.zh();
        let display = path.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_folder_trust_revoke_impl(state, &path),
            move |this, res, cx| match res {
                Ok(_) => {
                    this.flash = Some((
                        Tone::Warning,
                        if zh {
                            format!("已撤销对 {display} 的信任")
                        } else {
                            format!("Revoked trust for {display}")
                        },
                    ));
                    this.reload_all(cx);
                }
                Err(err) => {
                    this.flash = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    // ── Tool Policies ────────────────────────────────────────────────────────

    pub(super) fn set_tool_policy(
        &mut self,
        tool_key: String,
        action: Option<PermissionAction>,
        cx: &mut Context<Self>,
    ) {
        let task = spawn_blocking_into(
            cx,
            move |state| agent_tool_policy_set_impl(state, &tool_key, action),
            |this, res, cx| match res {
                Ok(_) => this.reload_all(cx),
                Err(err) => {
                    this.flash = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    // ── Permissions, Safety Rules & Shell Env ────────────────────────────────

    pub(super) fn delete_permission_rule(&mut self, rule_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| agent_permission_rule_delete_impl(state, &rule_id),
            |this, res, cx| match res {
                Ok(_) => this.reload_all(cx),
                Err(err) => {
                    this.flash = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn add_blocked_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pat = self.blocked_cmd_input.read(cx).value().trim().to_owned();
        if pat.is_empty() {
            return;
        }
        self.blocked_cmd_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        let mut list = self.settings.ai.agent.blocked_commands.clone();
        if !list.contains(&pat) {
            list.push(pat);
            self.patch_ai(
                serde_json::json!({ "agent": { "blocked_commands": list } }),
                None,
                cx,
            );
        }
    }

    pub(super) fn remove_blocked_command(&mut self, pattern: &str, cx: &mut Context<Self>) {
        let list: Vec<String> = self
            .settings
            .ai
            .agent
            .blocked_commands
            .iter()
            .filter(|p| p.as_str() != pattern)
            .cloned()
            .collect();
        self.patch_ai(
            serde_json::json!({ "agent": { "blocked_commands": list } }),
            None,
            cx,
        );
    }

    pub(super) fn add_shell_env(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.env_pass_input.read(cx).value().trim().to_owned();
        if name.is_empty() {
            return;
        }
        self.env_pass_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        let mut list = self.settings.ai.agent.shell_env_passthrough.clone();
        if !list.contains(&name) {
            list.push(name);
            self.patch_ai(
                serde_json::json!({ "agent": { "shell_env_passthrough": list } }),
                None,
                cx,
            );
        }
    }

    pub(super) fn remove_shell_env(&mut self, name: &str, cx: &mut Context<Self>) {
        let list: Vec<String> = self
            .settings
            .ai
            .agent
            .shell_env_passthrough
            .iter()
            .filter(|n| n.as_str() != name)
            .cloned()
            .collect();
        self.patch_ai(
            serde_json::json!({ "agent": { "shell_env_passthrough": list } }),
            None,
            cx,
        );
    }

    // ── MCP Servers ──────────────────────────────────────────────────────────

    pub(super) fn open_mcp_add_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let parent = cx.entity().clone();
        let sheet_ent = cx.new(|cx| McpSheet::for_new(parent, window, cx));
        Self::present_mcp_sheet(sheet_ent, window, cx);
    }

    pub(super) fn open_mcp_edit_sheet(
        &mut self,
        server: AgentMcpServer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let parent = cx.entity().clone();
        let timeout_s = self
            .settings
            .ai
            .agent
            .mcp_call_timeout_s
            .get(&server.id)
            .copied();
        let sheet_ent = cx.new(|cx| McpSheet::for_edit(parent, server, timeout_s, window, cx));
        Self::present_mcp_sheet(sheet_ent, window, cx);
    }

    pub(super) fn open_mcp_json_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let parent = cx.entity().clone();
        let existing: Vec<AgentMcpServer> = self
            .config
            .as_ref()
            .map(|c| c.mcp_servers.iter().map(|v| v.server.clone()).collect())
            .unwrap_or_default();
        let sheet_ent = cx.new(|cx| McpSheet::for_json(parent, existing, window, cx));
        Self::present_mcp_sheet(sheet_ent, window, cx);
    }

    fn present_mcp_sheet(
        drawer: gpui_kit::Entity<McpSheet>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.open_sheet(cx, move |sheet, _, cx| {
            let title = drawer.read(cx).sheet_title(cx);
            sheet
                .size(px(560.0))
                .resizable(true)
                .title(title)
                .child(drawer.clone())
        });
    }

    pub(super) fn toggle_mcp_enabled(
        &mut self,
        mut server: AgentMcpServer,
        enabled: bool,
        confirmed: bool,
        cx: &mut Context<Self>,
    ) {
        server.enabled = enabled;
        let sid = server.id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_mcp_server_save_impl(state, server, confirmed),
            move |this, res, cx| match res {
                Ok(_) => {
                    this.mcp_confirm_start_id = None;
                    this.reload_all(cx);
                }
                Err(pingdeck_desktop::core::error::AppError::ConfirmationRequired { .. }) => {
                    this.mcp_confirm_start_id = Some(sid);
                    cx.notify();
                }
                Err(err) => {
                    this.flash = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn review_mcp_server(&mut self, server_id: String, cx: &mut Context<Self>) {
        let zh = self.zh();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_mcp_tool_review_impl(state, &server_id, &[]),
            move |this, res, cx| match res {
                Ok(v) => {
                    this.flash = Some((
                        Tone::Success,
                        if zh {
                            format!("已更新 {} 的信任状态", v.server.name)
                        } else {
                            format!("Updated trust for {}", v.server.name)
                        },
                    ));
                    this.reload_all(cx);
                }
                Err(err) => {
                    this.flash = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn restart_mcp_server(&mut self, server_id: String, cx: &mut Context<Self>) {
        self.mcp_restarting_id = Some(server_id.clone());
        cx.notify();
        let task = spawn_async_into(
            cx,
            move |state| async move { agent_mcp_server_restart_impl(&state, &server_id).await },
            |this, res, cx| {
                this.mcp_restarting_id = None;
                if let Err(err) = res {
                    this.flash = Some((Tone::Danger, err.message().to_owned()));
                }
                this.reload_all(cx);
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn delete_mcp_server(&mut self, server_id: String, cx: &mut Context<Self>) {
        let zh = self.zh();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_mcp_server_delete_impl(state, &server_id),
            move |this, res, cx| {
                this.mcp_confirm_delete_id = None;
                match res {
                    Ok(_) => {
                        this.flash = Some((
                            Tone::Warning,
                            if zh {
                                "已删除 MCP 服务器"
                            } else {
                                "MCP server deleted"
                            }
                            .to_owned(),
                        ));
                        this.reload_all(cx);
                    }
                    Err(err) => {
                        this.flash = Some((Tone::Danger, err.message().to_owned()));
                        cx.notify();
                    }
                }
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn toggle_mcp_log(&mut self, server_id: String, cx: &mut Context<Self>) {
        if self.mcp_log_open_id.as_deref() == Some(server_id.as_str()) {
            self.mcp_log_open_id = None;
            cx.notify();
            return;
        }
        self.mcp_log_open_id = Some(server_id.clone());
        self.refresh_mcp_log(server_id, cx);
    }

    pub(super) fn refresh_mcp_log(&mut self, server_id: String, cx: &mut Context<Self>) {
        let sid = server_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_mcp_server_log_impl(state, &server_id),
            move |this, res, cx| match res {
                Ok(lines) => {
                    this.mcp_logs.insert(sid, lines);
                    cx.notify();
                }
                Err(err) => {
                    this.flash = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    // ── Skills ───────────────────────────────────────────────────────────────

    pub(super) fn rescan_skills(&mut self, cx: &mut Context<Self>) {
        let zh = self.zh();
        let task = spawn_blocking_into(
            cx,
            |state| {
                let entries = pingdeck_desktop::core::app::agent_skill_entries_impl(state, None)?;
                Ok::<_, pingdeck_desktop::core::error::AppError>(entries)
            },
            move |this, res, cx| match res {
                Ok(entries) => {
                    let count = entries.len();
                    this.skill_entries = entries;
                    this.flash = Some((
                        Tone::Success,
                        if zh {
                            format!("扫描完成：找到 {count} 个技能")
                        } else {
                            format!("Scan finished: {count} skill(s) found")
                        },
                    ));
                    this.reload_all(cx);
                }
                Err(err) => {
                    this.flash = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn pick_and_add_skill_dir(&mut self, cx: &mut Context<Self>) {
        let zh = self.zh();
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(SharedString::from(if zh {
                "选择技能文件夹"
            } else {
                "Choose a skills folder"
            })),
        });
        let task = cx.spawn(async move |weak, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(dir) = paths.into_iter().next()
            {
                let path_str = dir.to_string_lossy().into_owned();
                let _ = weak.update(cx, |this, cx| {
                    let mut dirs = this.settings.ai.agent.skill_dirs.clone();
                    if !dirs.contains(&path_str) {
                        dirs.push(path_str);
                        this.patch_ai(
                            serde_json::json!({ "agent": { "skill_dirs": dirs } }),
                            None,
                            cx,
                        );
                    }
                });
            }
        });
        self._tasks.push(task);
    }

    pub(super) fn remove_skill_dir(&mut self, path: &str, cx: &mut Context<Self>) {
        let dirs: Vec<String> = self
            .settings
            .ai
            .agent
            .skill_dirs
            .iter()
            .filter(|d| d.as_str() != path)
            .cloned()
            .collect();
        self.patch_ai(
            serde_json::json!({ "agent": { "skill_dirs": dirs } }),
            None,
            cx,
        );
    }

    pub(super) fn reveal_folder(&mut self, path: String, cx: &mut Context<Self>) {
        let expanded = pingdeck_desktop::core::agent::skills::expand_home(&path);
        let abs = expanded.to_string_lossy().into_owned();
        let task = spawn_blocking_into(
            cx,
            move |state| reveal_path_impl(state, &abs),
            |this, res, cx| {
                if let Err(err) = res {
                    this.flash = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn pick_and_import_skill(&mut self, cx: &mut Context<Self>) {
        let zh = self.zh();
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(SharedString::from(if zh {
                "选择包含 SKILL.md 的文件夹"
            } else {
                "Choose a folder containing SKILL.md"
            })),
        });
        let task = cx.spawn(async move |weak, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(dir) = paths.into_iter().next()
            {
                let path_str = dir.to_string_lossy().into_owned();
                let _ = weak.update(cx, |this, cx| {
                    let task = spawn_blocking_into(
                        cx,
                        move |state| agent_skill_import_impl(state, &path_str),
                        move |this, res, cx| match res {
                            Ok(sk) => {
                                this.flash = Some((
                                    Tone::Success,
                                    if zh {
                                        format!("已导入技能 {}", sk.name)
                                    } else {
                                        format!("Imported skill {}", sk.name)
                                    },
                                ));
                                this.reload_all(cx);
                            }
                            Err(err) => {
                                this.flash = Some((Tone::Danger, err.message().to_owned()));
                                cx.notify();
                            }
                        },
                    );
                    this._tasks.push(task);
                });
            }
        });
        self._tasks.push(task);
    }

    pub(super) fn toggle_skill(&mut self, name: String, enabled: bool, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| agent_skill_set_enabled_impl(state, &name, enabled, None),
            |this, res, cx| match res {
                Ok(()) => this.reload_all(cx),
                Err(err) => {
                    this.flash = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn remove_imported_skill(&mut self, name: String, cx: &mut Context<Self>) {
        let zh = self.zh();
        let label = name.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_skill_remove_impl(state, &name),
            move |this, res, cx| match res {
                Ok(()) => {
                    this.flash = Some((
                        Tone::Warning,
                        if zh {
                            format!("已移除导入技能 {label}")
                        } else {
                            format!("Removed imported skill {label}")
                        },
                    ));
                    this.reload_all(cx);
                }
                Err(err) => {
                    this.flash = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }
}
