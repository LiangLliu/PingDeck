//! Local Mock Server module (`NavModule::Mock`).
//!
//! Directly wired to `mock_server_list_impl`, `mock_server_save_impl`, `mock_server_start_impl`,
//! `mock_server_stop_impl`, `mock_server_status_list_impl`, and live `MockLogEvent` streams.

use std::collections::HashMap;

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Task, Window, div, px,
};
use pingdeck_desktop::core::app::{
    mock_port_suggest_impl, mock_server_delete_impl, mock_server_list_impl,
    mock_server_logs_subscribe_impl, mock_server_save_impl, mock_server_start_impl,
    mock_server_status_list_impl, mock_server_stop_impl, settings_get_impl,
};
use pingdeck_desktop::core::model::WorkspaceId;
use pingdeck_desktop::core::model::events::MockLogEvent;
use pingdeck_desktop::core::model::items::{MockServer, MockTransactionLog};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::streams::channel_sink;

use crate::bridge::{runtime, spawn_blocking_into, subscribe_stream_into};
use crate::ui::{
    InputState, ScrollableElement as _, Tone, UiIcon, badge, btn_danger, btn_primary,
    btn_secondary, card, empty_state, h_row, list_row, method_badge, palette, section_heading,
    status_dot, text_input, to_hsla, v_col,
};

pub struct MockView {
    workspace_id: Option<String>,
    servers: Vec<MockServer>,
    running_ports: HashMap<String, u16>,
    selected_id: Option<String>,
    new_server_input: Entity<InputState>,
    live_logs: Vec<MockTransactionLog>,
    log_stream_task: Option<Task<()>>,
    status_msg: Option<(Tone, String)>,
    _tasks: Vec<Task<()>>,
}

