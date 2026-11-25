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

use std::cell::Cell;
use std::collections::HashMap;

use gpui_kit::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Window, div, px,
};
use pingdeck_desktop::core::app::settings_get_impl;
use pingdeck_desktop::core::model::items::FlowExtractionSource;

use crate::bridge::runtime;
use crate::ui::{
    Anchor, EditorState, InputEvent, InputState, StatusShape, Tone, UiIcon, badge, btn_danger,
    btn_ghost, btn_icon, btn_primary, btn_secondary, dismissible_banner, empty_state_with_actions,
    h_resizable, h_row, icon_xs, list_row, meta_pill, palette, popover, progress_bar,
    resizable_panel, select_pill, status_indicator, text_input, to_hsla, toolbar_strip, v_col,
    v_resizable,
};

use state::{FlowModal, FlowRunTab, FlowStepTab};
pub use state::{FlowsHandoff, FlowsView};

impl FlowsView {
    pub fn new(workspace_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let lang = settings_get_impl(runtime(cx).state()).general.language;
        let f_tr = crate::ui::tr(cx).flows();
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(f_tr.ph_filter_flows()));
        let flow_name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Flow name"));
        let flow_desc_input = cx.new(|cx| InputState::new(window, cx).placeholder(f_tr.ph_desc()));
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
            cx.new(|cx| InputState::new(window, cx).placeholder(f_tr.ph_search_requests()));
        let new_flow_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(f_tr.ph_new_flow_name()));

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
            inspector_open: true,
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
            new_flow_name_input,
            active_modal: None,
            status_msg: None,
            pending_sync: false,
            syncing: false,
            pending_handoff: Cell::new(None),
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

    #[must_use]
    pub fn take_pending_handoff(&self) -> Option<FlowsHandoff> {
        self.pending_handoff.take()
    }

    pub fn set_active_environment(&mut self, env_id: Option<String>, cx: &mut Context<Self>) {
        if self.selected_env_id != env_id {
            self.selected_env_id = env_id;
            cx.notify();
        }
    }
}

