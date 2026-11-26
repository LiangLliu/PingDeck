//! Unified Workspace Import / Export Modal (`ImportExportModal`):
//! single source of truth shared by the Workspace menu, Requests module, and `⌘K` palette.
//!
//! 3 tabs:
//! 1. Import (cURL / Postman / OpenAPI / HAR / Insomnia / Bruno / Hoppscotch / Env JSON)
//!    with preview, 4 conflict strategies, and live workspace refresh;
//! 2. Sanitized Export (Postman v2.1 Collection or Environment JSON without plaintext secrets);
//! 3. Plaintext Export with Secrets (danger banner + mandatory acknowledgment checkbox).

use std::cell::Cell;

use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Task, Window, div, px,
};
use pingdeck_desktop::core::app::{
    CollectionDto, EnvironmentDto, ImportConflict, ImportPreviewDto, UndoToken,
    collection_list_impl, environment_list_impl,
};
use pingdeck_desktop::core::model::Request;

use crate::bridge::spawn_blocking_into;
use crate::ui::{
    Disableable as _, ScrollableElement as _, TextareaState, Tone, UiIcon, alert_banner, badge,
    btn_danger, btn_primary, btn_secondary, card, checkbox, choice_chip, h_row, icon_xs,
    modal_dialog, multiline_input, palette, section_heading, tab_bar_segmented, tab_item, to_hsla,
    tr, v_col,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IoTab {
    #[default]
    Import,
    SanitizedExport,
    SecretsExport,
}

impl IoTab {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Import => 0,
            Self::SanitizedExport => 1,
            Self::SecretsExport => 2,
        }
    }

    #[must_use]
    pub const fn from_index(i: usize) -> Self {
        match i {
            1 => Self::SanitizedExport,
            2 => Self::SecretsExport,
            _ => Self::Import,
        }
    }
}

/// Cross-shell side effect emitted when an import or export completes.
#[derive(Debug, Clone)]
pub enum ImportExportOutcome {
    OpenCurlDraft {
        request: Box<Request>,
        message: String,
    },
    WorkspaceImported {
        message: String,
        undo_token: Option<UndoToken>,
    },
    Exported {
        message: String,
    },
}

const IMPORT_FORMATS: [(&str, &str); 8] = [
    ("curl", "cURL"),
    ("postman", "Postman"),
    ("openapi", "OpenAPI"),
    ("har", "HAR"),
    ("insomnia", "Insomnia"),
    ("bruno", "Bruno"),
    ("hoppscotch", "Hoppscotch"),
    ("environment", "Env JSON"),
];

