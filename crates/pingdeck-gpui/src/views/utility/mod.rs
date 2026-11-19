//! Global Utility modal hosting all 6 tools wired directly to `pingdeck-core`:
//! Import/Export (cURL), Folder Sync, Cookies, TLS Certificates, Console (4-point native GPU
//! perf/IME bench), and Collection Runner.

pub mod bench;
mod tools;

use std::cell::Cell;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Task, Window, div, px,
};
use pingdeck_desktop::core::app::{
    CollectionDto, RunCollectionInput, TlsCertificateDto, certificate_delete_impl,
    certificate_list_impl, collection_list_impl, cookie_jar_delete_expired_impl,
    cookie_jar_list_impl, request_curl_import_impl, run_collection_impl, settings_get_impl,
    sync_export_impl, sync_import_impl, sync_status_impl,
};
use pingdeck_desktop::core::model::CookieJarEntry;
use pingdeck_desktop::core::model::events::RunnerEvent;
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::core::sync::{SyncImportOptions, SyncStatus};
use pingdeck_desktop::state::UtilityDialog;
use pingdeck_desktop::streams::channel_sink;

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into, subscribe_stream_into};
use crate::ui::{
    TextareaState, Tone, UiIcon, badge, btn_icon, btn_primary, card, h_row, list_row,
    multiline_input, palette, section_heading, to_hsla, v_col,
};

use bench::{BenchTab, NativeBenchState};

pub struct UtilityPanel {
    active: UtilityDialog,
    workspace_id: Option<String>,
    dismissed: Cell<bool>,
    pub bench: NativeBenchState,
    curl_input: Entity<TextareaState>,
    pub(super) sync_status: Option<SyncStatus>,
    pub(super) cookies: Vec<CookieJarEntry>,
    pub(super) certificates: Vec<TlsCertificateDto>,
    pub(super) collections: Vec<CollectionDto>,
    pub(super) runner_events: Vec<String>,
    pub(super) running_collection: bool,
    status_note: Option<(Tone, String)>,
    _tasks: Vec<Task<()>>,
}

