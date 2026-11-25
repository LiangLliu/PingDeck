//! Slide-over Call Detail Drawer (`WindowExt::open_sheet`) for inspecting `LogDetailDto`,
//! replaying, copying cURL / diagnostics, and handing off to AI Requests or Chat.

use gpui_kit::{
    Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _,
    Task, Window, div, px,
};
use pingdeck_desktop::core::app::{
    DiagnosticsBundleInput, LogDetailDto, llm_diagnostics_bundle_impl, llm_log_curl_impl,
    llm_log_delete_capture_impl, llm_log_get_impl, llm_log_open_in_api_client_impl,
    llm_log_replay_cancel_impl, llm_log_replay_impl, settings_get_impl,
};
use pingdeck_desktop::core::llm::capture::CurlKind;
use pingdeck_desktop::core::llm::gateway::replay::{ReplayEvent, ReplayMode};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::services::clipboard;
use pingdeck_desktop::streams::channel_sink;

use crate::bridge::{runtime, spawn_blocking_into, subscribe_stream_into};
use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, WindowExt as _, alert_banner, badge, btn_danger,
    btn_ghost, btn_icon, btn_primary, btn_secondary, card, h_row, icon_xs, meta_pill, palette,
    section_header, subtab_strip, tab_bar_underline, tab_item, to_hsla, v_col,
};

use super::call_drawer_tabs::{
    render_frames_section, render_overview_section, render_payloads_section, render_reply_section,
};
use super::state::{UsageHandoff, UsageView, source_label};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum DrawerTab {
    #[default]
    Overview,
    Reply,
    Payloads,
    Frames,
}

impl DrawerTab {
    const ALL: [Self; 4] = [Self::Overview, Self::Reply, Self::Payloads, Self::Frames];

    const fn index(self) -> usize {
        match self {
            Self::Overview => 0,
            Self::Reply => 1,
            Self::Payloads => 2,
            Self::Frames => 3,
        }
    }

    const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Reply,
            2 => Self::Payloads,
            3 => Self::Frames,
            _ => Self::Overview,
        }
    }

    const fn label(self, lang: Language) -> &'static str {
        let u = crate::ui::Tr(lang).usage();
        match self {
            Self::Overview => u.usage_call_drawer_overview(),
            Self::Reply => u.usage_call_drawer_merged_reply(),
            Self::Payloads => u.usage_call_drawer_payloads(),
            Self::Frames => u.usage_call_drawer_sse_frames(),
        }
    }
}

pub struct CallDetailSheet {
    parent: Entity<UsageView>,
    pub lang: Language,
    pub detail: LogDetailDto,
    tab: DrawerTab,
    replay_id: Option<String>,
    replay_output: String,
    confirm_redacted_replay: Option<ReplayMode>,
    status_note: Option<(Tone, String)>,
    pending_close: bool,
    _tasks: Vec<Task<()>>,
}

impl CallDetailSheet {
    pub fn new(
        parent: Entity<UsageView>,
        detail: LogDetailDto,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let lang = settings_get_impl(runtime(cx).state()).general.language;
        Self {
            parent,
            lang,
            detail,
            tab: DrawerTab::Overview,
            replay_id: None,
            replay_output: String::new(),
            confirm_redacted_replay: None,
            status_note: None,
            pending_close: false,
            _tasks: Vec::new(),
        }
    }

