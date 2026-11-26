//! Dedicated Local AI Gateway Cockpit (`NavModule::Gateway`).
//!
//! Full-width service cockpit separated from upstream `ProvidersView`:
//! - Unified top header with live listening state, copyable endpoint, auth badge,
//!   single primary CTA (`Start Gateway` when stopped; `Connect Coding Tool…` when live),
//!   and secondary lifecycle/log actions;
//! - 4 full-width tabs: `Overview` / `Clients & Tools` / `Fallback Rules` / `Access & Security`;
//! - Native right-hand Slide-Over Sheet (`connect_sheet.rs`) via `WindowExt::open_sheet`.

pub mod access;
pub mod actions;
pub mod actions_rules;
pub mod backups;
pub mod clients;
pub mod connect_sheet;
pub mod connect_sheet_body;
pub mod loader;
pub mod overview;
pub mod rules;

use std::cell::Cell;
use std::collections::HashMap;

use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Task, Window, div, px,
};
use pingdeck_desktop::core::app::{
    ClientInspectDto, FallbackCandidate, GatewayOverviewDto, GatewayRule, ManagedTool,
    ProviderModelView, ProviderView, RoutePreview, VerifyResult, settings_get_impl,
};
use pingdeck_desktop::core::client_config::{ClientApp, DetectedClient, DetectedSnippetClient};
use pingdeck_desktop::core::llm::store::{GatewayClient, ModelRef};
use pingdeck_desktop::core::model::settings::AiGatewaySettings;
use pingdeck_desktop::services::clipboard;
use pingdeck_desktop::state::SettingsSection;

use crate::bridge::runtime;
use crate::ui::{
    InputState, ScrollableElement as _, StatusShape, Tone, UiIcon, alert_banner, badge, btn_ghost,
    btn_icon, btn_primary, btn_secondary, h_row, icon_xs, palette, status_indicator,
    tab_bar_underline, tab_item, to_hsla, v_col,
};

use crate::views::providers::format::format_uptime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GatewayTab {
    #[default]
    Overview,
    Clients,
    Rules,
    Access,
}

impl GatewayTab {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Overview => 0,
            Self::Clients => 1,
            Self::Rules => 2,
            Self::Access => 3,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Clients,
            2 => Self::Rules,
            3 => Self::Access,
            _ => Self::Overview,
        }
    }
}

#[derive(Debug, Clone)]
pub enum GatewayHandoff {
    OpenProviders,
    OpenUsageProvider(Option<String>),
    OpenSettings(SettingsSection),
}

pub struct RuleEditorState {
    pub primary: ModelRef,
    pub chain: Vec<ModelRef>,
    pub candidates: Vec<FallbackCandidate>,
}

pub struct GatewayView {
    pub gateway_tab: GatewayTab,
    pub providers: Vec<ProviderView>,
    pub models: Vec<ProviderModelView>,
    pub overview: Option<GatewayOverviewDto>,
    pub gateway_settings: AiGatewaySettings,
    pub detected_clients: Vec<DetectedClient>,
    pub snippet_clients: Vec<DetectedSnippetClient>,
    pub inspections: HashMap<ClientApp, ClientInspectDto>,
    pub managed_tools: Vec<ManagedTool>,
    pub gateway_clients: Vec<GatewayClient>,
    pub gateway_rules: Vec<GatewayRule>,

    pub gw_port_input: Entity<InputState>,
    pub gw_extra_hosts_input: Entity<InputState>,
    pub new_client_name_input: Entity<InputState>,
    pub client_rpm_input: Entity<InputState>,
    pub client_conc_input: Entity<InputState>,

    pub editing_client_policy: Option<String>,
    pub confirm_revoke_client: Option<String>,
    pub confirm_lan_open: bool,
    pub revealed_client_token: Option<(String, String)>,
    pub client_verify_results: HashMap<ClientApp, VerifyResult>,
    pub rule_editor: Option<RuleEditorState>,
    pub route_preview: Option<RoutePreview>,
    pub client_models_page: usize,

    pub status_msg: Option<(Tone, String)>,
    pub pending_handoff: Cell<Option<GatewayHandoff>>,
    pub(super) pending_gw_inputs_sync: Option<(String, String)>,
    pub(super) llm_sub_id: Option<u64>,
    pub(super) llm_stream_task: Option<Task<()>>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl GatewayView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = settings_get_impl(runtime(cx).state());
        let gw_settings = settings.ai.gateway.clone();

