//! Unified 38px cross-platform top titlebar and Global Outbound Proxy popover.
//!
//! Icon-only chrome with balanced 28x28 hit targets (16px monochrome glyphs), crisp theme-aware
//! hover states, and strict event-propagation guards so rapid multi-clicks on icons never bubble
//! into the titlebar's double-click window zoom handler (only blank titlebar space zooms):
//! - **macOS** (traffic lights on left): `[traffic lights] [◫] ... [🌐] [>_] [☾/☀]`
//! - **Windows / Linux** (window controls on right or WM-configured side): `[◫] ... [🌐] [>_] [☾/☀] | [─ □ ×]`

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::app::ProxyTestOutcome;
use spanbay_desktop::core::proxy_url;
use spanbay_desktop::state::SettingsSection;

use crate::ui::{
    Anchor, Disableable as _, Popover, StatusShape, Tone, UiIcon, app_title_bar, badge,
    btn_primary, btn_secondary, h_row, icon_xs, menu_option, palette, popover, popover_header,
    spinner_xs, status_indicator, text_input, titlebar_icon_btn, to_hsla, tr, v_col,
};

use super::AppShell;
use super::theme_popover::render_theme_popover;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GlobalProxyMode {
    System,
    Custom,
    Direct,
}

impl GlobalProxyMode {
    #[must_use]
    pub(crate) fn from_url(raw: &str) -> Self {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            Self::System
        } else if trimmed.eq_ignore_ascii_case(proxy_url::DIRECT) {
            Self::Direct
        } else {
            Self::Custom
        }
    }
}

/// Formats a compact host:port preview for custom proxy URLs so the top bar pill stays crisp.
fn compact_proxy_host(raw: &str) -> String {
    let trimmed = raw.trim();
    let without_scheme = trimmed
        .split_once("://")
        .map_or(trimmed, |(_, rest)| rest)
        .trim_end_matches('/');
    // Strip any embedded userinfo (`user:pass@host:port`) before display.
    let host = without_scheme
        .rsplit_once('@')
        .map_or(without_scheme, |(_, h)| h);
    if host.chars().count() > 24 {
        let prefix: String = host.chars().take(23).collect();
        format!("{prefix}…")
    } else {
        host.to_owned()
    }
}

pub fn render_title_bar(shell: &AppShell, cx: &mut Context<AppShell>) -> impl IntoElement {
    let p = palette(cx);
    let sh = tr(cx).shell();
    let collapsed = shell.state.sidebar_collapsed;

    let toggle_btn = titlebar_icon_btn("tb-sidebar-toggle", UiIcon::PanelLeft, None, cx)
        .tooltip(if collapsed {
            sh.titlebar_expand_sidebar()
        } else {
            sh.titlebar_collapse_sidebar()
        })
        .on_click(cx.listener(|this, _, _, cx| {
            cx.stop_propagation();
            this.toggle_sidebar(cx);
        }));

    // Stop MouseDown and Click propagation on both interactive clusters so rapid multi-clicks on
    // any titlebar icon never reach `TitleBar`'s double-click window zoom handler.
    let left_cluster = div()
        .id("tb-left-cluster")
        .flex()
        .flex_row()
        .items_center()
        .h_full()
        .gap(px(4.0))
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.prevent_default();
            cx.stop_propagation();
        })
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(toggle_btn);

    let mut right_cluster = div()
        .id("tb-right-cluster")
        .flex()
        .flex_row()
        .items_center()
        .h_full()
        .gap(px(4.0))
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.prevent_default();
            cx.stop_propagation();
        })
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(render_proxy_popover(shell, cx))
        .child(
            titlebar_icon_btn("tb-console", UiIcon::Terminal, None, cx)
                .tooltip(sh.util_console())
                .on_click(cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.open_diagnostics(window, cx);
                })),
        )
        .child(render_theme_popover(shell, cx));

    if !cfg!(target_os = "macos") {
        // Subtle divider separating app global actions from Windows/Linux window controls.
        right_cluster = right_cluster.child(
            div()
                .w(px(1.0))
                .h(px(14.0))
                .mx(px(4.0))
                .bg(to_hsla(p.line_strong.composite_over(p.canvas))),
        );
    }

    let shell_close = cx.entity().clone();
    app_title_bar(cx)
        .on_close_window(move |_, window, cx| {
            let should_close =
                shell_close.update(cx, |this, cx| this.handle_window_close_request(window, cx));
            if should_close {
                window.remove_window();
            }
        })
        .child(left_cluster)
        .child(right_cluster)
}