pub struct ImportExportModal {
    pub(super) workspace_id: Option<String>,
    pub(super) active_env_id: Option<String>,
    pub(super) workspace_name: String,
    pub(super) dismissed: Cell<bool>,
    pub(super) pending_outcome: Cell<Option<ImportExportOutcome>>,
    pub payload_input: Entity<TextareaState>,
    pub io_tab: IoTab,
    pub io_format: String,
    pub io_conflict: ImportConflict,
    pub io_preview: Option<ImportPreviewDto>,
    pub io_export_col_id: Option<String>,
    pub io_export_env_id: Option<String>,
    pub io_ack_secrets: bool,
    pub collections: Vec<CollectionDto>,
    pub environments: Vec<EnvironmentDto>,
    pub status_note: Option<(Tone, String)>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl ImportExportModal {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let io_ph = tr(cx).utility().io_input_placeholder();
        let payload_input = cx.new(|cx| TextareaState::new(window, cx).placeholder(io_ph).rows(7));

        Self {
            workspace_id: None,
            active_env_id: None,
            workspace_name: String::new(),
            dismissed: Cell::new(false),
            pending_outcome: Cell::new(None),
            payload_input,
            io_tab: IoTab::Import,
            io_format: "curl".to_owned(),
            io_conflict: ImportConflict::Update,
            io_preview: None,
            io_export_col_id: None,
            io_export_env_id: None,
            io_ack_secrets: false,
            collections: Vec::new(),
            environments: Vec::new(),
            status_note: None,
            _tasks: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &mut self,
        workspace_id: Option<String>,
        workspace_name: String,
        active_env_id: Option<String>,
        initial_tab: IoTab,
        preselect_col_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace_id = workspace_id;
        self.workspace_name = workspace_name;
        self.active_env_id = active_env_id;
        self.io_tab = initial_tab;
        self.io_preview = None;
        self.io_ack_secrets = false;
        self.status_note = None;
        if preselect_col_id.is_some() {
            self.io_export_col_id = preselect_col_id;
        }
        self.dismissed.set(false);
        self.payload_input
            .update(cx, |t, cx| t.set_value("", window, cx));
        self.reload_catalog(cx);
        cx.notify();
    }

    pub fn has_pending_signal(&self) -> bool {
        if self.dismissed.get() {
            return true;
        }
        let val = self.pending_outcome.take();
        let has = val.is_some();
        self.pending_outcome.set(val);
        has
    }

    pub fn take_dismissed_flag(&self) -> bool {
        self.dismissed.replace(false)
    }

    pub fn take_pending_outcome(&self) -> Option<ImportExportOutcome> {
        self.pending_outcome.take()
    }

    pub(super) fn reload_catalog(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            self.collections.clear();
            self.environments.clear();
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let cols = collection_list_impl(state, ws_id.clone()).unwrap_or_default();
                let envs = environment_list_impl(state, ws_id).unwrap_or_default();
                (cols, envs)
            },
            |this, (cols, envs), cx| {
                if this
                    .io_export_col_id
                    .as_deref()
                    .is_none_or(|id| !cols.iter().any(|c| c.id.as_str() == id))
                {
                    this.io_export_col_id = cols.first().map(|c| c.id.as_str().to_owned());
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

impl Render for ImportExportModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let t = tr(cx);
        let u = t.utility();
        let c = t.common();
        let sh = t.shell();

        let title = if self.workspace_name.trim().is_empty() {
            sh.util_import_export().to_owned()
        } else {
            format!("{} · {}", sh.util_import_export(), self.workspace_name)
        };

        let tabs = tab_bar_segmented("util-io-tabs")
            .selected_index(self.io_tab.index())
            .child(tab_item(u.io_tab_import()))
            .child(tab_item(u.io_tab_sanitized()))
            .child(tab_item(u.io_tab_secrets()))
            .on_click(cx.listener(|this, idx: &usize, _, cx| {
                this.io_tab = IoTab::from_index(*idx);
                this.status_note = None;
                cx.notify();
            }));

        let tab_body: AnyElement = match self.io_tab {
            IoTab::Import => render_import_tab(self, cx),
            IoTab::SanitizedExport => render_export_tab(self, false, cx),
            IoTab::SecretsExport => render_export_tab(self, true, cx),
        };

        let mut body = v_col()
            .w_full()
            .gap(px(12.0))
            .child(
                h_row()
                    .w_full()
                    .justify_between()
                    .gap(px(8.0))
                    .flex_wrap()
                    .child(tabs)
                    .children(
                        self.status_note
                            .clone()
                            .map(|(tone, msg)| badge(SharedString::from(msg), tone, cx)),
                    ),
            )
            .child(
                div()
                    .id("util-io-scroll")
                    .w_full()
                    .max_h(px(440.0))
                    .overflow_y_scrollbar()
                    .child(tab_body),
            );
        body = body.text_color(to_hsla(p.ink));

        let footer_left = div()
            .flex_1()
            .min_w_0()
            .text_xs()
            .text_color(to_hsla(p.faint))
            .truncate()
            .child(u.io_footer_hint());

        let mut footer_right = h_row().gap(px(8.0)).flex_shrink_0().child(
            btn_secondary("uio-cancel", c.close()).on_click(cx.listener(|this, _, _, cx| {
                this.dismissed.set(true);
                cx.notify();
            })),
        );
        if self.io_tab == IoTab::Import {
            footer_right = footer_right
                .child(
                    btn_secondary("uio-preview", u.io_preview_btn())
                        .on_click(cx.listener(|this, _, _, cx| this.run_io_preview(cx))),
                )
                .child(
                    btn_primary("uio-commit", u.io_commit_btn())
                        .on_click(cx.listener(|this, _, _, cx| this.run_io_import(cx))),
                );
        }

        let footer = h_row()
            .w_full()
            .justify_between()
            .gap(px(12.0))
            .child(footer_left)
            .child(footer_right);

        let ent = cx.entity().clone();
        modal_dialog(
            "workspace-io-modal",
            title,
            680.0,
            body.into_any_element(),
            footer.into_any_element(),
            window,
            cx,
            move |_, _, cx| {
                ent.update(cx, |this, cx| {
                    this.dismissed.set(true);
                    cx.notify();
                });
            },
        )
    }
}

fn render_import_tab(modal: &ImportExportModal, cx: &mut Context<ImportExportModal>) -> AnyElement {
    let p = palette(cx);
    let u = tr(cx).utility();
    let mut fmt_row = h_row().gap(px(5.0)).flex_wrap();
    for (id, label) in IMPORT_FORMATS {
        let active = modal.io_format == id;
        let id_s = id.to_owned();
        fmt_row = fmt_row.child(
            choice_chip(
                SharedString::from(format!("uio-fmt-{id}")),
                label,
                active,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.io_format = id_s.clone();
                this.io_preview = None;
                cx.notify();
            })),
        );
    }

