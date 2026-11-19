//! Execution History module (`NavModule::History`).
//!
//! Backed directly by `history_page_impl` and `history_clear_filtered_impl`.

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Task, Window, div, px,
};
use pingdeck_desktop::core::app::{
    history_clear_filtered_impl, misc_logic_service::history_page_impl, settings_get_impl,
};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::core::model::{ExecutionHistoryEntry, HistoryFilter};

use crate::bridge::{runtime, spawn_blocking_into};
use crate::ui::{
    InputState, ScrollableElement as _, Tone, UiIcon, badge, btn_danger, btn_secondary, card,
    empty_state, h_row, list_row, method_badge, palette, section_heading, text_input, to_hsla,
    v_col,
};

pub struct HistoryView {
    workspace_id: Option<String>,
    entries: Vec<ExecutionHistoryEntry>,
    selected_id: Option<String>,
    search_input: Entity<InputState>,
    total_count: Option<u64>,
    _tasks: Vec<Task<()>>,
}

impl HistoryView {
    pub fn new(workspace_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter URL, method, status…"));
        let mut view = Self {
            workspace_id: workspace_id.clone(),
            entries: Vec::new(),
            selected_id: None,
            search_input,
            total_count: None,
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
        self.selected_id = None;
        if let Some(ws) = workspace_id {
            self.reload(ws, cx);
        }
        cx.notify();
    }

    pub fn reload(&mut self, workspace_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| {
                history_page_impl(
                    state,
                    workspace_id,
                    HistoryFilter::default(),
                    Some(100),
                    None,
                )
            },
            |this, res, cx| {
                if let Ok(page) = res {
                    this.total_count = page.total;
                    if this
                        .selected_id
                        .as_deref()
                        .is_none_or(|id| !page.entries.iter().any(|e| e.id.as_str() == id))
                    {
                        this.selected_id = page.entries.first().map(|e| e.id.as_str().to_owned());
                    }
                    this.entries = page.entries;
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    fn clear_history(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| {
                history_clear_filtered_impl(state, ws_id.clone(), HistoryFilter::default())
            },
            |this, res, cx| {
                if res.is_ok() {
                    this.entries.clear();
                    this.selected_id = None;
                    this.total_count = Some(0);
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}

impl Render for HistoryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let zh = settings_get_impl(runtime(cx).state()).general.language == Language::ZhCn;

        let Some(ws_id) = self.workspace_id.clone() else {
            return empty_state(
                UiIcon::History,
                if zh {
                    "未选择工作区"
                } else {
                    "No workspace selected"
                },
                if zh {
                    "选择一个工作区后即可查看请求与流程执行历史。"
                } else {
                    "Select a workspace to inspect request and flow execution history."
                },
                cx,
            );
        };

        let query = self.search_input.read(cx).value().trim().to_lowercase();
        let filtered: Vec<&ExecutionHistoryEntry> = self
            .entries
            .iter()
            .filter(|e| {
                query.is_empty()
                    || e.request_url.to_lowercase().contains(&query)
                    || e.request_name.to_lowercase().contains(&query)
                    || e.status_code.to_string().contains(&query)
            })
            .collect();

        let mut list = v_col()
            .flex_1()
            .gap(px(3.0))
            .p(px(8.0))
            .overflow_y_scrollbar();
        for entry in &filtered {
            let id = entry.id.as_str().to_owned();
            let selected = self.selected_id.as_deref() == Some(entry.id.as_str());
            let tone = match entry.status_code {
                200..=299 => Tone::Success,
                300..=399 => Tone::Accent,
                400..=499 => Tone::Warning,
                _ => Tone::Danger,
            };
            list = list.child(
                list_row(
                    SharedString::from(format!("hist-{id}")),
                    selected,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        this.selected_id = Some(id.clone());
                        cx.notify();
                    }),
                )
                .child(method_badge(&format!("{:?}", entry.method), cx))
                .child(
                    v_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .truncate()
                                .child(SharedString::from(entry.request_url.clone())),
                        )
                        .child(div().text_xs().text_color(to_hsla(p.muted)).child(format!(
                            "{} ms · {} B",
                            entry.elapsed_ms, entry.body_size_bytes
                        ))),
                )
                .child(badge(entry.status_code.to_string(), tone, cx)),
            );
        }

        let left = v_col()
            .w(px(360.0))
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
                    .child(
                        h_row()
                            .justify_between()
                            .child(section_heading(
                                format!(
                                    "{} ({})",
                                    if zh { "执行历史" } else { "History" },
                                    self.total_count.unwrap_or(self.entries.len() as u64)
                                ),
                                cx,
                            ))
                            .child(
                                h_row()
                                    .gap(px(4.0))
                                    .child(
                                        btn_secondary(
                                            "hist-refresh",
                                            if zh { "刷新" } else { "Refresh" },
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.reload(ws_id.clone(), cx);
                                            }),
                                        ),
                                    )
                                    .child(
                                        btn_danger("hist-clear", if zh { "清空" } else { "Clear" })
                                            .on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.clear_history(cx)
                                                }),
                                            ),
                                    ),
                            ),
                    )
                    .child(text_input(&self.search_input)),
            )
            .child(list);

        let selected = self
            .selected_id
            .as_deref()
            .and_then(|id| self.entries.iter().find(|e| e.id.as_str() == id));

        let right = if let Some(entry) = selected {
            let tone = match entry.status_code {
                200..=299 => Tone::Success,
                400..=499 => Tone::Warning,
                _ => Tone::Danger,
            };
            v_col()
                .flex_1()
                .h_full()
                .p(px(20.0))
                .gap(px(12.0))
                .overflow_y_scrollbar()
                .child(
                    h_row()
                        .gap(px(8.0))
                        .child(method_badge(&format!("{:?}", entry.method), cx))
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .truncate()
                                .child(SharedString::from(entry.request_url.clone())),
                        )
                        .child(badge(
                            format!("{} {}", entry.status_code, entry.status_text),
                            tone,
                            cx,
                        )),
                )
                .child(
                    h_row()
                        .gap(px(8.0))
                        .child(badge(format!("{} ms", entry.elapsed_ms), Tone::Neutral, cx))
                        .child(badge(
                            format!("{} B", entry.body_size_bytes),
                            Tone::Neutral,
                            cx,
                        ))
                        .child(badge(
                            SharedString::from(entry.executed_at.clone()),
                            Tone::Neutral,
                            cx,
                        )),
                )
                .child(
                    card(cx).p(px(12.0)).flex_1().child(
                        v_col()
                            .gap(px(6.0))
                            .child(section_heading(
                                if zh {
                                    "响应预览"
                                } else {
                                    "Response Preview"
                                },
                                cx,
                            ))
                            .child(div().text_xs().text_color(to_hsla(p.ink)).child(
                                SharedString::from(if entry.response_body_preview.is_empty() {
                                    entry
                                        .error_message
                                        .clone()
                                        .unwrap_or_else(|| "(empty body)".into())
                                } else {
                                    entry.response_body_preview.clone()
                                }),
                            )),
                    ),
                )
                .into_any_element()
        } else {
            empty_state(
                UiIcon::History,
                if zh {
                    "暂无历史记录"
                } else {
                    "No history recorded yet"
                },
                if zh {
                    "在「请求」或「流程」模块发送请求后，记录会自动出现在这里。"
                } else {
                    "Send a request or run a flow to record entries here."
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
