//! Dedicated Local AI Gateway Cockpit (`NavModule::Gateway`).
//!
//! Full-width service cockpit aligned 1:1 with `docs/design/prototype/`:
//! - Compact single-line `work-head` toolbar with live indicator, copyable endpoint, auth badge,
//!   status summary pill, and action cluster (`Connect tool…` / `Start Gateway`, lifecycle,
//!   and `Usage analytics ↗` pre-filtered to `source = Gateway`);
//! - 5 full-width tabs: `Overview` / `Clients` / `Fallback Rules` / `Traffic` / `Access`;
//! - Centered `ui::modal_dialog` wizard (`connect_sheet.rs`) for connecting coding tools & snippets.

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
pub mod traffic;
pub mod traffic_actions;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use gpui_kit::{
    AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, Window, div, px,
};
use spanbay_desktop::core::app::{
    ClientInspectDto, FallbackCandidate, GatewayOverviewDto, GatewayRule, ManagedTool,
    ProviderModelView, ProviderView, RoutePreview, VerifyResult, settings_get_impl,
};
use spanbay_desktop::core::client_config::{ClientApp, DetectedClient, DetectedSnippetClient};
use spanbay_desktop::core::llm::capture::ApiClientDraft;
use spanbay_desktop::core::llm::store::{GatewayClient, ModelRef, RequestLogEntry};
use spanbay_desktop::core::model::settings::AiGatewaySettings;
use spanbay_desktop::services::clipboard;
use spanbay_desktop::state::SettingsSection;

use crate::bridge::runtime;
use crate::ui::{
    InputEvent, InputState, ScrollableElement as _, StatusShape, Tone, UiIcon, alert_banner, badge,
    btn_ghost, btn_icon, btn_primary, btn_secondary, h_row, icon_xs, meta_pill, palette,
    status_indicator, subtab_strip, tab_bar_underline, tab_item, to_hsla, toolbar_strip, v_col,
};
use crate::views::providers::format::format_uptime;

use connect_sheet::ConnectClientSheet;
use traffic_actions::GatewayTrafficFilter;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GatewayTab {
    #[default]
    Overview,
    Clients,
    Rules,
    Traffic,
    Access,
}

impl GatewayTab {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Overview => 0,
            Self::Clients => 1,
            Self::Rules => 2,
            Self::Traffic => 3,
            Self::Access => 4,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Clients,
            2 => Self::Rules,
            3 => Self::Traffic,
            4 => Self::Access,
            _ => Self::Overview,
        }
    }
}

#[derive(Debug, Clone)]
pub enum GatewayHandoff {
    OpenProviders,
    OpenUsageGateway,
    OpenSettings(SettingsSection),
    OpenAiRequest(Box<ApiClientDraft>),
    AskAgent {
        title: String,
        context_markdown: String,
    },
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

    // Gateway-only Traffic Log tab state.
    pub traffic_filter: GatewayTrafficFilter,
    pub traffic_search_input: Entity<InputState>,
    pub traffic_logs: Vec<RequestLogEntry>,
    pub traffic_total: u64,
    pub traffic_page: usize,
    pub traffic_page_size: usize,
    pub traffic_loading: bool,
    pub traffic_pending_live: u64,
    pub traffic_body_match_ids: Option<HashSet<i64>>,
    pub traffic_body_searching: bool,

    // Access & Clients form inputs.
    pub gw_port_input: Entity<InputState>,
    pub gw_extra_hosts_input: Entity<InputState>,
    pub new_client_name_input: Entity<InputState>,
    pub client_rpm_input: Entity<InputState>,
    pub client_conc_input: Entity<InputState>,

    pub custom_token_open: bool,
    pub backups_expanded: bool,
    pub editing_client_policy: Option<String>,
    pub confirm_revoke_client: Option<String>,
    pub confirm_lan_open: bool,
    pub revealed_client_token: Option<(String, String)>,
    pub client_verify_results: HashMap<ClientApp, VerifyResult>,
    pub rule_editor: Option<RuleEditorState>,
    pub route_preview: Option<RoutePreview>,
    pub client_models_page: usize,
    pub connect_modal: Option<Entity<ConnectClientSheet>>,