    let mut conflict_row = h_row().gap(px(5.0)).flex_wrap().child(
        div()
            .text_xs()
            .text_color(to_hsla(p.muted))
            .child(u.io_conflicts_label()),
    );
    for (c, id_tag, label) in [
        (ImportConflict::Update, "update", u.io_conflict_update()),
        (
            ImportConflict::KeepBoth,
            "keep_both",
            u.io_conflict_keep_both(),
        ),
        (ImportConflict::Replace, "replace", u.io_conflict_replace()),
        (ImportConflict::Skip, "skip", u.io_conflict_skip()),
    ] {
        conflict_row = conflict_row.child(
            choice_chip(
                SharedString::from(format!("uio-cf-{id_tag}")),
                label,
                modal.io_conflict == c,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.io_conflict = c;
                cx.notify();
            })),
        );
    }

    let mut col = v_col()
        .w_full()
        .gap(px(10.0))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .items_start()
                .gap(px(8.0))
                .child(div().flex_1().min_w_0().child(fmt_row))
                .child(
                    btn_secondary("uio-pick-file", u.io_load_file())
                        .icon(icon_xs(UiIcon::Upload))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.pick_import_file(window, cx);
                        })),
                ),
        )
        .child(multiline_input(&modal.payload_input))
        .child(conflict_row);

    if let Some(prev) = &modal.io_preview {
        let mut pcard = card(cx)
            .p(px(12.0))
            .gap(px(6.0))
            .child(section_heading(u.io_preview_heading(), cx))
            .child(
                h_row()
                    .gap(px(6.0))
                    .flex_wrap()
                    .child(badge(prev.format.clone(), Tone::Accent, cx))
                    .children(
                        prev.name
                            .as_ref()
                            .map(|n| badge(SharedString::from(n.clone()), Tone::Neutral, cx)),
                    )
                    .children(
                        prev.requests
                            .map(|n| badge(u.io_preview_requests(n), Tone::Success, cx)),
                    )
                    .children(
                        prev.folders
                            .map(|n| badge(u.io_preview_folders(n), Tone::Neutral, cx)),
                    )
                    .children(
                        prev.variables
                            .map(|n| badge(u.io_preview_vars(n), Tone::Neutral, cx)),
                    ),
            );
        if !prev.warnings.is_empty() {
            pcard = pcard.child(alert_banner(
                Tone::Warning,
                prev.warnings.join(" · "),
                None,
                cx,
            ));
        }
        col = col.child(pcard);
    }

    col.into_any_element()
}

