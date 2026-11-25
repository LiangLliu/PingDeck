//! Settings › About page (`SettingsSection::About`, D-244).
//!
//! Displays three focused areas with live core data loaded off the UI thread:
//! 1. Build identity (version, build date, dev badge, licence, and optional repository links);
//! 2. Runtime environment (core & schema versions, OS/arch/renderer, secret store probe,
//!    OS command sandbox self-test, and read-only gateway/provider/log summary);
//! 3. Redacted diagnostic report (one-click copy with confirmed clipboard feedback plus an
//!    inline selectable preview).

mod diagnostics;
mod environment;

use std::time::Duration;

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, px,
};
use pingdeck_desktop::core::app::{
    AppInfoDto, ShellInfo, app_diagnostics_impl, app_info_impl, llm_provider_list_impl,
    settings_get_impl, system_open_external_impl,
};
use pingdeck_desktop::core::llm::gateway::GatewayState;
use pingdeck_desktop::core::logging::recent_problems;
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::i18n::{t, t_args};
use pingdeck_desktop::services::clipboard;

use crate::bridge::{runtime, spawn_blocking_into};
use crate::ui::{
    Tone, UiIcon, alert_banner, app_icon, badge, btn_ghost, h_row, icon_xs, palette, to_hsla, v_col,
};

pub(super) const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub(super) const BUILD_DATE: &str = env!("PINGDECK_BUILD_DATE");
pub(super) const GIT_SHA: &str = env!("PINGDECK_GIT_SHA");

fn current_shell_info() -> ShellInfo {
    let renderer = if cfg!(target_os = "macos") {
        "GPUI (Metal)"
    } else if cfg!(target_os = "linux") {
        "GPUI (Vulkan)"
    } else {
        "GPUI"
    };
    ShellInfo {
        app_version: APP_VERSION.to_owned(),
        webview_version: renderer.to_owned(),
    }
}

#[derive(Debug, Clone)]
pub(super) struct AboutSnapshot {
    pub info: AppInfoDto,
    pub gateway_running: bool,
    pub gateway_port: u16,
    pub gateway_lan: bool,
    pub enabled_providers: usize,
    pub total_providers: usize,
    pub recent_problem_count: usize,
}

pub struct AboutSettingsView {
    pub(super) loading: bool,
    pub(super) snapshot: Option<AboutSnapshot>,
    pub(super) load_error: Option<String>,
    pub(super) diag_open: bool,
    pub(super) diag_loading: bool,
    pub(super) diag_copying: bool,
    pub(super) diag_text: Option<String>,
    pub(super) diag_error: Option<String>,
    pub(super) diag_copied: bool,
    version_copied: bool,
    link_error: Option<String>,
    copy_reset_task: Option<Task<()>>,
    version_reset_task: Option<Task<()>>,
    _tasks: Vec<Task<()>>,
}

