//! Async actions for `SniffProviderSheet`: preset selection, live streaming sniff,
//! cancellation, quick preset creation, unverified creation, save/apply re-sniff, and duplicate key add.

use std::collections::BTreeMap;

use gpui_kit::{Context, Focusable as _, Window};
use spanbay_desktop::core::app::{
    ExtraInput, ProviderFromPresetInput, SniffSelection, UnverifiedExtras,
    llm_provider_add_key_from_sniff_impl, llm_provider_apply_resniff_impl,
    llm_provider_create_from_preset_impl, llm_provider_create_from_sniff_impl,
    llm_provider_create_unverified_impl, llm_provider_duplicate_check_impl,
    llm_provider_sniff_impl, llm_url_normalize_impl,
};
use spanbay_desktop::core::llm::sniff::{SniffEvent, SniffInput, SniffWarning};
use spanbay_desktop::core::llm::wire::Protocol;
use spanbay_desktop::streams::ChannelSink;

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into, subscribe_stream_into};
use crate::ui::Tone;

use super::sniff_sheet::{LiveRow, SniffProviderSheet};

impl SniffProviderSheet {
    pub(crate) fn pick_preset(
        &mut self,
        preset_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prev_preset_name = self
            .selected_preset_id
            .as_deref()
            .and_then(|pid| self.presets.iter().find(|p| p.id == pid))
            .map(|p| p.name.clone());
        if let Some(pr) = self.presets.iter().find(|p| p.id == preset_id).cloned() {
            self.selected_preset_id = Some(pr.id);
            if let Some(reg) = pr.regions.first() {
                self.selected_region_id = Some(reg.id.clone());
                self.url_input
                    .update(cx, |i, cx| i.set_value(reg.base_url.clone(), window, cx));
                self.normalized = llm_url_normalize_impl(&reg.base_url).ok();
            }
            let cur_name = self.name_input.read(cx).value().trim().to_owned();
            if cur_name.is_empty() || prev_preset_name.as_deref() == Some(cur_name.as_str()) {
                self.name_input
                    .update(cx, |i, cx| i.set_value(pr.name, window, cx));
            }
            self.flash = None;
            cx.notify();
        }
    }

    pub(crate) fn start_sniff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name_input.read(cx).value().trim().to_owned();
        if name.is_empty() && self.resniff_provider_id.is_none() {
            self.flash = Some((
                Tone::Warning,
                crate::ui::tr(cx).providers().prov_sniff_need_name().into(),
            ));
            self.name_input
                .update(cx, |s, cx| s.focus_handle(cx).focus(window, cx));
            cx.notify();
            return;
        }
        let base_url = self.url_input.read(cx).value().trim().to_owned();
        if base_url.is_empty() && self.resniff_provider_id.is_none() {
            self.flash = Some((
                Tone::Warning,
                crate::ui::tr(cx)
                    .providers()
                    .prov_sniff_need_url_or_preset()
                    .into(),
            ));
            self.url_input
                .update(cx, |s, cx| s.focus_handle(cx).focus(window, cx));
            cx.notify();
            return;
        }
        let key_raw = self.key_input.read(cx).value().trim().to_owned();
        let models_url = self.adv_models_url.read(cx).value().trim().to_owned();
        let proxy_url = self.adv_proxy_url.read(cx).value().trim().to_owned();

        let mut input = SniffInput::new(base_url);
        input.api_key = (!key_raw.is_empty()).then_some(key_raw.clone());
        input.preset_id.clone_from(&self.selected_preset_id);
        input.region_id.clone_from(&self.selected_region_id);
        input.provider_id.clone_from(&self.resniff_provider_id);
        input.models_url = (!models_url.is_empty()).then_some(models_url);
        input.proxy_url = (!proxy_url.is_empty()).then_some(proxy_url);
        input.extended = self.extended_scan;
        input.allow_billable_probe = self.allow_billable;
        input.confirm_private = true;
        input.confirm_key_send = true;

        let token = format!("sniff-{}", super::format::now_ms());
        self.step = 2;
        self.scanning = true;
        self.cancel_token = Some(token.clone());
        self.live_rows.clear();
        self.report = None;
        self.duplicate = None;
        self.flash = None;
        cx.notify();