fn render_export_tab(
    modal: &ImportExportModal,
    include_secrets: bool,
    cx: &mut Context<ImportExportModal>,
) -> AnyElement {
    let p = palette(cx);
    let u = tr(cx).utility();
    let can_export = !include_secrets || modal.io_ack_secrets;
    let mut col = v_col().w_full().gap(px(12.0));

    if include_secrets {
        col = col
            .child(alert_banner(
                Tone::Danger,
                u.io_export_secrets_warning(),
                None,
                cx,
            ))
            .child(
                h_row()
                    .gap(px(8.0))
                    .child(
                        checkbox("uio-exp-ack", modal.io_ack_secrets).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.io_ack_secrets = !this.io_ack_secrets;
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.ink))
                            .child(u.io_export_secrets_ack()),
                    ),
            );
    } else {
        col = col.child(alert_banner(
            Tone::Success,
            u.io_export_sanitized_notice(),
            None,
            cx,
        ));
    }

    let mut col_chips = h_row().gap(px(5.0)).flex_wrap();
    for c in &modal.collections {
        let cid = c.id.as_str().to_owned();
        let active = modal.io_export_col_id.as_deref() == Some(c.id.as_str());
        col_chips = col_chips.child(
            choice_chip(
                SharedString::from(format!("uio-ecol-{include_secrets}-{}", c.id.as_str())),
                c.name.clone(),
                active,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.io_export_col_id = Some(cid.clone());
                cx.notify();
            })),
        );
    }
    let has_col = modal.io_export_col_id.is_some();
    col = col.child(
        card(cx)
            .p(px(14.0))
            .gap(px(8.0))
            .child(section_heading(u.io_export_col_heading(), cx))
            .child(if modal.collections.is_empty() {
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(u.io_export_no_cols())
            } else {
                col_chips
            })
            .child(if include_secrets {
                btn_danger("uio-exp-col-sec", u.io_export_col_plaintext())
                    .disabled(!can_export || !has_col)
                    .on_click(cx.listener(|this, _, _, cx| this.export_collection(true, cx)))
            } else {
                btn_primary("uio-exp-col-safe", u.io_export_col_sanitized())
                    .disabled(!has_col)
                    .on_click(cx.listener(|this, _, _, cx| this.export_collection(false, cx)))
            }),
    );

    let mut env_chips = h_row().gap(px(5.0)).flex_wrap();
    for e in &modal.environments {
        let eid = e.id.clone();
        let active = modal.io_export_env_id.as_deref() == Some(e.id.as_str());
        env_chips = env_chips.child(
            choice_chip(
                SharedString::from(format!("uio-eenv-{include_secrets}-{}", e.id)),
                e.name.clone(),
                active,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.io_export_env_id = Some(eid.clone());
                cx.notify();
            })),
        );
    }
    let has_env = modal.io_export_env_id.is_some();
    col = col.child(
        card(cx)
            .p(px(14.0))
            .gap(px(8.0))
            .child(section_heading(u.io_export_env_heading(), cx))
            .child(if modal.environments.is_empty() {
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(u.io_export_no_envs())
            } else {
                env_chips
            })
            .child(if include_secrets {
                btn_danger("uio-exp-env-sec", u.io_export_env_plaintext())
                    .disabled(!can_export || !has_env)
                    .on_click(cx.listener(|this, _, _, cx| this.export_environment(true, cx)))
            } else {
                btn_primary("uio-exp-env-safe", u.io_export_env_sanitized())
                    .disabled(!has_env)
                    .on_click(cx.listener(|this, _, _, cx| this.export_environment(false, cx)))
            }),
    );

    col.into_any_element()
}
