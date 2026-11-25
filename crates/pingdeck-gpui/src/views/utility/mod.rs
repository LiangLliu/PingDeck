//! Global Utility modal hosting all 6 tools wired directly to `pingdeck-core`:
//! Import/Export (3-tab workspace import + sanitized/plaintext export), Folder Sync, Cookies,
//! TLS Certificates, Diagnostic Console (+ GPU perf bench), and Collection Runner.

pub mod bench;
pub mod console;
pub mod import_export;
mod import_export_actions;
mod tools;

use std::cell::Cell;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Task, Window, div, px,
};
use pingdeck_desktop::core::app::{
    CollectionDto, EnvironmentDto, ImportConflict, ImportPreviewDto, RunCollectionInput,
    TlsCertificateDto, certificate_delete_impl, certificate_list_impl, collection_list_impl,
    cookie_jar_delete_expired_impl, cookie_jar_list_impl, environment_list_impl,
    run_collection_impl, sync_export_impl, sync_import_impl, sync_status_impl,
};
use pingdeck_desktop::core::model::CookieJarEntry;
use pingdeck_desktop::core::model::events::RunnerEvent;
use pingdeck_desktop::core::sync::{SyncImportOptions, SyncStatus};
use pingdeck_desktop::state::UtilityDialog;
use pingdeck_desktop::streams::channel_sink;

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into, subscribe_stream_into};
use crate::ui::{
    TextareaState, Tone, UiIcon, badge, btn_icon, card, h_row, list_row, palette, to_hsla, tr,
    v_col,
};

use bench::NativeBenchState;
use console::ConsoleMode;
use import_export::IoTab;

pub struct UtilityPanel {
    active: UtilityDialog,
    pub(super) workspace_id: Option<String>,
    dismissed: Cell<bool>,
    pub bench: NativeBenchState,
    pub(crate) console_mode: ConsoleMode,
    pub(super) diag_log_tail: Vec<String>,
    pub(super) curl_input: Entity<TextareaState>,
    pub(crate) io_tab: IoTab,
    pub(super) io_format: String,
    pub(super) io_conflict: ImportConflict,
    pub(super) io_preview: Option<ImportPreviewDto>,
    pub(super) io_export_col_id: Option<String>,
    pub(super) io_export_env_id: Option<String>,
    pub(super) io_ack_secrets: bool,
    pub(super) sync_status: Option<SyncStatus>,
    pub(super) cookies: Vec<CookieJarEntry>,
    pub(super) certificates: Vec<TlsCertificateDto>,
    pub(super) collections: Vec<CollectionDto>,
    pub(super) environments: Vec<EnvironmentDto>,
    pub(super) runner_events: Vec<String>,
    pub(super) running_collection: bool,
    pub(super) status_note: Option<(Tone, String)>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl UtilityPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let bench = NativeBenchState::new(window, cx);
        let io_ph = tr(cx).utility().io_input_placeholder();
        let curl_input = cx.new(|cx| TextareaState::new(window, cx).placeholder(io_ph).rows(7));

        Self {
            active: UtilityDialog::Console,
            workspace_id: None,
            dismissed: Cell::new(false),
            bench,
            console_mode: ConsoleMode::Diagnostics,
            diag_log_tail: Vec::new(),
            curl_input,
            io_tab: IoTab::Import,
            io_format: "curl".to_owned(),
            io_conflict: ImportConflict::Update,
            io_preview: None,
            io_export_col_id: None,
            io_export_env_id: None,
            io_ack_secrets: false,
            sync_status: None,
            cookies: Vec::new(),
            certificates: Vec::new(),
            collections: Vec::new(),
            environments: Vec::new(),
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
        self.io_ack_secrets = false;
        self.reload_active(cx);
        cx.notify();
    }

    pub fn is_dismissed(&self) -> bool {
        self.dismissed.get()
    }

    pub fn take_dismissed_flag(&self) -> bool {
        self.dismissed.replace(false)
    }

    pub(super) fn reload_diagnostics(&mut self, cx: &mut Context<Self>) {
        let Some(log_path) = runtime(cx).log_file_path() else {
            self.diag_log_tail.clear();
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |_| {
                let text = std::fs::read_to_string(&log_path).unwrap_or_default();
                let lines: Vec<String> = text.lines().map(str::to_owned).collect();
                let start = lines.len().saturating_sub(100);
                lines[start..].to_vec()
            },
            |this, tail, cx| {
                this.diag_log_tail = tail;
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn reload_active(&mut self, cx: &mut Context<Self>) {
        let ws = self.workspace_id.clone();
        match self.active {
            UtilityDialog::Console => self.reload_diagnostics(cx),
            UtilityDialog::ImportExport => {
                if let Some(ws_id) = ws {
                    let task = spawn_blocking_into(
                        cx,
                        move |state| {
                            let cols =
                                collection_list_impl(state, ws_id.clone()).unwrap_or_default();
                            let envs = environment_list_impl(state, ws_id).unwrap_or_default();
                            (cols, envs)
                        },
                        |this, (cols, envs), cx| {
                            if this
                                .io_export_col_id
                                .as_deref()
                                .is_none_or(|id| !cols.iter().any(|c| c.id.as_str() == id))
                            {
                                this.io_export_col_id =
                                    cols.first().map(|c| c.id.as_str().to_owned());
                            }
                            if this
                                .io_export_env_id
                                .as_deref()
                                .is_none_or(|id| !envs.iter().any(|e| e.id == id))
                            {
                                this.io_export_env_id = envs.first().map(|e| e.id.clone());
                            }
                            this.collections = cols;
                            this.environments = envs;
                            cx.notify();
                        },
                    );
                    self._tasks.push(task);
                }
            }
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
        }
    }

    pub(super) fn run_sync_export(&mut self, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            |state| sync_export_impl(state, None),
            |this, res, cx| {
                match res {
                    Ok(r) => {
                        this.status_note = Some((
                            Tone::Success,
                            tr(cx).utility().sync_exported_files(r.files_written),
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

    pub(super) fn run_sync_import(&mut self, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            |state| sync_import_impl(state, None, &SyncImportOptions::default()),
            |this, res, cx| {
                match res {
                    Ok(r) => {
                        this.status_note = Some((
                            Tone::Success,
                            tr(cx).utility().sync_imported_entities(r.added + r.updated),
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
                        tr(cx).utility().cookies_removed_expired(deleted.len()),
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
                            tr(cx).utility().runner_finished(r.passed, r.total),
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
        let lang = tr(cx).lang();

        let mut nav = v_col()
            .w(px(190.0))
            .h_full()
            .p(px(10.0))
            .gap(px(4.0))
            .bg(to_hsla(p.pane))
            .border_r_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)));
        for util in UtilityDialog::ALL {
            nav = nav.child(
                list_row(
                    SharedString::from(format!("util-tab-{util:?}")),
                    self.active == util,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        this.active = util;
                        this.reload_active(cx);
                        cx.notify();
                    }),
                )
                .child(div().text_xs().child(util.label(lang))),
            );
        }

        let content: gpui_kit::AnyElement = match self.active {
            UtilityDialog::Console => console::render_console(self, cx),
            UtilityDialog::ImportExport => import_export::render_import_export(self, cx),
            UtilityDialog::Sync => tools::render_sync(self, cx),
            UtilityDialog::Cookies => tools::render_cookies(self, cx),
            UtilityDialog::Certificates => tools::render_certs(self, cx),
            UtilityDialog::Runner => tools::render_runner(self, cx),
        };

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
                                    .child(self.active.label(lang)),
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
