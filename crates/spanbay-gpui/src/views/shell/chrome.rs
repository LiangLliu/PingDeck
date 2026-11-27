//! Shell overlays: startup notice banner, command palette modal, utility modal, and toast stack.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use spanbay_desktop::state::{NavModule, ToastKind, UtilityDialog};

use crate::ui::{
    Disableable as _, StatusShape, Tone, UiIcon, WindowExt as _, btn_icon, btn_primary,
    btn_secondary, dismissible_banner, h_row, icon_xs, modal_dialog, palette, status_indicator,
    text_input, to_hsla, tr, v_col,
};
use crate::views::utility::{ImportExportOutcome, IoTab};

use super::AppShell;

impl AppShell {
    /// Opens the unified workspace Import / Export modal dialog.
    pub fn open_import_export(
        &mut self,
        initial_tab: IoTab,
        preselect_col_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.palette_open = false;
        self.new_workspace_input = None;
        self.new_workspace_sub = None;
        self.state.active_utility = Some(UtilityDialog::ImportExport);
        let ws_id = self.state.active_workspace_id.clone();
        let ws_name = self
            .state
            .workspaces
            .iter()
            .find(|w| self.state.active_workspace_id.as_deref() == Some(w.id.as_str()))
            .map_or_else(String::new, |w| w.name.clone());
        let active_env_id = self.state.active_environment_id.clone();
        self.import_export_view.update(cx, |modal, cx| {
            modal.open(
                ws_id,
                ws_name,
                active_env_id,
                initial_tab,
                preselect_col_id,
                window,
                cx,
            );
        });
        cx.notify();
    }

    /// Opens the native 620px right-hand Diagnostics & Logs slide-over sheet.
    pub fn open_diagnostics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.palette_open = false;
        self.state.active_utility = None;
        self.new_workspace_input = None;
        self.new_workspace_sub = None;
        self.diagnostics_view
            .update(cx, |diag, cx| diag.reload_diagnostics(cx));
        let diag_ent = self.diagnostics_view.clone();
        window.open_sheet(cx, move |sheet, w, cx| {
            let sheet_w =
                crate::ui::responsive_sheet_width(620.0, f32::from(w.viewport_size().width));
            sheet
                .size(px(sheet_w))
                .title(tr(cx).shell().util_console())
                .child(diag_ent.clone())
        });
        cx.notify();
    }

    /// Dispatches a `UtilityDialog` target (Import/Export modal or Diagnostics slide-over sheet).
    pub fn open_utility(
        &mut self,
        dialog: UtilityDialog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match dialog {
            UtilityDialog::ImportExport => self.open_import_export(IoTab::Import, None, window, cx),
            UtilityDialog::Diagnostics => self.open_diagnostics(window, cx),
        }
    }

    pub(super) fn handle_import_export_outcome(
        &mut self,
        outcome: ImportExportOutcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            ImportExportOutcome::OpenCurlDraft { request, message } => {
                self.state.active_utility = None;
                let ws_id = self.state.active_workspace_id.clone();
                self.navigate(NavModule::Requests, cx);
                self.modules
                    .open_request_in_requests(ws_id, *request, window, cx);
                self.push_toast_auto(ToastKind::Success, message, None, cx);
            }
            ImportExportOutcome::WorkspaceImported { message, .. } => {
                self.modules.refresh_workspace_after_import(cx);
                self.refresh_running_work(cx);
                self.push_toast_auto(ToastKind::Success, message, None, cx);
            }
            ImportExportOutcome::Exported { message } => {
                self.push_toast_auto(ToastKind::Success, message, None, cx);
            }
        }
    }
}

pub fn render_startup_banner(
    shell: &AppShell,
    cx: &mut Context<AppShell>,
) -> Option<gpui_kit::Div> {
    if shell.state.startup_notices.is_empty() {
        return None;
    }
    let notice_text = shell.state.startup_notices.join(" · ");
    Some(div().mb(px(6.0)).child(dismissible_banner(
        "dismiss-startup-notices",
        Tone::Warning,
        notice_text,
        cx.listener(|this, _, _, cx| {
            this.state.startup_notices.clear();
            cx.notify();
        }),
        cx,
    )))
}

pub fn render_palette_overlay(
    shell: &AppShell,
    cx: &mut Context<AppShell>,
) -> Option<gpui_kit::Stateful<gpui_kit::Div>> {
    if !shell.state.palette_open {
        return None;
    }
    let p = palette(cx);
    Some(
        div()
            .id("palette-backdrop")
            .absolute()
            .inset_0()
            .bg(to_hsla(p.scrim))
            .flex()
            .justify_center()
            .pt(px(72.0))
            .on_click(cx.listener(|this, _, _, cx| {
                this.state.palette_open = false;
                cx.notify();
            }))
            .child(
                div()
                    .id("palette-modal-card")
                    .w(px(580.0))
                    .max_h(px(460.0))
                    .shadow_lg()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(shell.palette_view.clone()),
            ),
    )
}