impl MockView {
    pub fn new(workspace_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let new_server_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Mock server name…"));
        let mut view = Self {
            workspace_id: workspace_id.clone(),
            servers: Vec::new(),
            running_ports: HashMap::new(),
            selected_id: None,
            new_server_input,
            live_logs: Vec::new(),
            log_stream_task: None,
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
        self.servers.clear();
        self.running_ports.clear();
        self.selected_id = None;
        self.live_logs.clear();
        self.log_stream_task = None;
        if let Some(ws) = workspace_id {
            self.reload(ws, cx);
        }
        cx.notify();
    }

    pub fn reload(&mut self, workspace_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let servers = mock_server_list_impl(state, workspace_id.clone())?;
                let statuses = mock_server_status_list_impl(state, workspace_id)?;
                Ok::<_, pingdeck_desktop::core::error::AppError>((servers, statuses))
            },
            |this, res, cx| {
                if let Ok((servers, statuses)) = res {
                    this.running_ports = statuses
                        .into_iter()
                        .filter(|s| s.running)
                        .map(|s| (s.server_id, s.port))
                        .collect();
                    if this
                        .selected_id
                        .as_deref()
                        .is_none_or(|id| !servers.iter().any(|s| s.id == id))
                    {
                        this.selected_id = servers.first().map(|s| s.id.clone());
                    }
                    this.servers = servers;
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    fn create_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let name = self.new_server_input.read(cx).value().trim().to_owned();
        if name.is_empty() {
            return;
        }
        self.new_server_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let port = mock_port_suggest_impl(state, ws_id.clone(), 3100).unwrap_or(3100);
                let Ok(workspace_id) = WorkspaceId::new(&ws_id) else {
                    return Err(pingdeck_desktop::core::error::AppError::validation(
                        "invalid workspace id",
                    ));
                };
                let server = MockServer {
                    id: format!("mock-{}", pingdeck_desktop::core::app::current_time_ms()),
                    workspace_id,
                    name,
                    port,
                    endpoint_prefix: String::new(),
                    latency_ms: 0,
                    cors_enabled: true,
                    headers: Vec::new(),
                    routes: Vec::new(),
                    data_buckets: Vec::new(),
                    callbacks: Vec::new(),
                    proxy_enabled: false,
                    proxy_base_url: String::new(),
                    proxy_remove_prefix: false,
                    proxy_req_headers: Vec::new(),
                    proxy_res_headers: Vec::new(),
                    updated_at_ms: pingdeck_desktop::core::app::current_time_ms(),
                    environment_id: None,
                };
                mock_server_save_impl(state, server)
            },
            |this, res, cx| {
                match res {
                    Ok(saved) => {
                        this.selected_id = Some(saved.id.clone());
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

    fn toggle_start_stop(&mut self, server_id: String, is_running: bool, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let ws_for_sub = ws_id.clone();
        let sid_for_sub = server_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                if is_running {
                    mock_server_stop_impl(state, ws_id, server_id)
                } else {
                    mock_server_start_impl(state, ws_id, server_id)
                }
            },
            move |this, res, cx| {
                match res {
                    Ok(status) => {
                        if status.running {
                            this.running_ports
                                .insert(status.server_id.clone(), status.port);
                            this.subscribe_logs(ws_for_sub, sid_for_sub, cx);
                        } else {
                            this.running_ports.remove(&status.server_id);
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

    fn subscribe_logs(&mut self, ws_id: String, server_id: String, cx: &mut Context<Self>) {
        let (sink, rx) = channel_sink::<MockLogEvent>();
        if mock_server_logs_subscribe_impl(runtime(cx).state(), ws_id, server_id, sink).is_ok() {
            self.log_stream_task = Some(subscribe_stream_into(cx, rx, 32, |this, batches, cx| {
                for ev in batches {
                    if let MockLogEvent::Batch { entries } = ev {
                        this.live_logs.extend(entries);
                        if this.live_logs.len() > 200 {
                            let excess = this.live_logs.len() - 200;
                            this.live_logs.drain(..excess);
                        }
                    }
                }
                cx.notify();
            }));
        }
    }

    fn delete_server(&mut self, server_id: String, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| mock_server_delete_impl(state, ws_id, server_id),
            |this, res, cx| {
                if res.is_ok() {
                    this.selected_id = None;
                    if let Some(ws) = this.workspace_id.clone() {
                        this.reload(ws, cx);
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}

impl Render for MockView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let zh = settings_get_impl(runtime(cx).state()).general.language == Language::ZhCn;

        let Some(_ws) = self.workspace_id.as_ref() else {
            return empty_state(
                UiIcon::Server,
                if zh {
                    "未选择工作区"
                } else {
                    "No workspace selected"
                },
                if zh {
                    "选择或创建工作区后可管理本地 Mock 服务与实时请求日志。"
                } else {
                    "Select a workspace to manage local Mock servers and live request logs."
                },
                cx,
            );
        };

        let mut list = v_col()
            .flex_1()
            .gap(px(4.0))
            .p(px(10.0))
            .overflow_y_scrollbar();
        for srv in &self.servers {
            let id = srv.id.clone();
            let running = self.running_ports.contains_key(&srv.id);
            let selected = self.selected_id.as_deref() == Some(srv.id.as_str());
            list = list.child(
                list_row(
                    SharedString::from(format!("mock-srv-{id}")),
                    selected,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        this.selected_id = Some(id.clone());
                        cx.notify();
                    }),
                )
                .justify_between()
                .child(
                    h_row()
                        .gap(px(8.0))
                        .min_w_0()
                        .child(status_dot(running, cx))
                        .child(
                            div()
                                .text_xs()
                                .truncate()
                                .child(SharedString::from(srv.name.clone())),
                        ),
                )
                .child(badge(format!(":{}", srv.port), Tone::Neutral, cx)),
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
                    .child(section_heading(
                        if zh { "Mock 服务" } else { "Mock Servers" },
                        cx,
                    ))
                    .child(
                        h_row()
                            .gap(px(4.0))
                            .child(div().flex_1().child(text_input(&self.new_server_input)))
                            .child(btn_primary("mock-create", "+").on_click(
                                cx.listener(|this, _, window, cx| this.create_server(window, cx)),
                            )),
                    ),
            )
            .child(list);

        let selected = self
            .selected_id
            .as_deref()
            .and_then(|id| self.servers.iter().find(|s| s.id == id))
            .cloned();

        let right = if let Some(srv) = selected {
            let running = self.running_ports.contains_key(&srv.id);
            let sid = srv.id.clone();
            let sid_del = srv.id.clone();
            let mut routes_box = v_col().gap(px(4.0));
            if srv.routes.is_empty() {
                routes_box =
                    routes_box.child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                        "暂无路由规则"
                    } else {
                        "No routes configured yet"
                    }));
            } else {
                for route in &srv.routes {
                    routes_box = routes_box.child(
                        h_row()
                            .gap(px(8.0))
                            .px(px(10.0))
                            .py(px(6.0))
                            .rounded(px(6.0))
                            .bg(to_hsla(p.surface_2))
                            .child(method_badge(&route.method, cx))
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .child(SharedString::from(route.path.clone())),
                            )
                            .child(badge(
                                if route.enabled { "Active" } else { "Off" },
                                if route.enabled {
                                    Tone::Success
                                } else {
                                    Tone::Neutral
                                },
                                cx,
                            )),
                    );
                }
            }

            let mut logs_box = v_col().gap(px(3.0));
            for log in self.live_logs.iter().rev().take(30) {
                logs_box = logs_box.child(
                    h_row()
                        .gap(px(8.0))
                        .px(px(8.0))
                        .py(px(4.0))
                        .rounded(px(5.0))
                        .bg(to_hsla(p.pane))
                        .child(method_badge(&log.method, cx))
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .truncate()
                                .child(SharedString::from(log.path.clone())),
                        )
                        .child(badge(log.status_code.to_string(), Tone::Neutral, cx)),
                );
            }

            v_col()
                .flex_1()
                .h_full()
                .p(px(20.0))
                .gap(px(14.0))
                .overflow_y_scrollbar()
                .child(
                    h_row()
                        .justify_between()
                        .child(
                            h_row()
                                .gap(px(8.0))
                                .child(status_dot(running, cx))
                                .child(
                                    div()
                                        .text_base()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .child(SharedString::from(srv.name.clone())),
                                )
                                .child(badge(format!("127.0.0.1:{}", srv.port), Tone::Accent, cx)),
                        )
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .child(
                                    if running {
                                        btn_secondary("mock-stop", if zh { "停止" } else { "Stop" })
                                    } else {
                                        btn_primary("mock-start", if zh { "启动" } else { "Start" })
                                    }
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.toggle_start_stop(sid.clone(), running, cx);
                                        },
                                    )),
                                )
                                .child(
                                    btn_danger("mock-del", if zh { "删除" } else { "Delete" })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.delete_server(sid_del.clone(), cx);
                                        })),
                                ),
                        ),
                )
                .child(
                    card(cx).p(px(12.0)).child(
                        v_col()
                            .gap(px(8.0))
                            .child(section_heading(
                                format!(
                                    "{} ({})",
                                    if zh { "路由规则" } else { "Routes" },
                                    srv.routes.len()
                                ),
                                cx,
                            ))
                            .child(routes_box),
                    ),
                )
                .child(
                    card(cx).p(px(12.0)).child(
                        v_col()
                            .gap(px(8.0))
                            .child(section_heading(
                                format!(
                                    "{} ({})",
                                    if zh {
                                        "实时请求日志"
                                    } else {
                                        "Live Request Logs"
                                    },
                                    self.live_logs.len()
                                ),
                                cx,
                            ))
                            .child(logs_box),
                    ),
                )
                .into_any_element()
        } else {
            empty_state(
                UiIcon::Server,
                if zh {
                    "暂无 Mock 服务"
                } else {
                    "No Mock server selected"
                },
                if zh {
                    "在左侧新建一个本地 Mock 服务并一键启动。"
                } else {
                    "Create a local Mock server on the left and start it with one click."
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