        let mk = |ph: &'static str, init: &str, w: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut st = InputState::new(w, cx).placeholder(ph);
                if !init.is_empty() {
                    st.set_value(init.to_owned(), w, cx);
                }
                st
            })
        };

        let gw_port_input = mk("4141", &gw_settings.port.to_string(), window, cx);
        let gw_extra_hosts_input = mk(
            "host.docker.internal",
            &gw_settings.extra_allowed_hosts.join(", "),
            window,
            cx,
        );
        let new_client_name_input = mk(
            crate::ui::tr(cx)
                .providers()
                .prov_providers_new_client_name(),
            "",
            window,
            cx,
        );
        let client_rpm_input = mk("60", "", window, cx);
        let client_conc_input = mk("8", "", window, cx);

        let mut view = Self {
            gateway_tab: GatewayTab::Overview,
            providers: Vec::new(),
            models: Vec::new(),
            overview: None,
            gateway_settings: gw_settings,
            detected_clients: Vec::new(),
            snippet_clients: Vec::new(),
            inspections: HashMap::new(),
            managed_tools: Vec::new(),
            gateway_clients: Vec::new(),
            gateway_rules: Vec::new(),
            gw_port_input,
            gw_extra_hosts_input,
            new_client_name_input,
            client_rpm_input,
            client_conc_input,
            editing_client_policy: None,
            confirm_revoke_client: None,
            confirm_lan_open: false,
            revealed_client_token: None,
            client_verify_results: HashMap::new(),
            rule_editor: None,
            route_preview: None,
            client_models_page: 0,
            status_msg: None,
            pending_handoff: Cell::new(None),
            pending_gw_inputs_sync: None,
            llm_sub_id: None,
            llm_stream_task: None,
            _tasks: Vec::new(),
        };
        view.ensure_llm_subscribed(cx);
        view.reload_all(cx);
        view
    }

    #[must_use]
    pub fn take_pending_handoff(&self) -> Option<GatewayHandoff> {
        self.pending_handoff.take()
    }

    pub fn set_tab(&mut self, tab: GatewayTab, cx: &mut Context<Self>) {
        self.gateway_tab = tab;
        cx.notify();
    }

    pub fn dismiss_modal_if_open(&mut self, cx: &mut Context<Self>) -> bool {
        let had = self.confirm_lan_open
            || self.confirm_revoke_client.is_some()
            || self.rule_editor.is_some()
            || self.editing_client_policy.is_some();
        if had {
            self.confirm_lan_open = false;
            self.confirm_revoke_client = None;
            self.rule_editor = None;
            self.editing_client_policy = None;
            cx.notify();
        }
        had
    }

    #[must_use]
    pub fn provider_name(&self, id: &str) -> String {
        self.providers
            .iter()
            .find(|p| p.provider.id == id)
            .map_or_else(|| id.to_owned(), |p| p.provider.name.clone())
    }

    #[must_use]
    pub fn format_model_ref(&self, raw: &str) -> String {
        if let Some((pid, mid)) = raw.split_once('/') {
            format!("{}/{mid}", self.provider_name(pid))
        } else {
            raw.to_owned()
        }
    }
}

