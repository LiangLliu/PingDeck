//! Complete Flows orchestration module (`NavModule::Flows`).
//!
//! Multi-step API pipeline builder & runner backed by `pingdeck_core::app::flow_service`:
//! - Master sidebar with search, step counts, and last-run pass/fail/cancelled badges
//! - Ordered step pipeline with inline or saved-request steps, reorder, duplicate, partial run
//!   ("Run up to step" / "Run only this step"), and live streaming `FlowEvent` progress
//! - Step inspector for Request Overrides, Extractions, Assertions, Condition/Retries, and Output
//! - Execution console showing live run progress, masked captured variables, and past 20 runs.

pub mod actions;
pub mod inspector;
pub mod inspector_exec;
pub mod modals;
pub mod run_panel;
pub mod sidebar;
pub mod state;
pub mod steps_pane;

use std::collections::HashMap;

use gpui_kit::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Window, div, px,
};
use pingdeck_desktop::core::app::settings_get_impl;
use pingdeck_desktop::core::model::items::FlowExtractionSource;

use crate::bridge::runtime;
use crate::ui::{
    EditorState, InputEvent, InputState, Tone, UiIcon, btn_danger, btn_icon, btn_primary,
    btn_secondary, choice_chip, dismissible_banner, empty_state_with_actions, h_resizable, h_row,
    icon_xs, palette, resizable_panel, text_input, to_hsla, toolbar_strip, v_col, v_resizable,
};

pub use state::FlowsView;
use state::{FlowModal, FlowRunTab, FlowStepTab};

