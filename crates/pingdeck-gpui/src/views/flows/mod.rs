//! Flow Orchestration module (`NavModule::Flows`).
//!
//! Directly wired to `flow_list_impl`, `flow_save_impl`, `flow_delete_impl`, and `flow_run_impl`
//! with real-time `FlowEvent` streaming.

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Task, Window, div, px,
};
use pingdeck_desktop::core::app::{
    FlowListEntryDto, current_time_ms, flow_delete_impl, flow_list_impl, flow_run_impl,
    flow_save_impl, settings_get_impl,
};
use pingdeck_desktop::core::flows::FlowEvent;
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::core::model::{Flow, WorkspaceId};
use pingdeck_desktop::streams::channel_sink;

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into, subscribe_stream_into};
use crate::ui::{
    InputState, ScrollableElement as _, Tone, UiIcon, badge, btn_danger, btn_primary, card,
    empty_state, h_row, list_row, palette, section_heading, text_input, to_hsla, v_col,
};

pub struct FlowsView {
    workspace_id: Option<String>,
    flows: Vec<FlowListEntryDto>,
    selected_id: Option<String>,
    new_flow_input: Entity<InputState>,
    running: bool,
    live_events: Vec<String>,
    status_msg: Option<(Tone, String)>,
    _tasks: Vec<Task<()>>,
}

