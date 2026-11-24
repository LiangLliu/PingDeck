//! Environments & secrets module (`NavModule::Environments`).
//!
//! Directly backed by `environment_list_impl`, `environment_save_impl`, `environment_delete_impl`,
//! `environment_secret_clipboard_set` (via `pingdeck_desktop::services::set_secret_text`), and
//! `environment_declared_hosts_impl`.

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Task, Window, div, px,
};
use pingdeck_desktop::core::app::{
    EnvironmentDto, EnvironmentVariableInput, SaveEnvironmentInput, environment_delete_impl,
    environment_list_impl, environment_save_impl, settings_get_impl,
};
use pingdeck_desktop::core::model::settings::Language;

use crate::bridge::{runtime, spawn_blocking_into};
use crate::ui::{
    InputState, ScrollableElement as _, Tone, UiIcon, badge, btn_danger, btn_icon, btn_primary,
    btn_secondary, card, empty_state, field_row, h_row, list_row, palette, section_heading,
    text_input, to_hsla, toggle_switch, v_col,
};

pub struct EnvironmentsView {
    workspace_id: Option<String>,
    environments: Vec<EnvironmentDto>,
    selected_id: Option<String>,
    new_env_input: Entity<InputState>,
    new_var_key: Entity<InputState>,
    new_var_val: Entity<InputState>,
    new_var_secret: bool,
    status_msg: Option<(Tone, String)>,
    _tasks: Vec<Task<()>>,
}

impl EnvironmentsView {
    pub fn new(workspace_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let new_env_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Environment name…"));
        let new_var_key = cx.new(|cx| InputState::new(window, cx).placeholder("KEY_NAME"));
        let new_var_val = cx.new(|cx| InputState::new(window, cx).placeholder("value"));
        let mut view = Self {
            workspace_id: workspace_id.clone(),
            environments: Vec::new(),
            selected_id: None,
            new_env_input,
            new_var_key,
            new_var_val,
            new_var_secret: false,
            status_msg: None,
            _tasks: Vec::new(),
        };
        if let Some(ws_id) = workspace_id {
            view.reload(ws_id, cx);
        }
        view
    }

    pub fn set_workspace(&mut self, workspace_id: Option<String>, cx: &mut Context<Self>) {
        if self.workspace_id == workspace_id {
            return;
        }
        self.workspace_id = workspace_id.clone();
        self.environments.clear();
        self.selected_id = None;
        if let Some(ws_id) = workspace_id {
            self.reload(ws_id, cx);
        }
        cx.notify();
    }