impl FlowsView {
    pub fn new(workspace_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let lang = settings_get_impl(runtime(cx).state()).general.language;
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("筛选流程 / Filter…"));
        let flow_name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Flow name"));
        let flow_desc_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("流程描述 / Description…"));
        let step_name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Step name"));
        let step_url_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://api.example.com/..."));
        let step_body_editor = cx.new(|cx| {
            let mut ed = EditorState::new(window, cx)
                .soft_wrap(true)
                .placeholder("Optional body override (JSON / text)…");
            ed.set_highlighter("json", cx);
            ed
        });
        let step_delay_input = cx.new(|cx| InputState::new(window, cx).placeholder("0"));
        let step_timeout_input = cx.new(|cx| InputState::new(window, cx).placeholder("inherit"));
        let step_cond_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. {{status}} == active"));
        let step_retries_input = cx.new(|cx| InputState::new(window, cx).placeholder("0"));
        let step_retry_wait_input = cx.new(|cx| InputState::new(window, cx).placeholder("500"));
        let kv_key_input = cx.new(|cx| InputState::new(window, cx).placeholder("Header name"));
        let kv_val_input = cx.new(|cx| InputState::new(window, cx).placeholder("Header value"));
        let ext_var_input = cx.new(|cx| InputState::new(window, cx).placeholder("var_name"));
        let ext_expr_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("$.data.token / Header name"));
        let assert_path_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Header key / JSONPath"));
        let assert_val_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Expected value (e.g. 200)"));
        let seed_key_input = cx.new(|cx| InputState::new(window, cx).placeholder("Variable key"));
        let seed_val_input = cx.new(|cx| InputState::new(window, cx).placeholder("Seed value"));
        let req_picker_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索已保存请求…"));

        let mut subs = Vec::new();
        for inp in [
            &flow_name_input,
            &flow_desc_input,
            &step_name_input,
            &step_url_input,
            &step_delay_input,
            &step_timeout_input,
            &step_cond_input,
            &step_retries_input,
            &step_retry_wait_input,
        ] {
            subs.push(cx.subscribe(inp, |this, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) && !this.syncing {
                    this.dirty = true;
                    cx.notify();
                }
            }));
        }
        subs.push(
            cx.subscribe(&step_body_editor, |this, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) && !this.syncing {
                    this.dirty = true;
                    cx.notify();
                }
            }),
        );
        for filter_inp in [&search_input, &req_picker_search] {
            subs.push(cx.subscribe(filter_inp, |_, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify();
                }
            }));
        }

        let mut view = Self {
            workspace_id: workspace_id.clone(),
            lang,
            entries: Vec::new(),
            requests: Vec::new(),
            environments: Vec::new(),
            selected_flow_id: None,
            draft: None,
            dirty: false,
            selected_step_id: None,
            selected_env_id: None,
            step_tab: FlowStepTab::Request,
            run_tab: FlowRunTab::LiveRun,
            running: false,
            cancel_token: None,
            live_total: 0,
            live_steps: HashMap::new(),
            last_result: None,
            past_runs: Vec::new(),
            selected_past_run_id: None,
            new_ext_source: FlowExtractionSource::BodyJsonPath,
            new_ext_secret: false,
            new_assert_kind_idx: 0,
            search_input,
            flow_name_input,
            flow_desc_input,
            step_name_input,
            step_url_input,
            step_body_editor,
            step_delay_input,
            step_timeout_input,
            step_cond_input,
            step_retries_input,
            step_retry_wait_input,
            kv_key_input,
            kv_val_input,
            ext_var_input,
            ext_expr_input,
            assert_val_input,
            assert_path_input,
            seed_key_input,
            seed_val_input,
            req_picker_search,
            active_modal: None,
            status_msg: None,
            pending_sync: false,
            syncing: false,
            _subscriptions: subs,
            _tasks: Vec::new(),
        };
        if let Some(ws) = workspace_id {
            view.reload(ws, cx);
        }
        view
    }

    pub fn set_workspace(&mut self, workspace_id: Option<String>, cx: &mut Context<Self>) {
        if self.workspace_id == workspace_id {
            return;
        }
        self.workspace_id = workspace_id.clone();
        self.entries.clear();
        self.requests.clear();
        self.environments.clear();
        self.selected_flow_id = None;
        self.draft = None;
        self.dirty = false;
        self.selected_step_id = None;
        self.selected_env_id = None;
        self.live_steps.clear();
        self.last_result = None;
        self.past_runs.clear();
        self.active_modal = None;
        if let Some(ws) = workspace_id {
            self.reload(ws, cx);
        }
        cx.notify();
    }

    pub fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        self.lang = settings_get_impl(runtime(cx).state()).general.language;
        if let Some(ws) = self.workspace_id.clone() {
            self.reload(ws, cx);
        }
    }

    pub fn dismiss_modal_if_open(&mut self, cx: &mut Context<Self>) -> bool {
        if self.active_modal.take().is_some() {
            cx.notify();
            true
        } else {
            false
        }
    }
}