impl Render for FlowsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.lang = crate::ui::active_language(cx);
        if self.pending_sync {
            self.sync_inputs(window, cx);
        }
        let p = palette(cx);
        let left = sidebar::render_sidebar(self, cx);

        let right = if let Some(flow) = self.draft.clone() {
            let fid_del = flow.id.clone();
            let fname_del = flow.name.clone();

            let ent_env = cx.entity().clone();
            let envs_pop = self.environments.clone();
            let sel_env_id = self.selected_env_id.clone();
            let sel_env = sel_env_id
                .as_deref()
                .and_then(|id| envs_pop.iter().find(|e| e.id == id));
            let env_label = sel_env
                .map(|e| {
                    if e.confirm_before_send {
                        format!("● {} ({})", e.name, crate::ui::tr(cx).flows().flows_guard())
                    } else {
                        e.name.clone()
                    }
                })
                .unwrap_or_else(|| crate::ui::tr(cx).flows().flows_no_env().to_owned());

            let env_picker = popover("flow-env-pop")
                .anchor(Anchor::TopLeft)
                .trigger(select_pill(
                    "flow-env-trig",
                    SharedString::from(env_label),
                    sel_env_id.is_some(),
                    cx,
                ))
                .content(move |_, _, pop_cx| {
                    let mut col = v_col().w(px(230.0)).gap(px(2.0)).p(px(6.0));
                    let e0 = ent_env.clone();
                    let ph0 = pop_cx.entity().clone();
                    col = col.child(
                        list_row(
                            "fl-env-none",
                            sel_env_id.is_none(),
                            pop_cx,
                            move |_, w, cx| {
                                e0.update(cx, |this, cx| {
                                    this.selected_env_id = None;
                                    cx.notify();
                                });
                                ph0.update(cx, |s, cx| s.dismiss(w, cx));
                            },
                        )
                        .gap(px(6.0))
                        .child(status_indicator(StatusShape::Idle, pop_cx))
                        .child(
                            div()
                                .text_xs()
                                .child(crate::ui::tr(pop_cx).flows().flows_no_environment()),
                        ),
                    );
                    for env in &envs_pop {
                        let eid = env.id.clone();
                        let sel = sel_env_id.as_deref() == Some(env.id.as_str());
                        let ent_i = ent_env.clone();
                        let ph = pop_cx.entity().clone();
                        col = col.child(
                            list_row(
                                SharedString::from(format!("fl-env-{eid}")),
                                sel,
                                pop_cx,
                                move |_, w, cx| {
                                    ent_i.update(cx, |this, cx| {
                                        this.selected_env_id = Some(eid.clone());
                                        cx.notify();
                                    });
                                    ph.update(cx, |s, cx| s.dismiss(w, cx));
                                },
                            )
                            .justify_between()
                            .child(
                                h_row()
                                    .gap(px(6.0))
                                    .child(status_indicator(
                                        if env.confirm_before_send {
                                            StatusShape::Warn
                                        } else {
                                            StatusShape::Ok
                                        },
                                        pop_cx,
                                    ))
                                    .child(div().text_xs().child(env.name.clone())),
                            )
                            .children(env.confirm_before_send.then(|| {
                                badge(
                                    crate::ui::tr(pop_cx).flows().flows_guard_2(),
                                    Tone::Warning,
                                    pop_cx,
                                )
                            })),
                        );
                    }
                    col
                });

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
                        .child(env_picker)
                        .child(
                            btn_ghost(
                                "flow-insp-tog",
                                if self.inspector_open {
                                    crate::ui::Tr(self.lang).flows().flows_hide_inspector()
                                } else {
                                    crate::ui::Tr(self.lang).flows().flows_inspector()
                                },
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.inspector_open = !this.inspector_open;
                                cx.notify();
                            })),
                        )
                        .child(
                            btn_secondary(
                                "flow-opt-btn",
                                crate::ui::Tr(self.lang).flows().flows_options(),
                            )
                            .icon(icon_xs(UiIcon::Sliders))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.active_modal = Some(FlowModal::Options);
                                cx.notify();
                            })),
                        )
                        .child(if self.dirty {
                            btn_primary("flow-save-btn", crate::ui::Tr(self.lang).common().save())
                                .icon(icon_xs(UiIcon::Check))
                                .on_click(cx.listener(|this, _, _, cx| this.save_flow(cx)))
                        } else {
                            btn_secondary(
                                "flow-save-btn",
                                crate::ui::Tr(self.lang).common().save_saved(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.save_flow(cx)))
                        })
                        .child(if self.running {
                            btn_danger(
                                "flow-run-btn",
                                crate::ui::Tr(self.lang).flows().flows_stop(),
                            )
                            .icon(icon_xs(UiIcon::Stop))
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_run(cx)))
                        } else {
                            btn_primary(
                                "flow-run-btn",
                                crate::ui::Tr(self.lang).flows().flows_run_flow(),
                            )
                            .icon(icon_xs(UiIcon::Play))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.run_flow(None, None, None, cx);
                            }))
                        })
                        .child(
                            btn_icon("flow-dup-btn", UiIcon::Copy)
                                .tooltip(crate::ui::Tr(self.lang).flows().head_duplicate())
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.duplicate_selected(cx)),
                                ),
                        )
                        .child(
                            btn_icon("flow-del-btn", UiIcon::Trash)
                                .tooltip(crate::ui::Tr(self.lang).flows().head_delete())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.active_modal = Some(FlowModal::DeleteFlow {
                                        id: fid_del.clone(),
                                        name: fname_del.clone(),
                                    });
                                    cx.notify();
                                })),
                        ),
                );

            let editor_split = h_row()
                .size_full()
                .min_h_0()
                .child(steps_pane::render_steps_pane(self, cx))
                .children(self.inspector_open.then(|| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(inspector::render_step_inspector(self, cx))
                }));

            let mut detail = v_col().size_full().min_h_0().child(top_bar);
            if self.running {
                let pct = if self.live_total > 0 {
                    (self.live_steps.len() as f32 / self.live_total as f32) * 100.0
                } else {
                    10.0
                };
                detail = detail.child(
                    v_col()
                        .px(px(14.0))
                        .py(px(6.0))
                        .gap(px(4.0))
                        .bg(to_hsla(p.pane))
                        .border_b_1()
                        .border_color(to_hsla(p.line.composite_over(p.pane)))
                        .child(
                            h_row()
                                .justify_between()
                                .child(
                                    div().text_xs().text_color(to_hsla(p.ink)).child(
                                        SharedString::from(
                                            crate::ui::tr(cx)
                                                .flows()
                                                .flows_running_flow_steps_complete(
                                                    self.live_steps.len(),
                                                    self.live_total,
                                                ),
                                        ),
                                    ),
                                )
                                .child(meta_pill("HistoryOnly", cx)),
                        )
                        .child(progress_bar("flow-live-prog", pct)),
                );
            }
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
            detail
                .child(
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
                .child(
                    h_row()
                        .flex_shrink_0()
                        .justify_between()
                        .px(px(14.0))
                        .py(px(4.0))
                        .bg(to_hsla(p.pane))
                        .border_t_1()
                        .border_color(to_hsla(p.line.composite_over(p.pane)))
                        .text_size(px(10.5))
                        .text_color(to_hsla(p.faint))
                        .child(
                            crate::ui::Tr(self.lang)
                                .flows()
                                .flows_persistence_historyonly_writes_history_doe(),
                        ),
                )
        } else {
            v_col().size_full().child(empty_state_with_actions(
                UiIcon::Workflow,
                crate::ui::Tr(self.lang).flows().flows_no_flow_selected(),
                crate::ui::Tr(self.lang)
                    .flows()
                    .flows_create_multi_step_api_flows(),
                Some(
                    btn_primary(
                        "flow-empty-new",
                        crate::ui::Tr(self.lang).flows().flows_new_flow(),
                    )
                    .icon(icon_xs(UiIcon::Plus))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_create_flow_modal(window, cx);
                    }))
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