    fn start_replay(&mut self, mode: ReplayMode, confirm_redacted: bool, cx: &mut Context<Self>) {
        if self.detail.replay.needs_confirmation && !confirm_redacted {
            self.confirm_redacted_replay = Some(mode);
            cx.notify();
            return;
        }
        self.confirm_redacted_replay = None;
        self.replay_output.clear();
        let log_id = self.detail.entry.id;
        let (sink, rx) = channel_sink::<ReplayEvent>();
        match llm_log_replay_impl(runtime(cx).state(), log_id, mode, confirm_redacted, sink) {
            Ok(started) => {
                self.replay_id = Some(started.replay_id);
                let task = subscribe_stream_into(cx, rx, 16, |this, events, cx| {
                    for ev in events {
                        match ev {
                            ReplayEvent::Frames { text } => this.replay_output.push_str(&text),
                            ReplayEvent::Done { new_log_id } => {
                                this.replay_id = None;
                                let u = crate::ui::Tr(this.lang).usage();
                                this.status_note = Some((
                                    Tone::Success,
                                    match new_log_id {
                                        Some(id) => u.usage_drawer_replay_done_log(id),
                                        None => u.usage_drawer_replay_done().to_owned(),
                                    },
                                ));
                                this.parent.update(cx, |p, cx| p.reload_all(cx));
                            }
                        }
                    }
                    cx.notify();
                });
                self._tasks.push(task);
            }
            Err(e) => {
                self.status_note = Some((Tone::Danger, e.message().to_owned()));
            }
        }
        cx.notify();
    }

    fn copy_curl(&mut self, kind: CurlKind, cx: &mut Context<Self>) {
        match llm_log_curl_impl(runtime(cx).state(), self.detail.entry.id, kind) {
            Ok(out) => {
                let _ = clipboard::set_text(&out.text);
                self.status_note = Some((
                    Tone::Success,
                    crate::ui::Tr(self.lang)
                        .usage()
                        .usage_call_drawer_copied_redacted_curl()
                        .to_owned(),
                ));
            }
            Err(e) => self.status_note = Some((Tone::Danger, e.message().to_owned())),
        }
        cx.notify();
    }

    fn copy_diagnostics(&mut self, cx: &mut Context<Self>) {
        let input = DiagnosticsBundleInput {
            include_provider_addresses: false,
            include_bodies_of: Some(self.detail.entry.id),
        };
        match llm_diagnostics_bundle_impl(runtime(cx).state(), input) {
            Ok(bundle) => {
                let _ = clipboard::set_text(&bundle);
                self.status_note = Some((
                    Tone::Success,
                    crate::ui::Tr(self.lang)
                        .usage()
                        .usage_call_drawer_copied_diagnostics_bundle()
                        .to_owned(),
                ));
            }
            Err(e) => self.status_note = Some((Tone::Danger, e.message().to_owned())),
        }
        cx.notify();
    }

