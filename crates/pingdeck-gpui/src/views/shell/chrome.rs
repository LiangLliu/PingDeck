//! Shell overlays: startup notice banner, command palette modal, utility modal, and toast stack.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, WindowControlArea, div, px,
};
use pingdeck_desktop::state::ToastKind;

use crate::ui::{
    StatusShape, Tone, UiIcon, btn_icon, dismissible_banner, h_row, icon_xs, palette,
    status_indicator, to_hsla, v_col,
};

use super::AppShell;

/// Subtle top window-drag strip above the floating page card so users can drag the window from
/// the top inset margin without a full-width titlebar chrome strip.
pub fn render_top_drag_edge() -> impl IntoElement {
    div()
        .w_full()
        .h(px(8.0))
        .flex_shrink_0()
        .window_control_area(WindowControlArea::Drag)
}

pub fn render_startup_banner(
    shell: &AppShell,
    cx: &mut Context<AppShell>,
) -> Option<gpui_kit::Div> {
    if shell.state.startup_notices.is_empty() {
        return None;
    }
    let notice_text = shell.state.startup_notices.join(" · ");
    Some(div().mx(px(8.0)).mb(px(6.0)).child(dismissible_banner(
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
    cx: &mut Context<AppShell>,
) -> Option<gpui_kit::Stateful<gpui_kit::Div>> {
    shell.state.active_utility?;
    let p = palette(cx);
    Some(
        div()
            .id("utility-backdrop")
            .absolute()
            .inset_0()
            .bg(to_hsla(p.scrim))
            .flex()
            .items_center()
            .justify_center()
            .p(px(32.0))
            .on_click(cx.listener(|this, _, _, cx| {
                this.state.active_utility = None;
                cx.notify();
            }))
            .child(
                div()
                    .id("utility-modal-card")
                    .w(px(940.0))
                    .h(px(660.0))
                    .max_w_full()
                    .max_h_full()
                    .shadow_lg()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(shell.utility_view.clone()),
            ),
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