    pub status_msg: Option<(Tone, String)>,
    pub pending_handoff: Cell<Option<GatewayHandoff>>,
    pub(super) pending_gw_inputs_sync: Option<(String, String)>,
    pub(super) llm_sub_id: Option<u64>,
    pub(super) llm_stream_task: Option<Task<()>>,
    pub(super) _subscriptions: Vec<Subscription>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl GatewayView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = settings_get_impl(runtime(cx).state());
        let gw_settings = settings.ai.gateway.clone();
        let pr = crate::ui::tr(cx).providers();

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
        let new_client_name_input = mk(pr.prov_providers_new_client_name(), "", window, cx);
        let client_rpm_input = mk("60", "", window, cx);
        let client_conc_input = mk("8", "", window, cx);
        let traffic_search_input = mk(pr.prov_gateway_traffic_search_placeholder(), "", window, cx);
        let tf_sub = cx.subscribe(&traffic_search_input, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change | InputEvent::PressEnter { .. }) {
                this.handle_traffic_search_changed(cx);
            }
        });
        let tok_sub = cx.subscribe_in(
            &new_client_name_input,
            window,
            |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.create_gateway_client(window, cx);
                }
            },
        );

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
            traffic_filter: GatewayTrafficFilter::All,
            traffic_search_input,
            traffic_logs: Vec::new(),
            traffic_total: 0,
            traffic_page: 0,
            traffic_page_size: 50,
            traffic_loading: false,
            traffic_pending_live: 0,
            traffic_body_match_ids: None,
            traffic_body_searching: false,
            gw_port_input,
            gw_extra_hosts_input,
            new_client_name_input,
            client_rpm_input,
            client_conc_input,
            custom_token_open: false,
            backups_expanded: false,
            editing_client_policy: None,
            confirm_revoke_client: None,
            confirm_lan_open: false,
            revealed_client_token: None,
            client_verify_results: HashMap::new(),
            rule_editor: None,
            route_preview: None,
            client_models_page: 0,
            connect_modal: None,
            status_msg: None,
            pending_handoff: Cell::new(None),
            pending_gw_inputs_sync: None,
            llm_sub_id: None,
            llm_stream_task: None,
            _subscriptions: vec![tf_sub, tok_sub],
            _tasks: Vec::new(),
        };
        view.ensure_llm_subscribed(cx);
        view.reload_all(cx);
        view.reload_traffic_page(true, cx);
        view
    }

    #[must_use]
    pub fn take_pending_handoff(&self) -> Option<GatewayHandoff> {
        self.pending_handoff.take()
    }

    pub fn set_tab(&mut self, tab: GatewayTab, cx: &mut Context<Self>) {
        self.gateway_tab = tab;
        if tab == GatewayTab::Traffic {
            self.reload_traffic_page(false, cx);
        } else {
            cx.notify();
        }
    }

    /// Switches to the Clients & Tokens tab, opens the inline token minting form, and focuses it.
    pub fn open_create_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.gateway_tab = GatewayTab::Clients;
        self.custom_token_open = true;
        self.new_client_name_input
            .update(cx, |input, cx| input.focus_handle(cx).focus(window, cx));
        cx.notify();
    }

    pub fn dismiss_modal_if_open(&mut self, cx: &mut Context<Self>) -> bool {
        let had = self.connect_modal.is_some()
            || self.confirm_lan_open
            || self.confirm_revoke_client.is_some()
            || self.rule_editor.is_some()
            || self.editing_client_policy.is_some()
            || self.custom_token_open;
        if had {
            self.connect_modal = None;
            self.confirm_lan_open = false;
            self.confirm_revoke_client = None;
            self.rule_editor = None;
            self.editing_client_policy = None;
            self.custom_token_open = false;
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

    #[must_use]
    pub fn today_hits_for_rule(&self, rule: &GatewayRule) -> u64 {
        let Some(ov) = &self.overview else {
            return 0;
        };
        let id_form = rule.primary.id_form();
        let name_form = format!(
            "{}/{}",
            self.provider_name(&rule.primary.provider_id),
            rule.primary.model
        );
        let mut sum = ov.ops.model_hits.get(&id_form).copied().unwrap_or(0);
        if name_form != id_form {
            sum = sum.saturating_add(ov.ops.model_hits.get(&name_form).copied().unwrap_or(0));
        }
        sum
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
            .map(|o| o.status.port)
            .filter(|&p| p > 0)
            .unwrap_or(self.gateway_settings.port);
        let base_url = self
            .overview
            .as_ref()
            .filter(|o| o.status.running && !o.status.base_url.is_empty())
            .map(|o| o.status.base_url.clone())
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

        let traffic_lbl: SharedString = if self.traffic_total > 0 {
            pr.prov_gateway_traffic(self.traffic_total).into()
        } else {
            pr.prov_gateway_traffic_tab().into()
        };

        let tabs = tab_bar_underline("gw-cockpit-tabs")
            .selected_index(self.gateway_tab.index())
            .child(tab_item(pr.prov_tab_overview()))
            .child(tab_item(pr.prov_gateway_clients(active_clients)))
            .child(tab_item(pr.prov_gateway_fallback_rules(rules_count)))
            .child(tab_item(traffic_lbl))
            .child(tab_item(pr.prov_gateway_access()))
            .on_click(move |idx, _, cx| {
                let next = GatewayTab::from_index(*idx);
                ent.update(cx, |this, cx| this.set_tab(next, cx));
            });

        let status_meta = if gw_running {
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

        // Token-first work-head action order:
        // running => [+ Create token] (primary) [Restart] [Stop] [Usage analytics ↗]
        // stopped => [Start Gateway] (primary) [+ Create token] [Usage analytics ↗]
        let mut right_actions = h_row().flex_shrink_0().gap(px(6.0));
        if gw_running {
            right_actions = right_actions
                .child(
                    btn_primary(
                        "gw-create-token-top",
                        pr.prov_gateway_clients_custom_token_btn(),
                    )
                    .icon(icon_xs(UiIcon::Key))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_create_token(window, cx);
                    })),
                )
                .child(
                    btn_secondary("gw-restart", pr.gw_restart())
                        .on_click(cx.listener(|this, _, _, cx| this.gateway_action("restart", cx))),
                )
                .child(
                    btn_secondary("gw-stop", pr.prov_gateway_stop())
                        .on_click(cx.listener(|this, _, _, cx| this.gateway_action("stop", cx))),
                );
        } else {
            right_actions = right_actions
                .child(
                    btn_primary("gw-start", pr.prov_gateway_start_gateway())
                        .on_click(cx.listener(|this, _, _, cx| this.gateway_action("start", cx))),
                )
                .child(
                    btn_secondary(
                        "gw-create-token-top",
                        pr.prov_gateway_clients_custom_token_btn(),
                    )
                    .icon(icon_xs(UiIcon::Key))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_create_token(window, cx);
                    })),
                );
        }
        right_actions = right_actions.child(
            btn_ghost("gw-open-usage", pr.prov_gateway_usage_analytics()).on_click(cx.listener(
                |this, _, _, cx| {
                    this.pending_handoff
                        .set(Some(GatewayHandoff::OpenUsageGateway));
                    cx.notify();
                },
            )),
        );

        let top_bar = toolbar_strip(cx)
            .justify_between()
            .flex_wrap()
            .child(
                h_row()
                    .gap(px(8.0))
                    .flex_1()
                    .min_w_0()
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
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(to_hsla(p.ink))
                            .child(pr.prov_gateway_local_gateway()),
                    )
                    .child(
                        btn_ghost("gw-copy-root", base_url)
                            .icon(icon_xs(UiIcon::Copy))
                            .border_1()
                            .border_color(to_hsla(p.line_strong.composite_over(p.surface)))
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
                    ))
                    .child(meta_pill(status_meta, cx)),
            )
            .child(right_actions);

        let flash_right = self.status_msg.clone().map(|(tone, msg)| {
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
                .into_any_element()
        });

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
            GatewayTab::Traffic => traffic::render_traffic_tab(self, cx).into_any_element(),
            GatewayTab::Access => access::render_access_tab(self, cx).into_any_element(),
        };

        v_col()
            .size_full()
            .min_w_0()
            .bg(to_hsla(p.surface))
            .child(top_bar)
            .child(subtab_strip(tabs, flash_right, cx))
            .child(
                div()
                    .id("gw-cockpit-scroll")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scrollbar()
                    .child(content_col.p(px(18.0)).child(tab_body)),
            )
            .children(self.connect_modal.clone())
    }
}