impl UtilityPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let bench = NativeBenchState::new(window, cx);
        let curl_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("curl -X POST https://api.example.com/v1/chat -H 'Content-Type: application/json' -d '{}'")
                .rows(6)
        });

        Self {
            active: UtilityDialog::Console,
            workspace_id: None,
            dismissed: Cell::new(false),
            bench,
            curl_input,
            sync_status: None,
            cookies: Vec::new(),
            certificates: Vec::new(),
            collections: Vec::new(),
            runner_events: Vec::new(),
            running_collection: false,
            status_note: None,
            _tasks: Vec::new(),
        }
    }

    pub fn open(
        &mut self,
        dialog: UtilityDialog,
        workspace_id: Option<String>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active = dialog;
        self.workspace_id = workspace_id;
        self.dismissed.set(false);
        self.reload_active(cx);
        cx.notify();
    }

    pub fn is_dismissed(&self) -> bool {
        self.dismissed.get()
    }

    fn reload_active(&mut self, cx: &mut Context<Self>) {
        let ws = self.workspace_id.clone();
        match self.active {
            UtilityDialog::Sync => {
                let task = spawn_blocking_into(cx, sync_status_impl, |this, status, cx| {
                    this.sync_status = Some(status);
                    cx.notify();
                });
                self._tasks.push(task);
            }
            UtilityDialog::Cookies => {
                if let Some(ws_id) = ws {
                    let task = spawn_blocking_into(
                        cx,
                        move |state| cookie_jar_list_impl(state, ws_id).unwrap_or_default(),
                        |this, cookies, cx| {
                            this.cookies = cookies;
                            cx.notify();
                        },
                    );
                    self._tasks.push(task);
                }
            }
            UtilityDialog::Certificates => {
                if let Some(ws_id) = ws {
                    let task = spawn_blocking_into(
                        cx,
                        move |state| certificate_list_impl(state, ws_id).unwrap_or_default(),
                        |this, certs, cx| {
                            this.certificates = certs;
                            cx.notify();
                        },
                    );
                    self._tasks.push(task);
                }
            }
            UtilityDialog::Runner => {
                if let Some(ws_id) = ws {
                    let task = spawn_blocking_into(
                        cx,
                        move |state| collection_list_impl(state, ws_id).unwrap_or_default(),
                        |this, cols, cx| {
                            this.collections = cols;
                            cx.notify();
                        },
                    );
                    self._tasks.push(task);
                }
            }
            UtilityDialog::ImportExport | UtilityDialog::Console => {}
        }
    }

    pub(super) fn run_sync_export(&mut self, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            |state| sync_export_impl(state, None),
            |this, res, cx| {
                match res {
                    Ok(r) => {
                        this.status_note =
                            Some((Tone::Success, format!("Exported {} files", r.files_written)));
                        this.reload_active(cx);
                    }
                    Err(err) => this.status_note = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn run_sync_import(&mut self, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            |state| sync_import_impl(state, None, &SyncImportOptions::default()),
            |this, res, cx| {
                match res {
                    Ok(r) => {
                        this.status_note = Some((
                            Tone::Success,
                            format!("Imported {} entities", r.added + r.updated),
                        ));
                        this.reload_active(cx);
                    }
                    Err(err) => this.status_note = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn clear_expired_cookies(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| cookie_jar_delete_expired_impl(state, ws_id),
            |this, res, cx| {
                if let Ok(deleted) = res {
                    this.status_note = Some((
                        Tone::Success,
                        format!("Removed {} expired cookies", deleted.len()),
                    ));
                    this.reload_active(cx);
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn delete_cert(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| certificate_delete_impl(state, ws_id, id),
            |this, res, cx| {
                if res.is_ok() {
                    this.reload_active(cx);
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn run_collection(&mut self, collection_id: String, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        self.running_collection = true;
        self.runner_events.clear();
        let (sink, rx) = channel_sink::<RunnerEvent>();
        self._tasks
            .push(subscribe_stream_into(cx, rx, 16, |this, batch, cx| {
                for ev in batch {
                    this.runner_events.push(format!("{ev:?}"));
                }
                cx.notify();
            }));
        let input = RunCollectionInput {
            workspace_id: ws_id,
            collection_id,
            environment_id: None,
            iterations: Some(1),
            delay_ms: Some(0),
            stop_on_error: Some(false),
            data: None,
            confirmed: Some(true),
            folder_id: None,
            request_ids: None,
        };
        self._tasks.push(spawn_async_into(
            cx,
            move |state| async move { run_collection_impl(&state, input, None, sink.as_ref()).await },
            |this, res, cx| {
                this.running_collection = false;
                match res {
                    Ok(r) => {
                        this.status_note = Some((
                            Tone::Success,
                            format!("Runner finished: {}/{} passed", r.passed, r.total),
                        ));
                    }
                    Err(err) => this.status_note = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        ));
        cx.notify();
    }
}

impl Render for UtilityPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let zh = settings_get_impl(runtime(cx).state()).general.language == Language::ZhCn;

        let mut nav = v_col()
            .w(px(190.0))
            .h_full()
            .p(px(10.0))
            .gap(px(4.0))
            .bg(to_hsla(p.pane))
            .border_r_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)));
        for util in UtilityDialog::ALL {
            let (zl, el) = util.label_pair();
            nav = nav.child(
                list_row(
                    SharedString::from(format!("util-tab-{el}")),
                    self.active == util,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        this.active = util;
                        this.reload_active(cx);
                        cx.notify();
                    }),
                )
                .child(div().text_xs().child(if zh { zl } else { el })),
            );
        }

        let content: gpui_kit::AnyElement = match self.active {
            UtilityDialog::Console => self
                .bench
                .render(
                    cx,
                    |this, tab: BenchTab, cx| {
                        this.bench.tab = tab;
                        cx.notify();
                    },
                    |this, cx| {
                        this.bench.append_stream_burst();
                        cx.notify();
                    },
                    |this, window, cx| {
                        this.bench.load_10mib_json(window, cx);
                        cx.notify();
                    },
                )
                .into_any_element(),
            UtilityDialog::ImportExport => v_col()
                .size_full()
                .gap(px(10.0))
                .child(section_heading(
                    if zh {
                        "粘贴 cURL 快速导入"
                    } else {
                        "Paste cURL to import"
                    },
                    cx,
                ))
                .child(multiline_input(&self.curl_input))
                .child(
                    h_row().gap(px(8.0)).child(
                        btn_primary(
                            "util-curl-parse",
                            if zh { "解析 cURL" } else { "Parse cURL" },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let raw = this.curl_input.read(cx).value().to_string();
                            let ws = this
                                .workspace_id
                                .clone()
                                .unwrap_or_else(|| "default".into());
                            match request_curl_import_impl(ws, raw) {
                                Ok(req) => {
                                    this.status_note = Some((
                                        Tone::Success,
                                        format!("Parsed: {:?} {}", req.method, req.url),
                                    ));
                                }
                                Err(err) => {
                                    this.status_note =
                                        Some((Tone::Danger, err.message().to_owned()));
                                }
                            }
                            cx.notify();
                        })),
                    ),
                )
                .into_any_element(),
            UtilityDialog::Sync => tools::render_sync(self, zh, cx),
            UtilityDialog::Cookies => tools::render_cookies(self, zh, cx),
            UtilityDialog::Certificates => tools::render_certs(self, zh, cx),
            UtilityDialog::Runner => tools::render_runner(self, zh, cx),
        };

        let (zl, el) = self.active.label_pair();
        card(cx)
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                h_row()
                    .justify_between()
                    .px(px(16.0))
                    .py(px(10.0))
                    .border_b_1()
                    .border_color(to_hsla(p.line.composite_over(p.surface)))
                    .child(
                        h_row()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child(if zh { zl } else { el }),
                            )
                            .when_some(self.status_note.clone(), |el, (tone, msg)| {
                                el.child(badge(SharedString::from(msg), tone, cx))
                            }),
                    )
                    .child(btn_icon("util-close", UiIcon::Close).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.dismissed.set(true);
                            cx.notify();
                        },
                    ))),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(nav)
                    .child(div().flex_1().min_w_0().p(px(16.0)).child(content)),
            )
    }
}
