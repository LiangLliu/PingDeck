//! Centered modal wizard (`ui::modal_dialog`) for connecting local AI coding tools and manual
//! snippet clients, matching `docs/design/prototype/js/data-overlays.js` (`connect-client`).

use std::collections::HashSet;

use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Task, Window, div, px,
};
use spanbay_desktop::core::app::{
    ClientConnectInput, ClientConnectResult, ClientPlanInput, OnExisting, SnippetSet, VerifyResult,
    llm_client_connect_impl, llm_client_preview_impl, llm_client_restore_impl,
    llm_client_snippets_impl, llm_client_verify_impl, llm_gateway_client_create_impl,
};
use spanbay_desktop::core::client_config::{ClientApp, ClientConfigPlan, SnippetClient};
use spanbay_desktop::core::llm::store::ClientInput;

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::{
    Anchor, InputState, ScrollableElement as _, Tone, alert_banner, badge, btn_secondary, h_row,
    list_row, modal_dialog, palette, popover, select_pill, to_hsla, v_col,
};

use super::connect_sheet_body::{
    render_advanced_section, render_connect_details, render_modal_footer, render_tool_picker,
};
use super::{GatewayHandoff, GatewayView};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectToolKind {
    File(ClientApp),
    Snippet(SnippetClient),
}

impl ConnectToolKind {
    #[must_use]
    pub const fn label(
        self,
        lang: spanbay_desktop::core::model::settings::Language,
    ) -> &'static str {
        match self {
            Self::File(app) => app.display_name(),
            Self::Snippet(SnippetClient::Cursor) => "Cursor",
            Self::Snippet(SnippetClient::Zed) => "Zed",
            Self::Snippet(SnippetClient::CherryStudio) => "Cherry Studio",
            Self::Snippet(SnippetClient::GenericEnv) => crate::ui::Tr(lang)
                .providers()
                .prov_connect_sheet_env_vars(),
        }
    }
}

pub struct ConnectClientSheet {
    parent: Entity<GatewayView>,
    gateway_url: String,
    pub tool: ConnectToolKind,
    pub installed_apps: HashSet<ClientApp>,
    pub installed_snippets: HashSet<SnippetClient>,
    /// `(canonical "provider_id/model", display "provider_name/model")`.
    pub available_models: Vec<(String, String)>,
    selected_model: Option<String>,
    fast_model: Option<String>,
    pub client_name_input: Entity<InputState>,
    pub unknown_to_default: bool,
    pub on_existing: OnExisting,
    pub show_advanced: bool,
    pub plan: Option<ClientConfigPlan>,
    plan_error: Option<String>,
    pub snippets: Option<SnippetSet>,
    pub revealed_token: Option<String>,
    pub connect_result: Option<ClientConnectResult>,
    pub verify_result: Option<VerifyResult>,
    pub busy: bool,
    pub flash: Option<(Tone, String)>,
    _tasks: Vec<Task<()>>,
}