    fn delete_capture(&mut self, cx: &mut Context<Self>) {
        let id = self.detail.entry.id;
        let task = spawn_blocking_into(
            cx,
            move |state| {
                llm_log_delete_capture_impl(state, id)?;
                llm_log_get_impl(state, id)
            },
            |this, res, cx| {
                match res {
                    Ok(updated) => {
                        this.detail = updated;
                        this.status_note = Some((
                            Tone::Success,
                            crate::ui::Tr(this.lang)
                                .usage()
                                .usage_call_drawer_deleted_capture_for_this_call()
                                .to_owned(),
                        ));
                        this.parent.update(cx, |p, cx| p.reload_all(cx));
                    }
                    Err(e) => this.status_note = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    fn open_in_requests(&mut self, cx: &mut Context<Self>) {
        match llm_log_open_in_api_client_impl(runtime(cx).state(), self.detail.entry.id) {
            Ok(draft) => {
                self.parent.update(cx, |p, cx| {
                    p.pending_handoff
                        .set(Some(UsageHandoff::OpenAiRequest(Box::new(draft))));
                    cx.notify();
                });
                self.pending_close = true;
                cx.notify();
            }
            Err(e) => {
                self.status_note = Some((Tone::Danger, e.message().to_owned()));
                cx.notify();
            }
        }
    }

    fn ask_agent(&mut self, cx: &mut Context<Self>) {
        let e = &self.detail.entry;
        let reply_preview = self
            .detail
            .merged_reply
            .as_ref()
            .map(|r| r.text.chars().take(1200).collect::<String>())
            .unwrap_or_default();
        let u = crate::ui::Tr(self.lang).usage();
        let error_line = e
            .error
            .as_deref()
            .map(|err| u.usage_drawer_ask_agent_error_line(err))
            .unwrap_or_default();
        let reply_block = if reply_preview.is_empty() {
            String::new()
        } else {
            format!("\n```text\n{reply_preview}\n```")
        };
        let md = u.usage_drawer_ask_agent_prompt(
            e.id,
            &e.requested_model,
            e.upstream_model.as_deref().unwrap_or("—"),
            e.provider_name.as_deref().unwrap_or("—"),
            e.status,
            &e.outcome,
            e.latency_ms,
            format!("{:?}", e.ttft_ms),
            e.input_tokens.unwrap_or(0),
            e.output_tokens.unwrap_or(0),
            e.cached_tokens.unwrap_or(0),
            error_line,
            reply_block,
        );
        self.parent.update(cx, |p, cx| {
            p.pending_handoff.set(Some(UsageHandoff::AskAgent {
                title: format!("Log #{} · {}", e.id, e.requested_model),
                context_markdown: md,
            }));
            cx.notify();
        });
        self.pending_close = true;
        cx.notify();
    }
}

impl Render for CallDetailSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.lang = crate::ui::active_language(cx);
        if self.pending_close {
            self.pending_close = false;
            window.close_sheet(cx);
        }
        let p = palette(cx);
        let e = &self.detail.entry;
        let status_tone = if e.outcome == "canceled" {
            Tone::Neutral
        } else if e.status >= 200 && e.status < 300 {
            Tone::Success
        } else {
            Tone::Danger
        };

        let header = h_row()
            .w_full()
            .justify_between()
            .px(px(16.0))
            .py(px(12.0))
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)))
            .child(
                h_row()
                    .gap(px(8.0))
                    .flex_1()
                    .min_w_0()
                    .child(badge(e.status.to_string(), status_tone, cx))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(format!("#{} · {}", e.id, e.requested_model)),
                    )
                    .child(meta_pill(source_label(&e.source, self.lang), cx)),
            )
            .child(
                btn_icon("u-drawer-close", UiIcon::Close).on_click(cx.listener(
                    |_, _, window, cx| {
                        window.close_sheet(cx);
                    },
                )),
            );

        let action_bar = h_row()
            .w_full()
            .px(px(16.0))
            .py(px(8.0))
            .gap(px(6.0))
            .flex_wrap()
            .bg(to_hsla(p.pane))
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)))
            .child(
                btn_primary(
                    "u-dr-open-req",
                    crate::ui::Tr(self.lang)
                        .usage()
                        .usage_call_drawer_open_in_requests(),
                )
                .icon(icon_xs(UiIcon::Send))
                .on_click(cx.listener(|this, _, _, cx| this.open_in_requests(cx))),
            )
            .child(
                btn_secondary(
                    "u-dr-ask-ai",
                    crate::ui::Tr(self.lang)
                        .usage()
                        .usage_call_drawer_ask_agent(),
                )
                .icon(icon_xs(UiIcon::Chat))
                .on_click(cx.listener(|this, _, _, cx| this.ask_agent(cx))),
            )
            .child(if let Some(rid) = self.replay_id.clone() {
                btn_danger(
                    "u-dr-stop-replay",
                    crate::ui::Tr(self.lang)
                        .usage()
                        .usage_call_drawer_stop_replay(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    llm_log_replay_cancel_impl(runtime(cx).state(), &rid);
                    this.replay_id = None;
                    cx.notify();
                }))
            } else {
                btn_secondary(
                    "u-dr-replay",
                    crate::ui::Tr(self.lang).usage().drawer_replay(),
                )
                .icon(icon_xs(UiIcon::Play))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.start_replay(ReplayMode::Replay, false, cx);
                }))
            })
            .children(self.detail.curl_gateway.available.then(|| {
                btn_ghost(
                    "u-dr-curl-gw",
                    crate::ui::Tr(self.lang).usage().usage_call_drawer_curl_gw(),
                )
                .on_click(cx.listener(|this, _, _, cx| this.copy_curl(CurlKind::Gateway, cx)))
            }))
            .children(self.detail.curl_upstream.available.then(|| {
                btn_ghost(
                    "u-dr-curl-up",
                    crate::ui::Tr(self.lang)
                        .usage()
                        .usage_call_drawer_curl_upstream(),
                )
                .on_click(cx.listener(|this, _, _, cx| this.copy_curl(CurlKind::Upstream, cx)))
            }))
            .child(
                btn_ghost(
                    "u-dr-diag",
                    crate::ui::Tr(self.lang)
                        .usage()
                        .usage_call_drawer_diagnostics(),
                )
                .icon(icon_xs(UiIcon::Copy))
                .on_click(cx.listener(|this, _, _, cx| this.copy_diagnostics(cx))),
            )
            .children(self.detail.capture.is_some().then(|| {
                btn_ghost(
                    "u-dr-del-cap",
                    crate::ui::Tr(self.lang)
                        .usage()
                        .usage_call_drawer_delete_capture(),
                )
                .on_click(cx.listener(|this, _, _, cx| this.delete_capture(cx)))
            }));

        let mut tabs = tab_bar_underline("u-drawer-tabs")
            .selected_index(self.tab.index())
            .on_click(cx.listener(|this, idx: &usize, _, cx| {
                this.tab = DrawerTab::from_index(*idx);
                cx.notify();
            }));
        for t in DrawerTab::ALL {
            tabs = tabs.child(tab_item(t.label(self.lang)));
        }

        let mut col = v_col().w_full().p(px(16.0)).gap(px(12.0));
        if let Some((tone, msg)) = &self.status_note {
            col = col.child(alert_banner(*tone, msg.clone(), None, cx));
        }
        if let Some(mode) = self.confirm_redacted_replay {
            col = col.child(alert_banner(
                Tone::Warning,
                crate::ui::Tr(self.lang)
                    .usage()
                    .usage_call_drawer_captured_body_holds_redacted_placeholders(),
                Some(
                    h_row()
                        .gap(px(6.0))
                        .child(
                            btn_primary(
                                "u-dr-confirm-rp",
                                crate::ui::Tr(self.lang).usage().usage_call_drawer_confirm(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.start_replay(mode, true, cx);
                                },
                            )),
                        )
                        .child(
                            btn_ghost("u-dr-cancel-rp", crate::ui::Tr(self.lang).common().cancel())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_redacted_replay = None;
                                    cx.notify();
                                })),
                        )
                        .into_any_element(),
                ),
                cx,
            ));
        }
        if !self.replay_output.is_empty() {
            col = col.child(
                card(cx)
                    .p(px(12.0))
                    .gap(px(6.0))
                    .child(section_header(
                        crate::ui::Tr(self.lang)
                            .usage()
                            .usage_call_drawer_replay_output(),
                        None,
                        cx,
                    ))
                    .child(
                        div()
                            .w_full()
                            .p(px(8.0))
                            .rounded(px(6.0))
                            .bg(to_hsla(p.sunken))
                            .text_xs()
                            .text_color(to_hsla(p.ink_2))
                            .child(self.replay_output.clone()),
                    ),
            );
        }

        col = col.child(match self.tab {
            DrawerTab::Overview => render_overview_section(self, cx),
            DrawerTab::Reply => render_reply_section(self, cx),
            DrawerTab::Payloads => render_payloads_section(self, cx),
            DrawerTab::Frames => render_frames_section(self, cx),
        });

        v_col()
            .size_full()
            .bg(to_hsla(p.surface))
            .child(header)
            .child(action_bar)
            .child(subtab_strip(tabs, None, cx))
            .child(
                div()
                    .id("u-drawer-body-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(col),
            )
    }
}