impl Render for GatewayView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some((port_s, hosts_s)) = self.pending_gw_inputs_sync.take() {
            self.gw_port_input
                .update(cx, |i, cx| i.set_value(port_s, window, cx));
            self.gw_extra_hosts_input
                .update(cx, |i, cx| i.set_value(hosts_s, window, cx));
        }

        let p = palette(cx);
        let pr = crate::ui::tr(cx).providers();
        let gw_running = self.overview.as_ref().is_some_and(|o| o.status.running);
        let gw_port = self
            .overview
            .as_ref()
            .map_or(self.gateway_settings.port, |o| o.status.port);
        let base_url = self
            .overview
            .as_ref()
            .map(|o| o.status.base_url.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("http://127.0.0.1:{gw_port}"));
        let copy_base = base_url.clone();
        let uptime_s = self.overview.as_ref().map_or(0, |o| o.status.uptime_s);
        let require_tok = self.gateway_settings.require_token;
        let serve_lan = self.gateway_settings.serve_lan;
        let active_clients = self
            .gateway_clients
            .iter()
            .filter(|c| c.is_active())
            .count();
        let rules_count = self.gateway_rules.len();
        let ent = cx.entity().clone();

        let clients_lbl = pr.prov_gateway_clients(active_clients);
        let rules_lbl = pr.prov_gateway_fallback_rules(rules_count);

        let tabs = tab_bar_underline("gw-cockpit-tabs")
            .selected_index(self.gateway_tab.index())
            .child(tab_item(pr.prov_tab_overview()))
            .child(tab_item(clients_lbl))
            .child(tab_item(rules_lbl))
            .child(tab_item(pr.prov_gateway_access()))
            .on_click(move |idx, _, cx| {
                let next = GatewayTab::from_index(*idx);
                ent.update(cx, |this, cx| {
                    this.gateway_tab = next;
                    cx.notify();
                });
            });

        let subline = if gw_running {
            let lang = crate::ui::active_language(cx);
            let bind = if serve_lan {
                pr.prov_gw_bind_lan()
            } else {
                pr.prov_gw_bind_loopback()
            };
            pr.prov_gw_status_running(format_uptime(uptime_s, lang), bind)
        } else {
            pr.prov_gateway_start_the_gateway_to_route().into()
        };

        // Single primary CTA per screen (docs/design.md §2.2 & §3.1):
        // stopped => primary "Start Gateway", secondary "Connect Coding Tool…";
        // running => secondary "Restart" / "Stop Gateway", primary "Connect Coding Tool…".
        let mut right_actions = h_row().flex_shrink_0().gap(px(6.0)).child(
            btn_ghost("gw-open-usage", pr.prov_gateway_logs()).on_click(cx.listener(
                |this, _, _, cx| {
                    this.pending_handoff
                        .set(Some(GatewayHandoff::OpenUsageProvider(None)));
                    cx.notify();
                },
            )),
        );

        if gw_running {
            right_actions = right_actions
                .child(
                    btn_secondary("gw-restart", pr.gw_restart())
                        .on_click(cx.listener(|this, _, _, cx| this.gateway_action("restart", cx))),
                )
                .child(
                    btn_secondary("gw-stop", pr.prov_gateway_stop())
                        .on_click(cx.listener(|this, _, _, cx| this.gateway_action("stop", cx))),
                )
                .child(
                    btn_primary("gw-connect-btn", pr.prov_gateway_connect_client())
                        .icon(icon_xs(UiIcon::Terminal))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_connect_sheet(None, window, cx);
                        })),
                );
        } else {
            right_actions = right_actions
                .child(
                    btn_secondary("gw-connect-btn", pr.prov_gateway_connect_client())
                        .icon(icon_xs(UiIcon::Terminal))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_connect_sheet(None, window, cx);
                        })),
                )
                .child(
                    btn_primary("gw-start", pr.prov_gateway_start_gateway())
                        .on_click(cx.listener(|this, _, _, cx| this.gateway_action("start", cx))),
                );
        }

        let header = v_col()
            .flex_shrink_0()
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)))
            .child(
                h_row()
                    .justify_between()
                    .gap(px(12.0))
                    .px(px(20.0))
                    .pt(px(14.0))
                    .pb(px(10.0))
                    .flex_wrap()
                    .child(
                        v_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(4.0))
                            .child(
                                h_row()
                                    .gap(px(8.0))
                                    .flex_wrap()
                                    .child(status_indicator(
                                        if gw_running {
                                            StatusShape::Live
                                        } else {
                                            StatusShape::Idle
                                        },
                                        cx,
                                    ))
                                    .child(
                                        div()
                                            .text_base()
                                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                            .text_color(to_hsla(p.ink))
                                            .child(pr.prov_gateway_local_gateway()),
                                    )
                                    .child(
                                        btn_ghost("gw-copy-root", base_url)
                                            .icon(icon_xs(UiIcon::Copy))
                                            .border_1()
                                            .border_color(to_hsla(
                                                p.line_strong.composite_over(p.surface),
                                            ))
                                            .bg(to_hsla(p.pane))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                let _ = clipboard::set_text(&copy_base);
                                                this.status_msg = Some((
                                                    Tone::Success,
                                                    crate::ui::tr(cx)
                                                        .providers()
                                                        .prov_gateway_gateway_url_copied()
                                                        .into(),
                                                ));
                                                cx.notify();
                                            })),
                                    )
                                    .child(badge(
                                        if require_tok {
                                            pr.prov_gateway_token_auth_on()
                                        } else {
                                            pr.prov_gateway_anonymous_allowed()
                                        },
                                        if require_tok {
                                            Tone::Success
                                        } else {
                                            Tone::Warning
                                        },
                                        cx,
                                    )),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(to_hsla(p.muted))
                                    .truncate()
                                    .child(SharedString::from(subline)),
                            ),
                    )
                    .child(right_actions),
            )
            .child(h_row().justify_between().px(px(16.0)).child(tabs).children(
                self.status_msg.clone().map(|(tone, msg)| {
                    h_row()
                        .flex_shrink_0()
                        .gap(px(4.0))
                        .child(badge(msg, tone, cx))
                        .child(
                            btn_icon("gw-clear-flash", UiIcon::Close).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.status_msg = None;
                                    cx.notify();
                                },
                            )),
                        )
                }),
            ));

        let mut content_col = v_col().w_full().gap(px(14.0));
        if self.providers.is_empty() {
            content_col = content_col.child(alert_banner(
                Tone::Warning,
                pr.prov_gateway_no_providers_banner(),
                Some(
                    btn_secondary("gw-go-providers", pr.prov_gateway_add_provider_btn())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.pending_handoff
                                .set(Some(GatewayHandoff::OpenProviders));
                            cx.notify();
                        }))
                        .into_any_element(),
                ),
                cx,
            ));
        }

        let tab_body = match self.gateway_tab {
            GatewayTab::Overview => {
                overview::render_overview_tab(self, active_clients, cx).into_any_element()
            }
            GatewayTab::Clients => clients::render_clients_tab(self, cx).into_any_element(),
            GatewayTab::Rules => rules::render_rules_tab(self, cx).into_any_element(),
            GatewayTab::Access => access::render_access_tab(self, cx).into_any_element(),
        };
        content_col = content_col.child(tab_body);

        v_col()
            .size_full()
            .bg(to_hsla(p.surface))
            .child(header)
            .child(
                div()
                    .id("gw-cockpit-scroll")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .p(px(20.0))
                    .overflow_y_scrollbar()
                    .child(content_col),
            )
    }
}