impl Render for FlowsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_sync {
            self.sync_inputs(window, cx);
        }
        let p = palette(cx);
        let left = sidebar::render_sidebar(self, cx);

        let right = if let Some(flow) = self.draft.clone() {
            let fid_del = flow.id.clone();
            let fname_del = flow.name.clone();

            let mut env_bar = h_row().gap(px(4.0)).flex_wrap().child(
                choice_chip(
                    "flow-env-none",
                    self.t("无环境", "No Env"),
                    self.selected_env_id.is_none(),
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.selected_env_id = None;
                    cx.notify();
                })),
            );
            for env in &self.environments {
                let eid = env.id.clone();
                let active = self.selected_env_id.as_deref() == Some(env.id.as_str());
                env_bar = env_bar.child(
                    choice_chip(
                        SharedString::from(format!("flow-env-{}", env.id)),
                        env.name.clone(),
                        active,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_env_id = Some(eid.clone());
                        cx.notify();
                    })),
                );
            }

            let top_bar = toolbar_strip(cx)
                .justify_between()
                .child(
                    h_row()
                        .flex_1()
                        .min_w_0()
                        .gap(px(8.0))
                        .child(div().w(px(210.0)).child(text_input(&self.flow_name_input)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(text_input(&self.flow_desc_input)),
                        ),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .flex_shrink_0()
                        .child(
                            btn_secondary("flow-opt-btn", self.t("选项", "Options"))
                                .icon(icon_xs(UiIcon::Sliders))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.active_modal = Some(FlowModal::Options);
                                    cx.notify();
                                })),
                        )
                        .child(if self.dirty {
                            btn_primary("flow-save-btn", self.t("保存", "Save"))
                                .icon(icon_xs(UiIcon::Check))
                                .on_click(cx.listener(|this, _, _, cx| this.save_flow(cx)))
                        } else {
                            btn_secondary("flow-save-btn", self.t("已保存", "Saved"))
                                .on_click(cx.listener(|this, _, _, cx| this.save_flow(cx)))
                        })
                        .child(if self.running {
                            btn_danger("flow-run-btn", self.t("停止", "Stop"))
                                .icon(icon_xs(UiIcon::Stop))
                                .on_click(cx.listener(|this, _, _, cx| this.cancel_run(cx)))
                        } else {
                            btn_primary("flow-run-btn", self.t("运行流程", "Run Flow"))
                                .icon(icon_xs(UiIcon::Play))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.run_flow(None, None, None, cx);
                                }))
                        })
                        .child(
                            btn_icon("flow-dup-btn", UiIcon::Copy)
                                .tooltip(self.t("复制流程", "Duplicate flow"))
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.duplicate_selected(cx)),
                                ),
                        )
                        .child(
                            btn_icon("flow-del-btn", UiIcon::Trash)
                                .tooltip(self.t("删除流程", "Delete flow"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.active_modal = Some(FlowModal::DeleteFlow {
                                        id: fid_del.clone(),
                                        name: fname_del.clone(),
                                    });
                                    cx.notify();
                                })),
                        ),
                );

            let env_strip = h_row()
                .w_full()
                .flex_shrink_0()
                .gap(px(8.0))
                .px(px(14.0))
                .py(px(6.0))
                .bg(to_hsla(p.pane))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(self.t("运行环境:", "Environment:")),
                )
                .child(env_bar);

            let editor_split = h_row()
                .size_full()
                .min_h_0()
                .child(steps_pane::render_steps_pane(self, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(inspector::render_step_inspector(self, cx)),
                );

            let mut detail = v_col()
                .size_full()
                .min_h_0()
                .child(top_bar)
                .child(env_strip);
            if let Some((tone, msg)) = &self.status_msg {
                detail = detail.child(div().px(px(12.0)).pt(px(8.0)).child(dismissible_banner(
                    "flow-banner-close",
                    *tone,
                    msg.clone(),
                    cx.listener(|this, _, _, cx| {
                        this.status_msg = None;
                        cx.notify();
                    }),
                    cx,
                )));
            }
            detail.child(
                div().flex_1().min_h_0().w_full().child(
                    v_resizable("flow-vert-split")
                        .child(resizable_panel().child(editor_split))
                        .child(
                            resizable_panel()
                                .size(px(210.0))
                                .size_range(px(120.0)..px(420.0))
                                .child(run_panel::render_run_panel(self, cx)),
                        ),
                ),
            )
        } else {
            v_col().size_full().child(empty_state_with_actions(
                UiIcon::Workflow,
                self.t("还没有流程", "No Flow Selected"),
                self.t(
                    "创建多步骤接口编排流程，自动串联登录鉴权、参数提取、条件跳过、失败重试与断言验证。",
                    "Create multi-step API flows with variable chaining, conditions, retries, and assertions.",
                ),
                Some(
                    btn_primary("flow-empty-new", self.t("新建流程", "New Flow"))
                        .icon(icon_xs(UiIcon::Plus))
                        .on_click(cx.listener(|this, _, _, cx| this.create_flow(cx)))
                        .into_any_element(),
                ),
                cx,
            ))
        };

        let mut root = div().relative().size_full().bg(to_hsla(p.surface)).child(
            h_resizable("flow-main-split")
                .child(
                    resizable_panel()
                        .size(px(250.0))
                        .size_range(px(200.0)..px(360.0))
                        .child(left),
                )
                .child(resizable_panel().child(right)),
        );
        if let Some(modal) = modals::render_modal(self, window, cx) {
            root = root.child(modal);
        }
        let _ = Tone::Neutral;
        root
    }
}