fn render_proxy_popover(shell: &AppShell, cx: &mut Context<AppShell>) -> Popover {
    let p = palette(cx);
    let sh = tr(cx).shell();
    let raw_proxy = shell.state.settings.network.proxy_url.clone();
    let no_proxy_count = shell.state.settings.network.no_proxy.len();
    let mode = GlobalProxyMode::from_url(&raw_proxy);
    let testing = shell.proxy_testing;
    let test_result = shell.proxy_test_result.clone();
    let proxy_input = shell.proxy_url_input.clone();
    let shell_ent = cx.entity().clone();

    let (tooltip_label, status_dot_color, dot_shape) = match mode {
        GlobalProxyMode::System => (
            sh.proxy_tooltip_system().to_owned(),
            Some(to_hsla(p.ok)),
            StatusShape::Ok,
        ),
        GlobalProxyMode::Custom => {
            let host = compact_proxy_host(&raw_proxy);
            (
                sh.proxy_tooltip_custom(host),
                Some(to_hsla(p.accent)),
                StatusShape::Live,
            )
        }
        GlobalProxyMode::Direct => (
            sh.proxy_tooltip_direct().to_owned(),
            Some(to_hsla(p.faint)),
            StatusShape::Idle,
        ),
    };

    let trigger_el = titlebar_icon_btn("tb-proxy-trigger", UiIcon::Globe, status_dot_color, cx)
        .tooltip(SharedString::from(tooltip_label));

    popover("tb-proxy-popover")
        .anchor(Anchor::TopRight)
        .trigger(trigger_el)
        .content(move |_, _, pop_cx| {
            let pp = palette(pop_cx);
            let t = tr(pop_cx);
            let sh = t.shell();
            let c = t.common();
            let e_set = shell_ent.clone();
            let e_sys = shell_ent.clone();
            let e_dir = shell_ent.clone();
            let e_cus = shell_ent.clone();
            let e_tst = shell_ent.clone();
            let p_set = pop_cx.entity().clone();
            let p_sys = pop_cx.entity().clone();
            let p_dir = pop_cx.entity().clone();

            let mut menu = v_col().w(px(316.0)).gap(px(6.0));
            menu = menu.child(popover_header(
                sh.proxy_title(),
                Some((
                    "tb-proxy-open-settings".into(),
                    sh.proxy_network_settings().into(),
                    Box::new(move |_, window, cx| {
                        e_set.update(cx, |this, cx| {
                            this.open_settings_section(SettingsSection::Network, cx);
                        });
                        p_set.update(cx, |st, cx| st.dismiss(window, cx));
                    }),
                )),
                pop_cx,
            ));

            menu = menu.child(
                div()
                    .px(px(8.0))
                    .text_size(px(11.0))
                    .text_color(to_hsla(pp.muted))
                    .child(sh.proxy_desc()),
            );

            // 1. System proxy option
            menu = menu.child(
                menu_option(
                    "tb-proxy-mode-system",
                    mode == GlobalProxyMode::System,
                    pop_cx,
                    move |_, window, cx| {
                        e_sys.update(cx, |this, cx| {
                            this.set_global_proxy_url(String::new(), cx);
                        });
                        p_sys.update(cx, |st, cx| st.dismiss(window, cx));
                    },
                )
                .justify_between()
                .child(
                    h_row()
                        .gap(px(8.0))
                        .min_w_0()
                        .flex_1()
                        .child(status_indicator(StatusShape::Ok, pop_cx))
                        .child(
                            v_col()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                                        .child(sh.proxy_mode_system_title()),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.5))
                                        .text_color(to_hsla(pp.muted))
                                        .child(sh.proxy_mode_system_desc()),
                                ),
                        ),
                )
                .children((mode == GlobalProxyMode::System).then(|| {
                    div()
                        .text_color(to_hsla(pp.accent))
                        .child(icon_xs(UiIcon::Check))
                })),
            );

            // 2. Direct (no proxy) option
            menu = menu.child(
                menu_option(
                    "tb-proxy-mode-direct",
                    mode == GlobalProxyMode::Direct,
                    pop_cx,
                    move |_, window, cx| {
                        e_dir.update(cx, |this, cx| {
                            this.set_global_proxy_url(proxy_url::DIRECT.to_owned(), cx);
                        });
                        p_dir.update(cx, |st, cx| st.dismiss(window, cx));
                    },
                )
                .justify_between()
                .child(
                    h_row()
                        .gap(px(8.0))
                        .min_w_0()
                        .flex_1()
                        .child(status_indicator(StatusShape::Idle, pop_cx))
                        .child(
                            v_col()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                                        .child(sh.proxy_mode_direct_title()),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.5))
                                        .text_color(to_hsla(pp.muted))
                                        .child(sh.proxy_mode_direct_desc()),
                                ),
                        ),
                )
                .children((mode == GlobalProxyMode::Direct).then(|| {
                    div()
                        .text_color(to_hsla(pp.accent))
                        .child(icon_xs(UiIcon::Check))
                })),
            );

            // 3. Custom proxy quick-apply card
            menu = menu.child(
                v_col()
                    .w_full()
                    .gap(px(6.0))
                    .p(px(8.0))
                    .rounded(px(7.0))
                    .bg(to_hsla(pp.sunken))
                    .border_1()
                    .border_color(to_hsla(if mode == GlobalProxyMode::Custom {
                        pp.accent_line.composite_over(pp.sunken)
                    } else {
                        pp.line.composite_over(pp.sunken)
                    }))
                    .child(
                        h_row()
                            .justify_between()
                            .child(
                                h_row()
                                    .gap(px(6.0))
                                    .child(status_indicator(
                                        if mode == GlobalProxyMode::Custom {
                                            StatusShape::Live
                                        } else {
                                            dot_shape
                                        },
                                        pop_cx,
                                    ))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                                            .text_color(to_hsla(pp.ink))
                                            .child(sh.proxy_mode_custom_title()),
                                    ),
                            )
                            .children(
                                (mode == GlobalProxyMode::Custom)
                                    .then(|| badge(c.active(), Tone::Accent, pop_cx)),
                            ),
                    )
                    .child(
                        h_row()
                            .w_full()
                            .gap(px(6.0))
                            .child(div().flex_1().min_w_0().child(text_input(&proxy_input)))
                            .child(btn_primary("tb-proxy-apply", c.apply()).on_click(
                                move |_, _, cx| {
                                    e_cus.update(cx, |this, cx| {
                                        this.apply_custom_global_proxy(cx);
                                    });
                                },
                            )),
                    )
                    .child(
                        h_row()
                            .justify_between()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(to_hsla(pp.muted))
                                    .child(if no_proxy_count > 0 {
                                        sh.proxy_bypass_count(no_proxy_count)
                                    } else {
                                        sh.proxy_supported_schemes().to_owned()
                                    }),
                            )
                            .child(
                                btn_secondary("tb-proxy-test", sh.proxy_test_btn())
                                    .disabled(testing)
                                    .on_click(move |_, _, cx| {
                                        e_tst.update(cx, |this, cx| {
                                            this.test_global_proxy(cx);
                                        });
                                    }),
                            ),
                    ),
            );

            if testing {
                menu = menu.child(
                    h_row().gap(px(6.0)).px(px(8.0)).child(spinner_xs()).child(
                        div()
                            .text_size(px(11.0))
                            .text_color(to_hsla(pp.muted))
                            .child(sh.proxy_testing()),
                    ),
                );
            } else if let Some(res) = &test_result {
                let (tone, msg) = match res.outcome {
                    ProxyTestOutcome::Connected => (
                        Tone::Success,
                        sh.proxy_test_connected(&res.route, res.elapsed_ms),
                    ),
                    ProxyTestOutcome::ProxyAuth => (Tone::Warning, sh.proxy_test_auth().to_owned()),
                    _ => (Tone::Danger, sh.proxy_test_failed(res.elapsed_ms)),
                };
                menu = menu.child(div().px(px(4.0)).child(badge(msg, tone, pop_cx)));
            }

            menu
        })
}