impl AboutSettingsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            loading: true,
            snapshot: None,
            load_error: None,
            diag_open: false,
            diag_loading: false,
            diag_copying: false,
            diag_text: None,
            diag_error: None,
            diag_copied: false,
            version_copied: false,
            link_error: None,
            copy_reset_task: None,
            version_reset_task: None,
            _tasks: Vec::new(),
        };
        view.refresh(cx);
        view
    }

    /// Probes live runtime environment facts on Tokio's blocking pool.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        cx.notify();
        let shell = current_shell_info();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let info = app_info_impl(state, &shell)?;
                let gateway = state.llm().gateway.status(state.llm());
                let gateway_running = matches!(gateway.state, GatewayState::Running);
                let providers = llm_provider_list_impl(state).unwrap_or_default();
                let total_providers = providers.len();
                let enabled_providers = providers.iter().filter(|p| p.enabled).count();
                let recent_problem_count = recent_problems().len();
                Ok::<AboutSnapshot, pingdeck_desktop::core::error::AppError>(AboutSnapshot {
                    info,
                    gateway_running,
                    gateway_port: gateway.port,
                    gateway_lan: gateway.lan,
                    enabled_providers,
                    total_providers,
                    recent_problem_count,
                })
            },
            |this, res, cx| {
                this.loading = false;
                match res {
                    Ok(snap) => {
                        this.snapshot = Some(snap);
                        this.load_error = None;
                    }
                    Err(err) => {
                        this.load_error = Some(err.message().to_owned());
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
        if self.diag_open {
            self.load_diagnostics_preview(cx);
        }
    }

    fn copy_version(&mut self, cx: &mut Context<Self>) {
        let mut text = format!("PingDeck v{APP_VERSION} ({BUILD_DATE})");
        if cfg!(debug_assertions) && !GIT_SHA.is_empty() {
            text.push_str(&format!(" [{GIT_SHA}]"));
        }
        let task = spawn_blocking_into(
            cx,
            move |_| clipboard::set_text(&text),
            |this, res, cx| {
                if res.is_ok() {
                    this.version_copied = true;
                    this.version_reset_task = Some(cx.spawn(async move |weak, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(2000))
                            .await;
                        let _ = weak.update(cx, |v, cx| {
                            v.version_copied = false;
                            cx.notify();
                        });
                    }));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn copy_diagnostics(&mut self, cx: &mut Context<Self>) {
        if self.diag_copying {
            return;
        }
        self.diag_copying = true;
        self.diag_error = None;
        cx.notify();

        let shell = current_shell_info();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let report = app_diagnostics_impl(state, &shell)?;
                clipboard::set_text(&report)?;
                Ok::<String, pingdeck_desktop::core::error::AppError>(report)
            },
            |this, res, cx| {
                this.diag_copying = false;
                match res {
                    Ok(report) => {
                        this.diag_text = Some(report);
                        this.diag_error = None;
                        this.diag_copied = true;
                        this.copy_reset_task = Some(cx.spawn(async move |weak, cx| {
                            cx.background_executor()
                                .timer(Duration::from_millis(2600))
                                .await;
                            let _ = weak.update(cx, |v, cx| {
                                v.diag_copied = false;
                                cx.notify();
                            });
                        }));
                    }
                    Err(err) => {
                        this.diag_copied = false;
                        this.diag_error = Some(err.message().to_owned());
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn toggle_diag_preview(&mut self, cx: &mut Context<Self>) {
        self.diag_open = !self.diag_open;
        if self.diag_open {
            self.load_diagnostics_preview(cx);
        } else {
            cx.notify();
        }
    }

    pub(super) fn load_diagnostics_preview(&mut self, cx: &mut Context<Self>) {
        self.diag_loading = true;
        self.diag_error = None;
        cx.notify();
        let shell = current_shell_info();
        let task = spawn_blocking_into(
            cx,
            move |state| app_diagnostics_impl(state, &shell),
            |this, res, cx| {
                this.diag_loading = false;
                match res {
                    Ok(report) => this.diag_text = Some(report),
                    Err(err) => this.diag_error = Some(err.message().to_owned()),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    fn open_external(&mut self, url: String, cx: &mut Context<Self>) {
        self.link_error = None;
        let task = spawn_blocking_into(
            cx,
            move |_| system_open_external_impl(&url),
            |this, res, cx| {
                if let Err(err) = res {
                    this.link_error = Some(err.message().to_owned());
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    fn render_identity_card(
        &self,
        lang: Language,
        zh: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = palette(cx);
        let version_line = t_args(
            lang,
            "set.about.version",
            &[("version", APP_VERSION), ("date", BUILD_DATE)],
        );
        let licence_line = t(lang, "set.about.licence").to_owned();
        let release_notes = self
            .snapshot
            .as_ref()
            .map(|s| s.info.release_notes_url.as_str())
            .filter(|u| !u.is_empty())
            .map(str::to_owned);
        let issue_tracker = self
            .snapshot
            .as_ref()
            .map(|s| s.info.issue_tracker_url.as_str())
            .filter(|u| !u.is_empty())
            .map(str::to_owned);

        let (chip_bg, chip_border, chip_fg) = if self.version_copied {
            (
                to_hsla(p.ok_soft),
                to_hsla(p.ok_line.composite_over(p.ok_soft)),
                to_hsla(p.ok),
            )
        } else {
            (
                to_hsla(p.sunken),
                to_hsla(p.line_strong.composite_over(p.sunken)),
                to_hsla(p.ink_2),
            )
        };
        let hover_bg = to_hsla(p.hover.composite_over(p.sunken));

        let version_chip = h_row()
            .id("about-version-copy")
            .gap(px(4.0))
            .px(px(8.0))
            .py(px(2.0))
            .rounded(px(6.0))
            .bg(chip_bg)
            .border_1()
            .border_color(chip_border)
            .text_xs()
            .font_weight(gpui_kit::FontWeight::MEDIUM)
            .text_color(chip_fg)
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .on_click(cx.listener(|this, _, _, cx| this.copy_version(cx)))
            .child(SharedString::from(if self.version_copied {
                if zh { "已复制版本" } else { "Copied" }.to_owned()
            } else {
                format!("v{APP_VERSION}")
            }))
            .child(icon_xs(if self.version_copied {
                UiIcon::Check
            } else {
                UiIcon::Copy
            }));

        let mut title_row = h_row()
            .gap(px(8.0))
            .flex_wrap()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(to_hsla(p.ink))
                    .child("PingDeck"),
            )
            .child(version_chip);

        if cfg!(debug_assertions) {
            title_row = title_row.child(badge(
                t(lang, "set.about.dev").to_owned(),
                Tone::Warning,
                cx,
            ));
            if !GIT_SHA.is_empty() {
                title_row = title_row.child(badge(GIT_SHA.to_owned(), Tone::Neutral, cx));
            }
        }

        let mut footer = h_row()
            .pt(px(12.0))
            .mt(px(14.0))
            .border_t_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)))
            .justify_between()
            .flex_wrap()
            .gap(px(10.0))
            .child(
                h_row()
                    .gap(px(8.0))
                    .flex_wrap()
                    .text_xs()
                    .text_color(to_hsla(p.faint))
                    .child(SharedString::from(licence_line))
                    .child("·")
                    .child(if zh {
                        "无账号 · 无云同步 · 无遥测"
                    } else {
                        "No account · No cloud sync · No telemetry"
                    }),
            );

        if release_notes.is_some() || issue_tracker.is_some() {
            let mut links = h_row().gap(px(6.0));
            if let Some(url) = release_notes {
                links = links.child(
                    btn_ghost(
                        "about-link-releases",
                        if zh { "更新日志" } else { "Release notes" },
                    )
                    .icon(icon_xs(UiIcon::ExternalLink))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_external(url.clone(), cx);
                    })),
                );
            }
            if let Some(url) = issue_tracker {
                links = links.child(
                    btn_ghost(
                        "about-link-issues",
                        if zh { "问题反馈" } else { "Issue tracker" },
                    )
                    .icon(icon_xs(UiIcon::ExternalLink))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_external(url.clone(), cx);
                    })),
                );
            }
            footer = footer.child(links);
        }

        let mut body = v_col()
            .child(
                h_row()
                    .items_start()
                    .gap(px(16.0))
                    .child(app_icon(56.0))
                    .child(
                        v_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(4.0))
                            .child(title_row)
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(to_hsla(p.muted))
                                    .child(SharedString::from(version_line)),
                            )
                            .child(
                                div()
                                    .pt(px(2.0))
                                    .text_xs()
                                    .text_color(to_hsla(p.ink_2))
                                    .child(if zh {
                                        "本地优先桌面 API 工作台、AI 网关与编码智能体宿主。"
                                    } else {
                                        "Local-first desktop API workbench, AI gateway & coding agent host."
                                    }),
                            ),
                    ),
            )
            .child(footer);

        if let Some(err) = &self.link_error {
            body = body.child(div().pt(px(8.0)).child(alert_banner(
                Tone::Danger,
                err.clone(),
                None,
                cx,
            )));
        }

        div()
            .w_full()
            .p(px(20.0))
            .rounded(px(10.0))
            .bg(to_hsla(p.surface))
            .border_1()
            .border_color(to_hsla(p.line_strong.composite_over(p.pane)))
            .child(body)
    }
}

impl Render for AboutSettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let lang = settings_get_impl(runtime(cx).state()).general.language;
        let zh = lang == Language::ZhCn;

        v_col()
            .w_full()
            .gap(px(20.0))
            .pb(px(24.0))
            .child(self.render_identity_card(lang, zh, cx))
            .child(environment::render_environment_section(self, lang, zh, cx))
            .child(diagnostics::render_diagnostics_section(self, lang, zh, cx))
    }
}
