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
use pingdeck_desktop::core::app::ProxyTestOutcome;
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::core::proxy_url;
use pingdeck_desktop::state::{SettingsSection, UtilityDialog};

use crate::ui::{
    Anchor, Disableable as _, Popover, StatusShape, Tone, UiIcon, app_title_bar, badge,
    btn_primary, btn_secondary, h_row, icon_xs, list_row, palette, popover, popover_header,
    spinner_xs, status_indicator, text_input, titlebar_icon_btn, to_hsla, v_col,
};

use super::AppShell;

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
    let zh = shell.language() == Language::ZhCn;
    let collapsed = shell.state.sidebar_collapsed;

    let toggle_btn = titlebar_icon_btn("tb-sidebar-toggle", UiIcon::PanelLeft, None, cx)
        .tooltip(if collapsed {
            if zh {
                "展开侧栏 (⌘B)"
            } else {
                "Expand sidebar (⌘B)"
            }
        } else if zh {
            "收起侧栏 (⌘B)"
        } else {
            "Collapse sidebar (⌘B)"
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
        .child(render_proxy_popover(shell, zh, cx))
        .child(
            titlebar_icon_btn("tb-console", UiIcon::Terminal, None, cx)
                .tooltip(if zh { "诊断控制台" } else { "Console" })
                .on_click(cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.open_utility(UtilityDialog::Console, window, cx);
                })),
        )
        .child(
            titlebar_icon_btn(
                "tb-theme",
                if p.is_dark { UiIcon::Sun } else { UiIcon::Moon },
                None,
                cx,
            )
            .tooltip(if zh {
                "切换浅/深色"
            } else {
                "Toggle Light / Dark"
            })
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_theme_mode(cx);
            })),
        );

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

    app_title_bar(cx).child(left_cluster).child(right_cluster)
}

fn render_proxy_popover(shell: &AppShell, zh: bool, cx: &mut Context<AppShell>) -> Popover {
    let p = palette(cx);
    let raw_proxy = shell.state.settings.network.proxy_url.clone();
    let no_proxy_count = shell.state.settings.network.no_proxy.len();
    let mode = GlobalProxyMode::from_url(&raw_proxy);
    let testing = shell.proxy_testing;
    let test_result = shell.proxy_test_result.clone();
    let proxy_input = shell.proxy_url_input.clone();
    let shell_ent = cx.entity().clone();

    let (tooltip_label, status_dot_color, dot_shape) = match mode {
        GlobalProxyMode::System => (
            if zh {
                "全局代理：系统代理（HTTP · 流程 · AI）".to_owned()
            } else {
                "Global proxy: System proxy (HTTP · Flows · AI)".to_owned()
            },
            Some(to_hsla(p.ok)),
            StatusShape::Ok,
        ),
        GlobalProxyMode::Custom => {
            let host = compact_proxy_host(&raw_proxy);
            (
                if zh {
                    format!("全局代理：{host}（HTTP · 流程 · AI）")
                } else {
                    format!("Global proxy: {host} (HTTP · Flows · AI)")
                },
                Some(to_hsla(p.accent)),
                StatusShape::Live,
            )
        }
        GlobalProxyMode::Direct => (
            if zh {
                "全局代理：直连（无代理）".to_owned()
            } else {
                "Global proxy: Direct (no proxy)".to_owned()
            },
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
                if zh {
                    "全局出站代理"
                } else {
                    "Global Outbound Proxy"
                },
                Some((
                    "tb-proxy-open-settings".into(),
                    if zh {
                        "网络设置 ↗"
                    } else {
                        "Network settings ↗"
                    }
                    .into(),
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
                    .child(if zh {
                        "应用于所有 HTTP 请求、流程与 AI 调用；单条请求或单个服务商仍可单独覆盖。"
                    } else {
                        "Applies to all HTTP requests, flows, and AI calls; per-request or per-provider overrides still win."
                    }),
            );

            // 1. System proxy option
            menu = menu.child(
                list_row(
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
                                        .child(if zh { "跟随系统代理" } else { "System proxy" }),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.5))
                                        .text_color(to_hsla(pp.muted))
                                        .child(if zh {
                                            "自动使用操作系统或环境变量代理"
                                        } else {
                                            "Inherit OS / environment proxy settings"
                                        }),
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
                list_row(
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
                                        .child(if zh {
                                            "直连（不使用代理）"
                                        } else {
                                            "Direct (no proxy)"
                                        }),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.5))
                                        .text_color(to_hsla(pp.muted))
                                        .child(if zh {
                                            "绕过系统代理，直接访问目标主机"
                                        } else {
                                            "Bypass system proxy and connect directly"
                                        }),
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
                                            .child(if zh { "自定义代理" } else { "Custom proxy" }),
                                    ),
                            )
                            .children((mode == GlobalProxyMode::Custom).then(|| {
                                badge(if zh { "已启用" } else { "Active" }, Tone::Accent, pop_cx)
                            })),
                    )
                    .child(
                        h_row()
                            .w_full()
                            .gap(px(6.0))
                            .child(div().flex_1().min_w_0().child(text_input(&proxy_input)))
                            .child(
                                btn_primary("tb-proxy-apply", if zh { "应用" } else { "Apply" })
                                    .on_click(move |_, _, cx| {
                                        e_cus.update(cx, |this, cx| {
                                            this.apply_custom_global_proxy(cx);
                                        });
                                    }),
                            ),
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
                                        if zh {
                                            format!("已配置 {no_proxy_count} 条绕过规则")
                                        } else {
                                            format!("{no_proxy_count} bypass rule(s)")
                                        }
                                    } else if zh {
                                        "支持 http / https / socks5".to_owned()
                                    } else {
                                        "Supports http / https / socks5".to_owned()
                                    }),
                            )
                            .child(
                                btn_secondary(
                                    "tb-proxy-test",
                                    if zh { "测试连通" } else { "Test" },
                                )
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
                    h_row()
                        .gap(px(6.0))
                        .px(px(8.0))
                        .child(spinner_xs())
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(pp.muted))
                                .child(if zh {
                                    "正在测试当前代理连通性…"
                                } else {
                                    "Testing active proxy route…"
                                }),
                        ),
                );
            } else if let Some(res) = &test_result {
                let (tone, msg) = match res.outcome {
                    ProxyTestOutcome::Connected => (
                        Tone::Success,
                        if zh {
                            format!("连通正常 · {} ({} ms)", res.route, res.elapsed_ms)
                        } else {
                            format!("Connected via {} ({} ms)", res.route, res.elapsed_ms)
                        },
                    ),
                    ProxyTestOutcome::ProxyAuth => (
                        Tone::Warning,
                        if zh {
                            "代理需要身份认证 (HTTP 407)".to_owned()
                        } else {
                            "Proxy auth required (HTTP 407)".to_owned()
                        },
                    ),
                    _ => (
                        Tone::Danger,
                        if zh {
                            format!("连接失败 ({} ms)", res.elapsed_ms)
                        } else {
                            format!("Connection failed ({} ms)", res.elapsed_ms)
                        },
                    ),
                };
                menu = menu.child(div().px(px(4.0)).child(badge(msg, tone, pop_cx)));
            }

            menu
        })
}