impl ConnectClientSheet {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        parent: Entity<GatewayView>,
        initial_tool: ConnectToolKind,
        gateway_url: String,
        available_models: Vec<(String, String)>,
        installed_apps: HashSet<ClientApp>,
        installed_snippets: HashSet<SnippetClient>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let default_name = initial_tool
            .label(crate::ui::active_language(cx))
            .to_owned();
        let selected_model = available_models.first().map(|(id, _)| id.clone());
        let client_name_input = cx.new(|cx| {
            let mut st = InputState::new(window, cx).placeholder("Client name…");
            st.set_value(default_name, window, cx);
            st
        });
        let mut sheet = Self {
            parent,
            gateway_url,
            tool: initial_tool,
            installed_apps,
            installed_snippets,
            available_models,
            selected_model,
            fast_model: None,
            client_name_input,
            unknown_to_default: false,
            on_existing: OnExisting::Rotate,
            show_advanced: false,
            plan: None,
            plan_error: None,
            snippets: None,
            revealed_token: None,
            connect_result: None,
            verify_result: None,
            busy: false,
            flash: None,
            _tasks: Vec::new(),
        };
        sheet.refresh_preview(cx);
        sheet
    }

    pub fn close_modal(&self, cx: &mut Context<Self>) {
        self.parent.update(cx, |p, cx| {
            p.connect_modal = None;
            cx.notify();
        });
    }

    pub fn select_tool(
        &mut self,
        tool: ConnectToolKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tool = tool;
        self.connect_result = None;
        self.verify_result = None;
        self.revealed_token = None;
        self.flash = None;
        let lbl = tool.label(crate::ui::active_language(cx)).to_owned();
        self.client_name_input
            .update(cx, |i, cx| i.set_value(lbl, window, cx));
        self.refresh_preview(cx);
    }

    fn refresh_preview(&mut self, cx: &mut Context<Self>) {
        self.plan_error = None;
        match self.tool {
            ConnectToolKind::File(app) => {
                let alias = self.selected_model.clone();
                let fast_alias = self.fast_model.clone();
                let task = spawn_blocking_into(
                    cx,
                    move |state| {
                        llm_client_preview_impl(
                            state,
                            ClientPlanInput {
                                app,
                                client_id: None,
                                alias,
                                fast_alias,
                            },
                        )
                    },
                    |this, res, cx| {
                        match res {
                            Ok(plan) => {
                                this.plan = Some(plan);
                                this.plan_error = None;
                            }
                            Err(err) => {
                                this.plan = None;
                                this.plan_error = Some(err.message().to_owned());
                            }
                        }
                        cx.notify();
                    },
                );
                self._tasks.push(task);
            }
            ConnectToolKind::Snippet(sc) => {
                let addr = self.gateway_url.clone();
                let alias = self
                    .selected_model
                    .clone()
                    .unwrap_or_else(|| "provider/model".into());
                let revealed = self.revealed_token.is_some();
                let task = spawn_blocking_into(
                    cx,
                    move |state| llm_client_snippets_impl(state, sc, &addr, &alias, revealed),
                    |this, res, cx| {
                        match res {
                            Ok(set) => {
                                this.snippets = Some(set);
                                this.plan_error = None;
                            }
                            Err(err) => {
                                this.snippets = None;
                                this.plan_error = Some(err.message().to_owned());
                            }
                        }
                        cx.notify();
                    },
                );
                self._tasks.push(task);
            }
        }
    }

    pub fn apply_connect(&mut self, cx: &mut Context<Self>) {
        let ConnectToolKind::File(app) = self.tool else {
            return;
        };
        let name = self.client_name_input.read(cx).value().trim().to_owned();
        let input = ClientConnectInput {
            app,
            alias: self.selected_model.clone(),
            fast_alias: self.fast_model.clone(),
            client_name: (!name.is_empty()).then_some(name),
            on_existing: self.on_existing,
            reviewed: self.plan.clone(),
            extend_allowlist: Some(true),
            unknown_model_to_default: Some(self.unknown_to_default),
        };
        self.busy = true;
        self.flash = None;
        cx.notify();

        let parent = self.parent.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| llm_client_connect_impl(state, input),
            move |this, res, cx| {
                this.busy = false;
                match res {
                    Ok(done) => {
                        this.flash = Some((
                            Tone::Success,
                            crate::ui::tr(cx)
                                .providers()
                                .prov_connect_sheet_configured_and_backed_up(&done.client.name),
                        ));
                        this.connect_result = Some(done);
                        parent.update(cx, |p, cx| p.reload_all(cx));
                    }
                    Err(err) => this.flash = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn mint_snippet_token(&mut self, cx: &mut Context<Self>) {
        let raw_name = self.client_name_input.read(cx).value().trim().to_owned();
        let name = if raw_name.is_empty() {
            self.tool.label(crate::ui::active_language(cx)).to_owned()
        } else {
            raw_name
        };
        let allowed_models: Vec<String> = self.selected_model.clone().into_iter().collect();
        let allow_all = allowed_models.is_empty();
        let parent = self.parent.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                llm_gateway_client_create_impl(
                    state,
                    ClientInput {
                        name,
                        allow_all,
                        allowed_aliases: Vec::new(),
                        allowed_models,
                    },
                )
            },
            move |this, res, cx| {
                match res {
                    Ok(created) => {
                        this.revealed_token = Some(created.token);
                        this.flash = Some((
                            Tone::Success,
                            crate::ui::tr(cx)
                                .providers()
                                .prov_connect_sheet_token_minted_and_spliced_into()
                                .into(),
                        ));
                        this.refresh_preview(cx);
                        parent.update(cx, |p, cx| p.reload_all(cx));
                    }
                    Err(err) => this.flash = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn run_verify(&mut self, cx: &mut Context<Self>) {
        let (ConnectToolKind::File(app), Some(res)) = (self.tool, &self.connect_result) else {
            return;
        };
        let cid = res.client.id.clone();
        self.busy = true;
        cx.notify();
        let task = spawn_async_into(
            cx,
            move |state| async move { llm_client_verify_impl(&state, &cid, app).await },
            |this, res, cx| {
                this.busy = false;
                match res {
                    Ok(v) => this.verify_result = Some(v),
                    Err(err) => this.flash = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn restore_prev(&mut self, cx: &mut Context<Self>) {
        let ConnectToolKind::File(app) = self.tool else {
            return;
        };
        let parent = self.parent.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| llm_client_restore_impl(state, app, false),
            move |this, res, cx| {
                match res {
                    Ok(r) => {
                        this.flash = Some((
                            Tone::Success,
                            crate::ui::tr(cx)
                                .providers()
                                .prov_connect_sheet_restored_file_s(r.files.len()),
                        ));
                        this.connect_result = None;
                        this.verify_result = None;
                        this.refresh_preview(cx);
                        parent.update(cx, |p, cx| p.reload_all(cx));
                    }
                    Err(err) => this.flash = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    fn target_display_path(&self) -> String {
        match self.tool {
            ConnectToolKind::File(app) => self
                .plan
                .as_ref()
                .and_then(|pl| pl.files.first().map(|f| f.path.clone()))
                .unwrap_or_else(|| match app {
                    ClientApp::ClaudeCode => "~/.claude/settings.json".to_owned(),
                    ClientApp::Codex => "~/.codex/config.toml".to_owned(),
                    ClientApp::OpenCode => "~/.config/opencode/opencode.json".to_owned(),
                    ClientApp::GeminiCli => "~/.gemini/settings.json".to_owned(),
                }),
            ConnectToolKind::Snippet(_) => format!("{}/v1", self.gateway_url),
        }
    }
}

impl Render for ConnectClientSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let pr = crate::ui::tr(cx).providers();

        let ent_m = cx.entity().clone();
        let models_list = self.available_models.clone();
        let cur_model = self
            .selected_model
            .as_deref()
            .map(|id| {
                self.available_models
                    .iter()
                    .find(|(k, _)| k == id)
                    .map_or_else(|| id.to_owned(), |(_, d)| d.clone())
            })
            .unwrap_or_else(|| pr.prov_connect_sheet_pick_default_model().into());
        let model_picker = popover("conn-model-pop")
            .anchor(Anchor::TopLeft)
            .trigger(select_pill("conn-model-trig", cur_model, false, cx).w_full())
            .content(move |_, _, cx| {
                let list_h = ((models_list.len().min(60) as f32) * 30.0 + 12.0).clamp(56.0, 280.0);
                let mut col = v_col().w(px(300.0)).gap(px(2.0)).p(px(6.0));
                if models_list.is_empty() {
                    col = col.child(
                        div().p(px(8.0)).text_xs().child(
                            crate::ui::tr(cx)
                                .providers()
                                .prov_connect_sheet_enable_at_least_one_chat(),
                        ),
                    );
                } else {
                    for (i, (m_id, m_disp)) in models_list.iter().take(60).enumerate() {
                        let m_val = m_id.clone();
                        let ent = ent_m.clone();
                        let pop_h = cx.entity().clone();
                        col = col.child(
                            list_row(
                                SharedString::from(format!("conn-m-{i}")),
                                false,
                                cx,
                                move |_, window, cx| {
                                    ent.update(cx, |this, cx| {
                                        this.selected_model = Some(m_val.clone());
                                        this.refresh_preview(cx);
                                    });
                                    pop_h.update(cx, |s, cx| s.dismiss(window, cx));
                                },
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .child(SharedString::from(m_disp.clone())),
                            ),
                        );
                    }
                }
                div()
                    .w(px(300.0))
                    .h(px(list_h))
                    .overflow_y_scrollbar()
                    .id("conn-model-pop-scroll")
                    .child(col)
            });

        let target_label = match self.tool {
            ConnectToolKind::File(_) => pr.prov_connect_modal_target_file(),
            ConnectToolKind::Snippet(_) => pr.prov_connect_modal_gateway_endpoint(),
        };
        let core_grid = h_row()
            .w_full()
            .gap(px(12.0))
            .items_start()
            .child(
                v_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.muted))
                            .child(target_label),
                    )
                    .child(
                        div()
                            .w_full()
                            .px(px(10.0))
                            .py(px(6.5))
                            .rounded(px(7.0))
                            .bg(to_hsla(p.sunken))
                            .border_1()
                            .border_color(to_hsla(p.line.composite_over(p.sunken)))
                            .text_xs()
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(SharedString::from(self.target_display_path())),
                    ),
            )
            .child(
                v_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.muted))
                            .child(pr.prov_connect_sheet_default_model()),
                    )
                    .child(model_picker)
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(to_hsla(p.faint))
                            .child(pr.prov_connect_default_model_desc()),
                    ),
            );

        let no_models_banner = self.available_models.is_empty().then(|| {
            alert_banner(
                Tone::Warning,
                pr.prov_connect_modal_no_models_hint(),
                Some(
                    btn_secondary("conn-go-providers", pr.prov_gateway_add_provider_btn())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.parent.update(cx, |p, cx| {
                                p.connect_modal = None;
                                p.pending_handoff.set(Some(GatewayHandoff::OpenProviders));
                                cx.notify();
                            });
                        }))
                        .into_any_element(),
                ),
                cx,
            )
        });

        let mut body_col = v_col()
            .w_full()
            .gap(px(12.0))
            .child(render_tool_picker(self, cx))
            .children(no_models_banner)
            .child(core_grid)
            .child(render_advanced_section(self, cx));

        if let Some((tone, msg)) = &self.flash {
            body_col = body_col.child(badge(msg.clone(), *tone, cx));
        }
        if let Some(err) = &self.plan_error {
            body_col = body_col.child(badge(err.clone(), Tone::Danger, cx));
        }
        body_col = body_col.child(render_connect_details(self, cx));

        let vp = window.viewport_size();
        let (_, max_card_h) =
            crate::ui::responsive_modal_bounds(760.0, f32::from(vp.width), f32::from(vp.height));
        let body_max_h = (max_card_h - 144.0).max(240.0);
        let scrollable_body = div()
            .id("conn-modal-scroll")
            .w_full()
            .max_h(px(body_max_h))
            .overflow_y_scrollbar()
            .child(body_col)
            .into_any_element();

        let footer = render_modal_footer(self, cx);
        let ent_close = cx.entity().clone();
        modal_dialog(
            "gw-connect-modal",
            pr.prov_loader_connect_local_ai_coding_tool(),
            760.0,
            scrollable_body,
            footer,
            window,
            cx,
            move |_, _, cx| {
                ent_close.update(cx, |this, cx| this.close_modal(cx));
            },
        )
    }
}