    pub fn reload(&mut self, workspace_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| environment_list_impl(state, workspace_id),
            |this, res, cx| {
                match res {
                    Ok(list) => {
                        if this
                            .selected_id
                            .as_deref()
                            .is_none_or(|id| !list.iter().any(|e| e.id == id))
                        {
                            this.selected_id = list.first().map(|e| e.id.clone());
                        }
                        this.environments = list;
                    }
                    Err(err) => {
                        this.status_msg = Some((Tone::Danger, err.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    fn create_environment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let name = self.new_env_input.read(cx).value().trim().to_owned();
        if name.is_empty() {
            return;
        }
        self.new_env_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        let id = format!("env-{}", pingdeck_desktop::core::app::current_time_ms());
        let input = SaveEnvironmentInput {
            id: id.clone(),
            workspace_id: ws_id.clone(),
            name,
            variables: Vec::new(),
            confirm_before_send: Some(false),
            confirm_safe_methods: Some(false),
            declared_hosts: None,
            base_revision: None,
        };
        let task = spawn_blocking_into(
            cx,
            move |state| environment_save_impl(state, input),
            move |this, res, cx| {
                match res {
                    Ok(saved) => {
                        this.selected_id = Some(saved.id.clone());
                        this.status_msg = Some((Tone::Success, format!("Saved {}", saved.name)));
                        this.reload(ws_id, cx);
                    }
                    Err(err) => {
                        this.status_msg = Some((Tone::Danger, err.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    fn add_variable(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(env) = self.selected_env().cloned() else {
            return;
        };
        let key = self.new_var_key.read(cx).value().trim().to_owned();
        let val = self.new_var_val.read(cx).value().to_string();
        if key.is_empty() {
            return;
        }
        let mut vars: Vec<EnvironmentVariableInput> = env
            .variables
            .iter()
            .map(|v| EnvironmentVariableInput {
                id: Some(v.id.clone()),
                key: v.key.clone(),
                value: v.value.clone(),
                enabled: v.enabled,
                is_secret: v.is_secret,
                note: v.note.clone(),
            })
            .collect();
        vars.push(EnvironmentVariableInput {
            id: None,
            key,
            value: Some(val),
            enabled: true,
            is_secret: self.new_var_secret,
            note: String::new(),
        });
        self.new_var_key
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.new_var_val
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.save_env(
            SaveEnvironmentInput {
                id: env.id,
                workspace_id: env.workspace_id,
                name: env.name,
                variables: vars,
                confirm_before_send: Some(env.confirm_before_send),
                confirm_safe_methods: Some(env.confirm_safe_methods),
                declared_hosts: Some(env.declared_hosts),
                base_revision: Some(env.revision),
            },
            cx,
        );
    }

    fn save_env(&mut self, input: SaveEnvironmentInput, cx: &mut Context<Self>) {
        let ws_id = input.workspace_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| environment_save_impl(state, input),
            move |this, res, cx| {
                match res {
                    Ok(saved) => {
                        this.status_msg = Some((Tone::Success, format!("Updated {}", saved.name)));
                        this.reload(ws_id, cx);
                    }
                    Err(err) => {
                        this.status_msg = Some((Tone::Danger, err.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        let Some(env) = self.selected_env().cloned() else {
            return;
        };
        let ws_id = env.workspace_id.clone();
        let env_id = env.id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| environment_delete_impl(state, ws_id.clone(), env_id),
            move |this, res, cx| {
                match res {
                    Ok(_) => {
                        this.selected_id = None;
                        this.status_msg = Some((Tone::Neutral, "Environment deleted".into()));
                        if let Some(ws) = this.workspace_id.clone() {
                            this.reload(ws, cx);
                        }
                    }
                    Err(err) => {
                        this.status_msg = Some((Tone::Danger, err.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    fn selected_env(&self) -> Option<&EnvironmentDto> {
        let id = self.selected_id.as_deref()?;
        self.environments.iter().find(|e| e.id == id)
    }
}

impl Render for EnvironmentsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let zh = settings_get_impl(runtime(cx).state()).general.language == Language::ZhCn;

        let Some(_ws_id) = self.workspace_id.as_ref() else {
            return empty_state(
                UiIcon::Layers,
                if zh {
                    "未选择工作区"
                } else {
                    "No workspace selected"
                },
                if zh {
                    "先在左侧选择或创建一个工作区以管理环境变量与密钥。"
                } else {
                    "Select or create a workspace in the sidebar to manage environments and secrets."
                },
                cx,
            );
        };

        let mut list = v_col()
            .gap(px(4.0))
            .p(px(10.0))
            .flex_1()
            .overflow_y_scrollbar();
        for env in &self.environments {
            let id = env.id.clone();
            let selected = self.selected_id.as_deref() == Some(env.id.as_str());
            let name = env.name.clone();
            let count = env.variables.len();
            list = list.child(
                list_row(
                    SharedString::from(format!("env-row-{id}")),
                    selected,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        this.selected_id = Some(id.clone());
                        cx.notify();
                    }),
                )
                .justify_between()
                .child(div().text_xs().truncate().child(SharedString::from(name)))
                .child(badge(count.to_string(), Tone::Neutral, cx)),
            );
        }

        let left_pane = v_col()
            .w(px(250.0))
            .h_full()
            .bg(to_hsla(p.pane))
            .border_r_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)))
            .child(
                v_col()
                    .p(px(10.0))
                    .gap(px(6.0))
                    .border_b_1()
                    .border_color(to_hsla(p.line.composite_over(p.pane)))
                    .child(section_heading(
                        if zh { "环境列表" } else { "Environments" },
                        cx,
                    ))
                    .child(
                        h_row()
                            .gap(px(4.0))
                            .child(div().flex_1().child(text_input(&self.new_env_input)))
                            .child(btn_primary("env-add", "+").on_click(cx.listener(
                                |this, _, window, cx| this.create_environment(window, cx),
                            ))),
                    ),
            )
            .child(list);

        let right_pane = if let Some(env) = self.selected_env().cloned() {
            let mut vars_list = v_col().gap(px(4.0));
            for (idx, var) in env.variables.iter().enumerate() {
                let env_for_del = env.clone();
                let key = var.key.clone();
                let is_secret = var.is_secret;
                let shown = if is_secret {
                    "••••••••".to_owned()
                } else {
                    var.value.clone().unwrap_or_default()
                };
                vars_list = vars_list.child(
                    h_row()
                        .gap(px(8.0))
                        .px(px(10.0))
                        .py(px(6.0))
                        .rounded(px(6.0))
                        .bg(to_hsla(p.surface_2))
                        .child(
                            div()
                                .w(px(180.0))
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(SharedString::from(key)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(to_hsla(p.ink_2))
                                .truncate()
                                .child(SharedString::from(shown)),
                        )
                        .child(if is_secret {
                            badge(if zh { "密钥" } else { "Secret" }, Tone::Warning, cx)
                        } else {
                            badge(if zh { "明文" } else { "Plain" }, Tone::Neutral, cx)
                        })
                        .child(btn_icon(("env-var-del", idx), UiIcon::Trash).on_click(
                            cx.listener(move |this, _, _, cx| {
                                let mut next_vars: Vec<EnvironmentVariableInput> = env_for_del
                                    .variables
                                    .iter()
                                    .enumerate()
                                    .filter(|(i, _)| *i != idx)
                                    .map(|(_, v)| EnvironmentVariableInput {
                                        id: Some(v.id.clone()),
                                        key: v.key.clone(),
                                        value: v.value.clone(),
                                        enabled: v.enabled,
                                        is_secret: v.is_secret,
                                        note: v.note.clone(),
                                    })
                                    .collect();
                                next_vars.shrink_to_fit();
                                this.save_env(
                                    SaveEnvironmentInput {
                                        id: env_for_del.id.clone(),
                                        workspace_id: env_for_del.workspace_id.clone(),
                                        name: env_for_del.name.clone(),
                                        variables: next_vars,
                                        confirm_before_send: Some(env_for_del.confirm_before_send),
                                        confirm_safe_methods: Some(
                                            env_for_del.confirm_safe_methods,
                                        ),
                                        declared_hosts: Some(env_for_del.declared_hosts.clone()),
                                        base_revision: Some(env_for_del.revision.clone()),
                                    },
                                    cx,
                                );
                            }),
                        )),
                );
            }

            let guard_env = env.clone();
            v_col()
                .size_full()
                .p(px(20.0))
                .gap(px(14.0))
                .overflow_y_scrollbar()
                .child(
                    h_row()
                        .justify_between()
                        .child(
                            h_row()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .text_base()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .child(SharedString::from(env.name.clone())),
                                )
                                .child(badge(
                                    format!("{} vars", env.variables.len()),
                                    Tone::Accent,
                                    cx,
                                )),
                        )
                        .child(
                            btn_danger("env-delete", if zh { "删除环境" } else { "Delete" })
                                .on_click(cx.listener(|this, _, _, cx| this.delete_selected(cx))),
                        ),
                )
                .child(
                    card(cx).p(px(12.0)).child(field_row(
                        if zh {
                            "发送前确认（生产环境保护）"
                        } else {
                            "Confirm before send (production guard)"
                        },
                        Some(SharedString::from(if zh {
                            "开启后，向此环境发送写请求前会要求二次确认。"
                        } else {
                            "Requires explicit confirmation before sending write requests in this environment."
                        })),
                        toggle_switch("env-guard-toggle", env.confirm_before_send).on_change(
                            cx.listener(move |this, checked: &bool, _, cx| {
                                let vars = guard_env
                                    .variables
                                    .iter()
                                    .map(|v| EnvironmentVariableInput {
                                        id: Some(v.id.clone()),
                                        key: v.key.clone(),
                                        value: v.value.clone(),
                                        enabled: v.enabled,
                                        is_secret: v.is_secret,
                                        note: v.note.clone(),
                                    })
                                    .collect();
                                this.save_env(
                                    SaveEnvironmentInput {
                                        id: guard_env.id.clone(),
                                        workspace_id: guard_env.workspace_id.clone(),
                                        name: guard_env.name.clone(),
                                        variables: vars,
                                        confirm_before_send: Some(*checked),
                                        confirm_safe_methods: Some(guard_env.confirm_safe_methods),
                                        declared_hosts: Some(guard_env.declared_hosts.clone()),
                                        base_revision: Some(guard_env.revision.clone()),
                                    },
                                    cx,
                                );
                            }),
                        ),
                        cx,
                    )),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(div().w(px(180.0)).child(text_input(&self.new_var_key)))
                        .child(div().flex_1().child(text_input(&self.new_var_val)))
                        .child(
                            btn_secondary(
                                "env-var-secret-toggle",
                                if self.new_var_secret {
                                    if zh { "密钥: 开" } else { "Secret: ON" }
                                } else if zh {
                                    "密钥: 关"
                                } else {
                                    "Secret: OFF"
                                },
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.new_var_secret = !this.new_var_secret;
                                cx.notify();
                            })),
                        )
                        .child(
                            btn_primary("env-var-add", if zh { "添加变量" } else { "Add" })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.add_variable(window, cx);
                                })),
                        ),
                )
                .child(vars_list)
                .into_any_element()
        } else {
            empty_state(
                UiIcon::Key,
                if zh { "还没有环境" } else { "No environment selected" },
                if zh {
                    "在左侧输入名称创建一个环境，密钥会存放在独立 0600 权限的 .secrets 明文库中。"
                } else {
                    "Create an environment on the left. Secrets are stored in the adjacent 0600 plaintext .secrets vault."
                },
                cx,
            )
            .into_any_element()
        };

        h_row()
            .size_full()
            .items_start()
            .bg(to_hsla(p.surface))
            .child(left_pane)
            .child(div().flex_1().h_full().min_w_0().child(right_pane))
    }
}
