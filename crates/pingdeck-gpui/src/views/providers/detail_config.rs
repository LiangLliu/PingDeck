//! Provider Config Tab (`ProviderConfigForm` entity):
//! Identity, Protocol Endpoints, and state/actions.
//! Extra Headers, Compatibility (Quirks), Advanced, Save bar & Danger Zone live in `detail_config_extra.rs`.

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Task, Window, div, px,
};
use pingdeck_desktop::core::app::{
    Impact, ProviderView, llm_provider_delete_impl, llm_provider_impact_impl,
    llm_provider_save_view_impl,
};
use pingdeck_desktop::core::llm::store::SaveProviderOptions;
use pingdeck_desktop::core::llm::wire::{AuthStyle, EndpointUrlMode, Protocol, ProviderEndpoint};

use crate::bridge::spawn_blocking_into;
use crate::ui::{
    Anchor, InputEvent, InputState, Tone, badge, btn_ghost, card, field_row, h_row, list_row,
    palette, popover, section_heading, select_pill, text_input, to_hsla, toggle_switch, v_col,
};

use super::ProvidersView;
use super::detail_config_extra::render_extra_cards;
use super::detail_config_pricing::render_pricing_card;
use super::format::{format_auth, format_protocol};

pub struct ProviderConfigForm {
    pub(super) parent: Entity<ProvidersView>,
    pub view: ProviderView,
    name_input: Entity<InputState>,
    base_url_input: Entity<InputState>,
    notes_input: Entity<InputState>,
    pub(super) models_url_input: Entity<InputState>,
    pub(super) proxy_input: Entity<InputState>,
    pub(crate) relay_mult_input: Entity<InputState>,
    pub(crate) relay_currency: Option<String>,
    pub(super) anthropic_ver_input: Entity<InputState>,
    pub(super) reasoning_field_input: Entity<InputState>,
    pub(super) new_hdr_name: Entity<InputState>,
    pub(super) new_hdr_val: Entity<InputState>,
    endpoint_prefix_inputs: Vec<(Protocol, Entity<InputState>)>,
    pub(super) saving: bool,
    pub(super) saving_pricing: bool,
    pub(super) pricing_flash: Option<(Tone, String)>,
    pub(super) needs_host_confirm: bool,
    pub(super) impact: Option<Impact>,
    pub(super) confirm_delete_open: bool,
    pub(super) flash: Option<(Tone, String)>,
    _subscriptions: Vec<Subscription>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl ProviderConfigForm {
    pub fn new(
        parent: Entity<ProvidersView>,
        pv: ProviderView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prov = &pv.provider;
        let mk_input =
            |val: String, ph: &'static str, window: &mut Window, cx: &mut Context<Self>| {
                cx.new(|cx| {
                    let mut st = InputState::new(window, cx).placeholder(ph);
                    st.set_value(val, window, cx);
                    st
                })
            };

        let name_input = mk_input(prov.name.clone(), "Provider name", window, cx);
        let base_url_input = mk_input(prov.base_url.clone(), "https://api.example.com", window, cx);
        let notes_input = mk_input(prov.notes.clone(), "Private notes…", window, cx);
        let models_url_input = mk_input(
            prov.models_endpoint.clone().unwrap_or_default(),
            "/v1/models (empty = auto)",
            window,
            cx,
        );
        let proxy_input = mk_input(
            prov.proxy_url.clone(),
            "Empty = Settings › Network; or direct / http://…",
            window,
            cx,
        );
        let (mult_str, relay_currency) = prov
            .quirks
            .relay_pricing
            .as_ref()
            .map(|rp| (format!("{}", rp.multiplier), rp.currency.clone()))
            .unwrap_or_default();
        let relay_mult_input = mk_input(mult_str, "e.g. 0.5 or 1.0", window, cx);
        let mult_sub = cx.subscribe(&relay_mult_input, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                this.pricing_flash = None;
                cx.notify();
            }
        });
        // Re-render whenever `ProvidersView` finishes reloading provider pricing / catalog families
        // or reacts to `LlmEvent::CatalogChanged` / `ModelsChanged`.
        let parent_obs = cx.observe(&parent, |_, _, cx| cx.notify());
        let anthropic_ver_input = mk_input(
            prov.quirks.anthropic_version.clone().unwrap_or_default(),
            "2023-06-01 (Auto)",
            window,
            cx,
        );
        let reasoning_field_input = mk_input(
            prov.quirks
                .reasoning_response_field
                .clone()
                .unwrap_or_default(),
            "reasoning_content (Auto)",
            window,
            cx,
        );
        let new_hdr_name = mk_input(String::new(), "Header name (e.g. X-Title)", window, cx);
        let new_hdr_val = mk_input(String::new(), "Header value", window, cx);

        let mut endpoint_prefix_inputs = Vec::new();
        for proto in Protocol::ALL {
            let prefix = prov
                .endpoints
                .iter()
                .find(|e| e.protocol == proto)
                .map(|e| e.path_prefix.clone())
                .unwrap_or_default();
            let inp = mk_input(prefix, "/v1", window, cx);
            endpoint_prefix_inputs.push((proto, inp));
        }

        let mut form = Self {
            parent,
            view: pv,
            name_input,
            base_url_input,
            notes_input,
            models_url_input,
            proxy_input,
            relay_mult_input,
            relay_currency,
            anthropic_ver_input,
            reasoning_field_input,
            new_hdr_name,
            new_hdr_val,
            endpoint_prefix_inputs,
            saving: false,
            saving_pricing: false,
            pricing_flash: None,
            needs_host_confirm: false,
            impact: None,
            confirm_delete_open: false,
            flash: None,
            _subscriptions: vec![mult_sub, parent_obs],
            _tasks: Vec::new(),
        };
        form.load_impact(cx);
        form
    }

    pub fn provider_id(&self) -> &str {
        &self.view.provider.id
    }

    fn load_impact(&mut self, cx: &mut Context<Self>) {
        let pid = self.view.provider.id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| llm_provider_impact_impl(state, &pid).ok(),
            |this, impact, cx| {
                this.impact = impact;
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    fn toggle_endpoint(&mut self, proto: Protocol, cx: &mut Context<Self>) {
        let enabled_cnt = self
            .view
            .provider
            .endpoints
            .iter()
            .filter(|e| e.enabled)
            .count();
        let eps = &mut self.view.provider.endpoints;
        if let Some(ep) = eps.iter_mut().find(|e| e.protocol == proto) {
            if ep.enabled && enabled_cnt <= 1 {
                self.flash = Some((
                    Tone::Warning,
                    crate::ui::tr(cx)
                        .providers()
                        .prov_cfg_need_one_protocol()
                        .into(),
                ));
            } else {
                ep.enabled = !ep.enabled;
            }
        } else {
            let prefix = self
                .endpoint_prefix_inputs
                .iter()
                .find(|(p, _)| *p == proto)
                .map(|(_, inp)| inp.read(cx).value().trim().to_owned())
                .unwrap_or_default();
            eps.push(ProviderEndpoint {
                protocol: proto,
                path_prefix: prefix,
                auth: None,
                url_mode: EndpointUrlMode::Standard,
                enabled: true,
            });
        }
        cx.notify();
    }

    pub(super) fn add_extra_header(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let k = self.new_hdr_name.read(cx).value().trim().to_owned();
        let v = self.new_hdr_val.read(cx).value().trim().to_owned();
        if k.is_empty() || v.is_empty() {
            return;
        }
        self.view.provider.quirks.extra_headers.insert(k, v);
        self.new_hdr_name
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.new_hdr_val
            .update(cx, |i, cx| i.set_value("", window, cx));
        cx.notify();
    }

    pub(super) fn save(&mut self, confirm_host_change: bool, cx: &mut Context<Self>) {
        let mut next = self.view.provider.clone();
        next.name = self.name_input.read(cx).value().trim().to_owned();
        next.base_url = self.base_url_input.read(cx).value().trim().to_owned();
        next.notes = self.notes_input.read(cx).value().to_string();
        let murl = self.models_url_input.read(cx).value().trim().to_owned();
        next.models_endpoint = (!murl.is_empty()).then_some(murl);
        next.proxy_url = self.proxy_input.read(cx).value().trim().to_owned();

        let av = self.anthropic_ver_input.read(cx).value().trim().to_owned();
        next.quirks.anthropic_version = (!av.is_empty()).then_some(av);
        let rf = self
            .reasoning_field_input
            .read(cx)
            .value()
            .trim()
            .to_owned();
        next.quirks.reasoning_response_field = (!rf.is_empty()).then_some(rf);

        match self.parse_relay_pricing(cx) {
            Ok(relay) => next.quirks.relay_pricing = relay,
            Err(msg) => {
                self.flash = Some((Tone::Danger, msg.into()));
                cx.notify();
                return;
            }
        }

        for (proto, inp) in &self.endpoint_prefix_inputs {
            let pref = inp.read(cx).value().trim().to_owned();
            if let Some(ep) = next.endpoints.iter_mut().find(|e| e.protocol == *proto) {
                ep.path_prefix = pref;
            }
        }

        let opts = SaveProviderOptions {
            expected_updated_at_ms: Some(self.view.provider.updated_at_ms),
            confirm_host_change,
            keep_former_name: true,
        };
        self.saving = true;
        self.needs_host_confirm = false;
        self.flash = None;
        cx.notify();

        let parent = self.parent.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| llm_provider_save_view_impl(state, next, opts),
            move |this, res, cx| {
                this.saving = false;
                match res {
                    Ok(saved) => {
                        this.view = saved;
                        this.flash = Some((
                            Tone::Success,
                            crate::ui::tr(cx)
                                .providers()
                                .prov_detail_config_configuration_saved()
                                .into(),
                        ));
                        parent.update(cx, |p, cx| p.reload_all(cx));
                    }
                    Err(err) => {
                        let msg = err.message().to_owned();
                        if msg.contains("host_change") || msg.contains("different host") {
                            this.needs_host_confirm = true;
                        }
                        this.flash = Some((Tone::Danger, msg));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn delete_now(&mut self, cx: &mut Context<Self>) {
        let pid = self.view.provider.id.clone();
        let parent = self.parent.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| llm_provider_delete_impl(state, &pid),
            move |this, res, cx| match res {
                Ok(()) => {
                    parent.update(cx, |p, cx| {
                        p.selection = super::LeftSelection::Empty;
                        p.status_msg = Some((
                            Tone::Neutral,
                            crate::ui::tr(cx)
                                .providers()
                                .prov_detail_config_provider_deleted()
                                .into(),
                        ));
                        p.reload_all(cx);
                    });
                }
                Err(err) => {
                    this.flash = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }
}

impl Render for ProviderConfigForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let prov = &self.view.provider;
        let ent = cx.entity().clone();

        let cur_auth = prov.auth;
        let ent_auth = ent;
        let auth_picker = popover("cfg-auth-pop")
            .anchor(Anchor::TopRight)
            .trigger(select_pill(
                "cfg-auth-trig",
                format_auth(cur_auth, crate::ui::active_language(cx)),
                false,
                cx,
            ))
            .content(move |_, _, cx| {
                let mut col = v_col().w(px(190.0)).gap(px(2.0)).p(px(6.0));
                for (i, a) in [
                    AuthStyle::Bearer,
                    AuthStyle::XApiKey,
                    AuthStyle::XGoogApiKey,
                    AuthStyle::RawAuthorization,
                    AuthStyle::None,
                ]
                .into_iter()
                .enumerate()
                {
                    let ent = ent_auth.clone();
                    let pop_h = cx.entity().clone();
                    col = col.child(
                        list_row(
                            SharedString::from(format!("cfg-auth-{i}")),
                            cur_auth == a,
                            cx,
                            move |_, window, cx| {
                                ent.update(cx, |this, cx| {
                                    this.view.provider.auth = a;
                                    cx.notify();
                                });
                                pop_h.update(cx, |s, cx| s.dismiss(window, cx));
                            },
                        )
                        .child(
                            div()
                                .text_xs()
                                .child(format_auth(a, crate::ui::active_language(cx))),
                        ),
                    );
                }
                col
            });

        let identity_card = card(cx).p(px(16.0)).child(
            v_col()
                .gap(px(10.0))
                .child(section_heading(
                    crate::ui::tr(cx).providers().cfg_sec_identity(),
                    cx,
                ))
                .child(field_row(
                    crate::ui::tr(cx).providers().prov_detail_config_enabled(),
                    Some(SharedString::from(
                        crate::ui::tr(cx)
                            .providers()
                            .prov_detail_config_when_off_the_gateway_chat(),
                    )),
                    toggle_switch("cfg-enabled-sw", prov.enabled).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.view.provider.enabled = !this.view.provider.enabled;
                            cx.notify();
                        },
                    )),
                    cx,
                ))
                .child(field_row(
                    crate::ui::tr(cx).providers().cfg_name(),
                    Some(SharedString::from(
                        crate::ui::tr(cx)
                            .providers()
                            .prov_detail_config_shown_across_lists_and_used(),
                    )),
                    div().w(px(280.0)).child(text_input(&self.name_input)),
                    cx,
                ))
                .child(field_row(
                    crate::ui::tr(cx).providers().prov_first_address(),
                    Some(SharedString::from(
                        crate::ui::tr(cx)
                            .providers()
                            .prov_detail_config_changing_host_requires_confirmation_and(),
                    )),
                    div().w(px(340.0)).child(text_input(&self.base_url_input)),
                    cx,
                ))
                .child(field_row(
                    crate::ui::tr(cx)
                        .providers()
                        .prov_detail_config_default_auth(),
                    None,
                    auth_picker,
                    cx,
                ))
                .child(field_row(
                    crate::ui::tr(cx).providers().prov_key_menu_notes(),
                    None,
                    div().w(px(340.0)).child(text_input(&self.notes_input)),
                    cx,
                )),
        );

        let mut ep_col = v_col().gap(px(8.0)).child(section_heading(
            crate::ui::tr(cx)
                .providers()
                .prov_detail_config_protocol_endpoints(),
            cx,
        ));
        for (idx, (proto, inp)) in self.endpoint_prefix_inputs.iter().enumerate() {
            let proto = *proto;
            let ep = prov.endpoints.iter().find(|e| e.protocol == proto);
            let enabled = ep.is_some_and(|e| e.enabled);
            let is_primary = prov.primary_protocol == Some(proto);
            let as_typed = ep.is_some_and(|e| e.url_mode == EndpointUrlMode::AsTyped);

            ep_col = ep_col.child(
                h_row()
                    .justify_between()
                    .gap(px(8.0))
                    .flex_wrap()
                    .px(px(10.0))
                    .py(px(7.0))
                    .rounded(px(7.0))
                    .bg(to_hsla(p.pane))
                    .child(
                        h_row()
                            .gap(px(8.0))
                            .child(toggle_switch(("cfg-ep-en", idx), enabled).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.toggle_endpoint(proto, cx);
                                }),
                            ))
                            .child(
                                div()
                                    .w(px(110.0))
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(if enabled { p.ink } else { p.muted }))
                                    .child(format_protocol(proto, crate::ui::active_language(cx))),
                            )
                            .child(div().w(px(150.0)).child(text_input(inp)))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(to_hsla(p.faint))
                                    .child(proto.suffix()),
                            ),
                    )
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .children(enabled.then(|| {
                                btn_ghost(
                                    SharedString::from(format!("cfg-ep-astyped-{idx}")),
                                    if as_typed {
                                        crate::ui::tr(cx).providers().prov_detail_config_as_typed()
                                    } else {
                                        crate::ui::tr(cx).providers().prov_detail_config_standard()
                                    },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if let Some(ep) = this
                                            .view
                                            .provider
                                            .endpoints
                                            .iter_mut()
                                            .find(|e| e.protocol == proto)
                                        {
                                            ep.url_mode = if ep.url_mode == EndpointUrlMode::AsTyped
                                            {
                                                EndpointUrlMode::Standard
                                            } else {
                                                EndpointUrlMode::AsTyped
                                            };
                                            cx.notify();
                                        }
                                    },
                                ))
                            }))
                            .children((enabled && proto.is_chat()).then(|| {
                                if is_primary {
                                    badge(
                                        crate::ui::tr(cx).providers().sniff_proto_primary(),
                                        Tone::Success,
                                        cx,
                                    )
                                    .into_any_element()
                                } else {
                                    btn_ghost(
                                        SharedString::from(format!("cfg-ep-prim-{idx}")),
                                        crate::ui::tr(cx)
                                            .providers()
                                            .prov_detail_config_set_primary(),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.view.provider.primary_protocol = Some(proto);
                                        cx.notify();
                                    }))
                                    .into_any_element()
                                }
                            })),
                    ),
            );
        }
        let endpoints_card = card(cx).p(px(16.0)).child(ep_col);

        v_col()
            .w_full()
            .gap(px(14.0))
            .child(identity_card)
            .child(endpoints_card)
            .child(render_pricing_card(self, cx))
            .child(render_extra_cards(self, cx))
    }
}