pub fn render_utility_overlay(
    shell: &AppShell,
    window: &Window,
    cx: &mut Context<AppShell>,
) -> Option<AnyElement> {
    if let Some(input) = &shell.new_workspace_input {
        return Some(render_new_workspace_modal(input, window, cx));
    }
    match shell.state.active_utility {
        Some(UtilityDialog::ImportExport) => {
            Some(shell.import_export_view.clone().into_any_element())
        }
        _ => None,
    }
}

fn render_new_workspace_modal(
    input: &gpui_kit::Entity<crate::ui::InputState>,
    window: &Window,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let p = palette(cx);
    let t = tr(cx);
    let sh = t.shell();
    let c = t.common();
    let name_empty = input.read(cx).value().trim().is_empty();

    let body = v_col()
        .w_full()
        .gap(px(12.0))
        .child(
            v_col()
                .w_full()
                .gap(px(6.0))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink_2))
                        .child(sh.workspace_name_label()),
                )
                .child(div().w_full().min_w_0().child(text_input(input))),
        )
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(sh.workspace_modal_desc()),
        )
        .into_any_element();

    let footer = h_row()
        .gap(px(8.0))
        .child(
            btn_secondary("ws-create-cancel", c.cancel())
                .on_click(cx.listener(|this, _, _, cx| this.cancel_create_workspace(cx))),
        )
        .child(
            btn_primary("ws-create-ok", sh.workspace_create_short())
                .disabled(name_empty)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.submit_create_workspace(cx);
                })),
        )
        .into_any_element();

    let ent = cx.entity().clone();
    modal_dialog(
        "new-workspace-dialog",
        sh.workspace_modal_title(),
        440.0,
        body,
        footer,
        window,
        cx,
        move |_, _, cx| {
            ent.update(cx, |this, cx| this.cancel_create_workspace(cx));
        },
    )
}

pub fn render_toast_stack(shell: &AppShell, cx: &mut Context<AppShell>) -> Option<gpui_kit::Div> {
    if shell.state.toasts.is_empty() {
        return None;
    }
    let p = palette(cx);
    let mut stack = v_col()
        .absolute()
        .right(px(18.0))
        .bottom(px(18.0))
        .gap(px(8.0))
        .w(px(340.0));
    for toast in &shell.state.toasts {
        let id = toast.id;
        let (stripe, shape) = match toast.kind {
            ToastKind::Info => (to_hsla(p.accent), StatusShape::Ok),
            ToastKind::Success => (to_hsla(p.ok), StatusShape::Ok),
            ToastKind::Warning => (to_hsla(p.warn), StatusShape::Warn),
            ToastKind::Error => (to_hsla(p.bad), StatusShape::Bad),
        };
        let detail = toast.detail.clone();
        stack =
            stack.child(
                h_row()
                    .overflow_hidden()
                    .rounded(px(9.0))
                    .bg(to_hsla(p.overlay))
                    .border_1()
                    .border_color(to_hsla(p.line_strong.composite_over(p.overlay)))
                    .shadow_md()
                    .child(div().w(px(3.0)).h_full().bg(stripe))
                    .child(
                        h_row()
                            .flex_1()
                            .min_w_0()
                            .justify_between()
                            .gap(px(8.0))
                            .px(px(10.0))
                            .py(px(8.0))
                            .child(
                                h_row()
                                    .gap(px(8.0))
                                    .min_w_0()
                                    .child(status_indicator(shape, cx))
                                    .child(
                                        v_col()
                                            .min_w_0()
                                            .gap(px(1.0))
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                                    .text_color(to_hsla(p.ink))
                                                    .truncate()
                                                    .child(SharedString::from(toast.title.clone())),
                                            )
                                            .children(detail.map(|d| {
                                                div()
                                                    .text_xs()
                                                    .text_color(to_hsla(p.muted))
                                                    .truncate()
                                                    .child(SharedString::from(d))
                                            })),
                                    ),
                            )
                            .child(btn_icon(("toast-close", id), UiIcon::Close).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.state.dismiss_toast(id);
                                    cx.notify();
                                }),
                            )),
                    ),
            );
    }
    let _ = (Tone::Neutral, icon_xs);
    Some(stack)
}