        let (sink, rx) = ChannelSink::<SniffEvent>::pair();
        self._stream_task = Some(subscribe_stream_into(cx, rx, 16, |this, batch, cx| {
            for ev in batch {
                match ev {
                    SniffEvent::Planned { rows } => {
                        this.live_rows = rows
                            .into_iter()
                            .map(|r| LiveRow {
                                id: r.id,
                                label: format!("{} {} ({})", r.method, r.path, r.label),
                                status: None,
                                latency_ms: None,
                                note: String::new(),
                                done: false,
                            })
                            .collect();
                    }
                    SniffEvent::RequestFinished {
                        id,
                        status,
                        latency_ms,
                        note,
                    } => {
                        if let Some(row) = this.live_rows.iter_mut().find(|r| r.id == id) {
                            row.status = status;
                            row.latency_ms = Some(latency_ms);
                            row.note = note;
                            row.done = true;
                        }
                    }
                    _ => {}
                }
            }
            cx.notify();
        }));

        let _ = window;
        let api_key_for_dup = (!key_raw.is_empty()).then_some(key_raw);
        let is_resniff = self.resniff_provider_id.is_some();
        let task = spawn_async_into(
            cx,
            move |state| async move {
                let res = llm_provider_sniff_impl(&state, input, Some(token), &sink).await;
                let dup = match (&res, is_resniff) {
                    (Ok(rep), false) => llm_provider_duplicate_check_impl(
                        &state,
                        &rep.report_id,
                        api_key_for_dup.as_deref(),
                    )
                    .ok(),
                    _ => None,
                };
                (res, dup)
            },
            |this, (res, dup), cx| {
                this.scanning = false;
                this.cancel_token = None;
                this._stream_task = None;
                match res {
                    Ok(rep) => {
                        this.checked_protocols = rep
                            .endpoints
                            .iter()
                            .filter(|e| e.route.is_supported())
                            .map(|e| e.protocol)
                            .collect();
                        this.primary_protocol = rep
                            .endpoints
                            .iter()
                            .find(|e| e.is_default && e.route.is_supported())
                            .or_else(|| rep.endpoints.iter().find(|e| e.route.is_supported()))
                            .map(|e| e.protocol);
                        this.checked_models = rep
                            .models
                            .iter()
                            .filter(|m| m.default_enabled)
                            .map(|m| m.id.clone())
                            .collect();
                        this.duplicate = dup;
                        this.report = Some(rep);
                    }
                    Err(err) => {
                        this.flash = Some((Tone::Danger, err.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn cancel_sniff(&mut self, cx: &mut Context<Self>) {
        if let Some(tok) = self.cancel_token.take() {
            runtime(cx).state().cancellations.cancel(&tok);
        }
        self._stream_task = None;
        self.scanning = false;
    }

    pub(super) fn close_modal(&mut self, cx: &mut Context<Self>) {
        self.cancel_sniff(cx);
        self.parent.update(cx, |p, cx| {
            p.sniff_modal = None;
            cx.notify();
        });
    }

    pub(super) fn create_preset_direct(&mut self, cx: &mut Context<Self>) {
        let Some(preset_id) = self.selected_preset_id.clone() else {
            return;
        };
        let raw_name = self.name_input.read(cx).value().trim().to_owned();
        if raw_name.is_empty() {
            self.flash = Some((
                Tone::Warning,
                crate::ui::tr(cx).providers().prov_sniff_need_name().into(),
            ));
            cx.notify();
            return;
        }
        let raw_key = self.key_input.read(cx).value().trim().to_owned();
        let input = ProviderFromPresetInput {
            preset_id,
            region_id: self.selected_region_id.clone(),
            placeholders: BTreeMap::new(),
            name: Some(raw_name),
            secret: (!raw_key.is_empty()).then_some(raw_key),
        };
        self.saving = true;
        cx.notify();
        let parent = self.parent.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| llm_provider_create_from_preset_impl(state, input),
            move |this, res, cx| {
                this.saving = false;
                match res {
                    Ok(saved) => {
                        this.flash = Some((
                            Tone::Success,
                            crate::ui::tr(cx)
                                .providers()
                                .prov_sniff_sheet_actions_added_from_preset(&saved.name),
                        ));
                        parent.update(cx, |p, cx| {
                            p.selection = super::LeftSelection::Provider(saved.id);
                            p.sniff_modal = None;
                            p.status_msg = Some((
                                Tone::Success,
                                crate::ui::tr(cx)
                                    .providers()
                                    .prov_sniff_sheet_actions_added_provider(saved.name),
                            ));
                            p.reload_all(cx);
                        });
                    }
                    Err(err) => this.flash = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(crate) fn create_unverified(&mut self, cx: &mut Context<Self>) {
        let name_raw = self.name_input.read(cx).value().trim().to_owned();
        if name_raw.is_empty() {
            self.flash = Some((
                Tone::Warning,
                crate::ui::tr(cx).providers().prov_sniff_need_name().into(),
            ));
            cx.notify();
            return;
        }
        let url = self.url_input.read(cx).value().trim().to_owned();
        if url.is_empty() {
            self.flash = Some((
                Tone::Warning,
                crate::ui::tr(cx)
                    .providers()
                    .prov_sniff_need_url_or_preset()
                    .into(),
            ));
            cx.notify();
            return;
        }
        let key_raw = self.key_input.read(cx).value().trim().to_owned();
        let preset_id = self.selected_preset_id.clone();
        self.saving = true;
        cx.notify();
        let parent = self.parent.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                llm_provider_create_unverified_impl(
                    state,
                    &url,
                    Some(Protocol::OpenaiChat),
                    Some(name_raw.as_str()),
                    UnverifiedExtras {
                        preset_id: preset_id.as_deref(),
                        api_key: (!key_raw.is_empty()).then_some(key_raw),
                    },
                )
            },
            move |this, res, cx| {
                this.saving = false;
                match res {
                    Ok(saved) => {
                        this.flash = Some((
                            Tone::Success,
                            crate::ui::tr(cx)
                                .providers()
                                .prov_sniff_sheet_actions_added_unverified_provider(&saved.name),
                        ));
                        parent.update(cx, |p, cx| {
                            p.selection = super::LeftSelection::Provider(saved.id);
                            p.sniff_modal = None;
                            p.status_msg = Some((
                                Tone::Success,
                                crate::ui::tr(cx)
                                    .providers()
                                    .prov_sniff_sheet_actions_added_provider(saved.name),
                            ));
                            p.reload_all(cx);
                        });
                    }
                    Err(err) => this.flash = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn save_from_sniff(&mut self, allow_duplicate: bool, cx: &mut Context<Self>) {
        let Some(rep) = &self.report else {
            return;
        };
        let report_id = rep.report_id.clone();
        let name = self.name_input.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.flash = Some((
                Tone::Warning,
                crate::ui::tr(cx).providers().prov_sniff_need_name().into(),
            ));
            cx.notify();
            return;
        }
        let manual_models: Vec<String> = self
            .manual_models_input
            .read(cx)
            .value()
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect();
        let selection = SniffSelection {
            name,
            primary_protocol: self.primary_protocol,
            protocols: self.checked_protocols.iter().copied().collect(),
            enabled_models: self.checked_models.iter().cloned().collect(),
            manual_models,
            key_name: None,
        };
        let key_raw = self.key_input.read(cx).value().trim().to_owned();
        let api_key = (!key_raw.is_empty()).then_some(key_raw);
        let proxy_raw = self.adv_proxy_url.read(cx).value().trim().to_owned();
        let extra = ExtraInput {
            headers: Vec::new(),
            user_agent: None,
            proxy_url: (!proxy_raw.is_empty()).then_some(proxy_raw),
        };
        let resniff_id = self.resniff_provider_id.clone();

        self.saving = true;
        self.flash = None;
        cx.notify();

        let parent = self.parent.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| match resniff_id {
                Some(pid) => llm_provider_apply_resniff_impl(state, &report_id, &pid, selection),
                None => llm_provider_create_from_sniff_impl(
                    state,
                    &report_id,
                    selection,
                    api_key,
                    extra,
                    allow_duplicate,
                ),
            },
            move |this, res, cx| {
                this.saving = false;
                match res {
                    Ok(saved) => {
                        this.flash = Some((
                            Tone::Success,
                            crate::ui::tr(cx)
                                .providers()
                                .prov_sniff_sheet_actions_saved_provider(&saved.name),
                        ));
                        parent.update(cx, |p, cx| {
                            p.selection = super::LeftSelection::Provider(saved.id);
                            p.sniff_modal = None;
                            p.status_msg = Some((
                                Tone::Success,
                                crate::ui::tr(cx)
                                    .providers()
                                    .prov_sniff_sheet_actions_added_provider(saved.name),
                            ));
                            p.reload_all(cx);
                        });
                    }
                    Err(err) => this.flash = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn add_key_to_existing(&mut self, existing_id: String, cx: &mut Context<Self>) {
        let key_raw = self.key_input.read(cx).value().trim().to_owned();
        if key_raw.is_empty() {
            return;
        }
        let parent = self.parent.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| llm_provider_add_key_from_sniff_impl(state, &existing_id, &key_raw, None),
            move |this, res, cx| {
                match res {
                    Ok(k) => {
                        this.flash = Some((
                            Tone::Success,
                            crate::ui::tr(cx)
                                .providers()
                                .prov_sniff_sheet_actions_added_key_to_existing_provider(k.name),
                        ));
                        parent.update(cx, |p, cx| {
                            p.selection = super::LeftSelection::Provider(k.provider_id);
                            p.sniff_modal = None;
                            p.reload_all(cx);
                        });
                    }
                    Err(err) => this.flash = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}

pub(super) fn format_sniff_warning(
    w: &SniffWarning,
    lang: spanbay_desktop::core::model::settings::Language,
) -> String {
    let p = crate::ui::Tr(lang).providers();
    match w {
        SniffWarning::UserinfoIgnored => {
            p.prov_sniff_sheet_actions_ignored_userinfo_in_url().into()
        }
        SniffWarning::KeyInUrlIgnored => p.prov_sniff_sheet_actions_ignored_api_key_in_url().into(),
        SniffWarning::DashboardPathIgnored => {
            p.prov_sniff_sheet_actions_trimmed_dashboard_path().into()
        }
        SniffWarning::PlainHttpPublic => p
            .prov_sniff_sheet_actions_plain_http_on_public_host()
            .into(),
        SniffWarning::RedirectIgnored { location_host } => match location_host {
            Some(h) => p.prov_sniff_redirect_host(h),
            None => p.prov_sniff_redirect_3xx().into(),
        },
        SniffWarning::BodyTruncated => p.prov_sniff_sheet_actions_response_body_truncated().into(),
        SniffWarning::ListTruncated => p
            .prov_sniff_sheet_actions_model_list_truncated_first_5()
            .into(),
        SniffWarning::RateLimited => p.prov_sniff_sheet_actions_rate_limited_during_scan().into(),
        SniffWarning::LookalikeHost => p
            .prov_sniff_sheet_actions_lookalike_host_key_not_sent()
            .into(),
        SniffWarning::VendorKeyOnOtherHost => p
            .prov_sniff_sheet_actions_vendor_key_prefix_mismatch_key()
            .into(),
        SniffWarning::PresetRouteMismatch { protocol } => {
            p.prov_sniff_sheet_actions_preset_route_differs_from_host(format!("{protocol:?}"))
        }
        SniffWarning::RegionMismatch {
            suggested_region_id,
        } => p.prov_sniff_sheet_actions_key_may_belong_to_region(suggested_region_id),
        SniffWarning::HtmlPage => p
            .prov_sniff_sheet_actions_host_returned_html_page_instead()
            .into(),
        SniffWarning::ChallengePage => p
            .prov_sniff_sheet_actions_host_returned_bot_challenge_page()
            .into(),
        SniffWarning::ScanTimeout => p
            .prov_sniff_sheet_actions_scan_timed_out_partial_results()
            .into(),
        SniffWarning::ScanCancelled => p
            .prov_sniff_sheet_actions_scan_cancelled_partial_results_kept()
            .into(),
        SniffWarning::BrandThirdParty => p
            .prov_sniff_sheet_actions_brand_inferred_from_third_party()
            .into(),
    }
}