impl FlowsView {
    pub fn new(workspace_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let new_flow_input = cx.new(|cx| InputState::new(window, cx).placeholder("New flow name…"));
        let mut view = Self {
            workspace_id: workspace_id.clone(),
            flows: Vec::new(),
            selected_id: None,
            new_flow_input,
            running: false,
            live_events: Vec::new(),
            status_msg: None,
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
        self.flows.clear();
        self.selected_id = None;
        self.live_events.clear();
        if let Some(ws) = workspace_id {
            self.reload(ws, cx);
        }
        cx.notify();
    }

    pub fn reload(&mut self, workspace_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| flow_list_impl(state, workspace_id),
            |this, res, cx| {
                if let Ok(list) = res {
                    if this
                        .selected_id
                        .as_deref()
                        .is_none_or(|id| !list.iter().any(|f| f.flow.id == id))
                    {
                        this.selected_id = list.first().map(|f| f.flow.id.clone());
                    }
                    this.flows = list;
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    fn create_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let name = self.new_flow_input.read(cx).value().trim().to_owned();
        if name.is_empty() {
            return;
        }
        self.new_flow_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        let Ok(workspace_id) = WorkspaceId::new(&ws_id) else {
            return;
        };
        let flow = Flow {
            id: format!("flow-{}", current_time_ms()),
            workspace_id,
            name,
            description: String::new(),
            steps: Vec::new(),
            updated_at_ms: current_time_ms(),
            options: Default::default(),
        };
        let task = spawn_blocking_into(
            cx,
            move |state| flow_save_impl(state, flow),
            move |this, res, cx| {
                match res {
                    Ok(saved) => {
                        this.selected_id = Some(saved.id.clone());
                        this.reload(ws_id, cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    fn run_selected(&mut self, cx: &mut Context<Self>) {
        let (Some(ws_id), Some(flow_id)) = (self.workspace_id.clone(), self.selected_id.clone())
        else {
            return;
        };
        self.running = true;
        self.live_events.clear();
        let (sink, rx) = channel_sink::<FlowEvent>();
        self._tasks
            .push(subscribe_stream_into(cx, rx, 16, |this, batch, cx| {
                for ev in batch {
                    this.live_events.push(format!("{ev:?}"));
                }
                cx.notify();
            }));
        let ws_reload = ws_id.clone();
        let task = spawn_async_into(
            cx,
            move |state| async move {
                flow_run_impl(
                    &state,
                    ws_id,
                    flow_id,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(true),
                    sink.as_ref(),
                )
                .await
            },
            move |this, res, cx| {
                this.running = false;
                match res {
                    Ok(summary) => {
                        this.status_msg = Some((
                            Tone::Success,
                            format!("Finished {} step(s)", summary.steps.len()),
                        ));
                        this.reload(ws_reload, cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
        cx.notify();
    }

    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        let (Some(ws_id), Some(flow_id)) = (self.workspace_id.clone(), self.selected_id.clone())
        else {
            return;
        };
        let ws_reload = ws_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| flow_delete_impl(state, ws_id, flow_id),
            move |this, res, cx| {
                if res.is_ok() {
                    this.selected_id = None;
                    this.reload(ws_reload, cx);
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}

impl Render for FlowsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let zh = settings_get_impl(runtime(cx).state()).general.language == Language::ZhCn;

        let Some(_ws) = self.workspace_id.as_ref() else {
            return empty_state(
                UiIcon::Workflow,
                if zh {
                    "未选择工作区"
                } else {
                    "No workspace selected"
                },
                if zh {
                    "选择工作区后可编排和运行多步骤 API 流程。"
                } else {
                    "Select a workspace to author and run multi-step API flows."
                },
                cx,
            );
        };

        let mut list = v_col()
            .flex_1()
            .gap(px(4.0))
            .p(px(10.0))
            .overflow_y_scrollbar();
        for entry in &self.flows {
            let id = entry.flow.id.clone();
            let sel = self.selected_id.as_deref() == Some(entry.flow.id.as_str());
            list = list.child(
                list_row(
                    SharedString::from(format!("flow-row-{id}")),
                    sel,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        this.selected_id = Some(id.clone());
                        cx.notify();
                    }),
                )
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .truncate()
                        .child(SharedString::from(entry.flow.name.clone())),
                )
                .child(badge(
                    format!("{} steps", entry.flow.steps.len()),
                    Tone::Neutral,
                    cx,
                )),
            );
        }

        let left = v_col()
            .w(px(260.0))
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
                    .child(section_heading(if zh { "流程列表" } else { "Flows" }, cx))
                    .child(
                        h_row()
                            .gap(px(4.0))
                            .child(div().flex_1().child(text_input(&self.new_flow_input)))
                            .child(btn_primary("flow-add", "+").on_click(
                                cx.listener(|this, _, window, cx| this.create_flow(window, cx)),
                            )),
                    ),
            )
            .child(list);

        let selected = self
            .selected_id
            .as_deref()
            .and_then(|id| self.flows.iter().find(|f| f.flow.id == id));

        let right = if let Some(entry) = selected {
            v_col()
                .size_full()
                .p(px(20.0))
                .gap(px(14.0))
                .overflow_y_scrollbar()
                .child(
                    h_row()
                        .justify_between()
                        .child(
                            div()
                                .text_base()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child(SharedString::from(entry.flow.name.clone())),
                        )
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .child(
                                    btn_primary(
                                        "flow-run",
                                        if self.running {
                                            if zh { "运行中…" } else { "Running…" }
                                        } else if zh {
                                            "运行流程"
                                        } else {
                                            "Run Flow"
                                        },
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| this.run_selected(cx))),
                                )
                                .child(
                                    btn_danger("flow-del", if zh { "删除" } else { "Delete" })
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.delete_selected(cx)),
                                        ),
                                ),
                        ),
                )
                .children(
                    self.status_msg
                        .clone()
                        .map(|(tone, msg)| badge(msg, tone, cx)),
                )
                .child(
                    card(cx).p(px(14.0)).child(
                        v_col()
                            .gap(px(6.0))
                            .child(section_heading(
                                format!(
                                    "{} ({})",
                                    if zh { "步骤" } else { "Steps" },
                                    entry.flow.steps.len()
                                ),
                                cx,
                            ))
                            .children(entry.flow.steps.iter().enumerate().map(|(i, s)| {
                                h_row()
                                    .gap(px(8.0))
                                    .py(px(4.0))
                                    .child(badge(format!("#{}", i + 1), Tone::Accent, cx))
                                    .child(
                                        div().text_xs().child(SharedString::from(s.name.clone())),
                                    )
                            })),
                    ),
                )
                .child(
                    card(cx).p(px(14.0)).child(
                        v_col()
                            .gap(px(4.0))
                            .child(section_heading(
                                if zh {
                                    "实时执行日志"
                                } else {
                                    "Live Run Trace"
                                },
                                cx,
                            ))
                            .children(self.live_events.iter().map(|line| {
                                div()
                                    .text_xs()
                                    .text_color(to_hsla(p.ink_2))
                                    .child(SharedString::from(line.clone()))
                            })),
                    ),
                )
                .into_any_element()
        } else {
            empty_state(
                UiIcon::Workflow,
                if zh {
                    "暂无流程"
                } else {
                    "No flow selected"
                },
                if zh {
                    "在左侧新建一个 API 编排流程，串联多个请求与变量提取。"
                } else {
                    "Create a flow on the left to chain multiple requests and variable extractions."
                },
                cx,
            )
            .into_any_element()
        };

        h_row()
            .size_full()
            .items_start()
            .bg(to_hsla(p.surface))
            .child(left)
            .child(div().flex_1().h_full().min_w_0().child(right))
    }
}
