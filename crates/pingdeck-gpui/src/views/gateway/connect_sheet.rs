//! Native right-hand Slide-Over Sheet for connecting local AI coding tools.

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Task, Window, div, px,
};
use pingdeck_desktop::core::app::{
    ClientConnectInput, ClientConnectResult, ClientPlanInput, OnExisting, SnippetSet, VerifyResult,
    llm_client_connect_impl, llm_client_preview_impl, llm_client_restore_impl,
    llm_client_snippets_impl, llm_client_verify_impl, llm_gateway_client_create_impl,
};
use pingdeck_desktop::core::client_config::{ClientApp, ClientConfigPlan, SnippetClient};
use pingdeck_desktop::core::llm::store::ClientInput;

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::{
    Anchor, InputState, ScrollableElement as _, Tone, badge, card, field_row, filter_chip, h_row,
    list_row, popover, section_heading, select_pill, tab_bar_segmented, tab_item, text_input,
    toggle_switch, v_col,
};

use super::GatewayView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectToolKind {
    File(ClientApp),
    Snippet(SnippetClient),
}

impl ConnectToolKind {
    #[must_use]
    pub const fn label(
        self,
        lang: pingdeck_desktop::core::model::settings::Language,
    ) -> &'static str {
        match self {
            Self::File(app) => app.display_name(),
            Self::Snippet(SnippetClient::Zed) => "Zed",
            Self::Snippet(SnippetClient::Cursor) => "Cursor",
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
    /// `(canonical "provider_id/model", display "provider_name/model")`.
    available_models: Vec<(String, String)>,
    selected_model: Option<String>,
    fast_model: Option<String>,
    client_name_input: Entity<InputState>,
    unknown_to_default: bool,
    on_existing: OnExisting,
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
    pub fn new(
        parent: Entity<GatewayView>,
        initial_tool: ConnectToolKind,
        gateway_url: String,
        available_models: Vec<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tool = initial_tool;
        let default_name = tool.label(crate::ui::active_language(cx)).to_owned();
        let selected_model = available_models.first().map(|(id, _)| id.clone());
        let client_name_input = cx.new(|cx| {
            let mut st = InputState::new(window, cx).placeholder("Client name…");
            st.set_value(default_name, window, cx);
            st
        });
        let mut sheet = Self {
            parent,
            gateway_url,
            tool,
            available_models,
            selected_model,
            fast_model: None,
            client_name_input,
            unknown_to_default: false,
            on_existing: OnExisting::Rotate,
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

    fn select_tool(&mut self, tool: ConnectToolKind, window: &mut Window, cx: &mut Context<Self>) {
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
}

impl Render for ConnectClientSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let all_tools = [
            ConnectToolKind::File(ClientApp::ClaudeCode),
            ConnectToolKind::File(ClientApp::Codex),
            ConnectToolKind::File(ClientApp::OpenCode),
            ConnectToolKind::File(ClientApp::GeminiCli),
            ConnectToolKind::Snippet(SnippetClient::Zed),
            ConnectToolKind::Snippet(SnippetClient::Cursor),
            ConnectToolKind::Snippet(SnippetClient::CherryStudio),
            ConnectToolKind::Snippet(SnippetClient::GenericEnv),
        ];
        let mut tool_bar = h_row().flex_wrap().gap(px(5.0));
        for (i, kind) in all_tools.into_iter().enumerate() {
            tool_bar = tool_bar.child(
                filter_chip(
                    SharedString::from(format!("conn-tool-{i}")),
                    kind.label(crate::ui::active_language(cx)),
                    None,
                    self.tool == kind,
                    cx,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_tool(kind, window, cx);
                })),
            );
        }

        let ent_m = cx.entity().clone();
        let models_list = self.available_models.clone();
        let disp_for = |id: &str| {
            self.available_models
                .iter()
                .find(|(k, _)| k == id)
                .map_or_else(|| id.to_owned(), |(_, d)| d.clone())
        };
        let cur_model = self
            .selected_model
            .as_deref()
            .map(disp_for)
            .unwrap_or_else(|| {
                crate::ui::tr(cx)
                    .providers()
                    .prov_connect_sheet_pick_default_model()
                    .into()
            });
        let model_picker = popover("conn-model-pop")
            .anchor(Anchor::TopLeft)
            .trigger(select_pill(
                "conn-model-trig",
                cur_model,
                self.selected_model.is_some(),
                cx,
            ))
            .content(move |_, _, cx| {
                let list_h = ((models_list.len().min(60) as f32) * 30.0 + 12.0).clamp(56.0, 300.0);
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

        let ent_ex = cx.entity().clone();
        let ex_idx = match self.on_existing {
            OnExisting::Rotate | OnExisting::Fail => 0,
            OnExisting::SecondClient => 1,
            OnExisting::ReplaceRevoked => 2,
        };
        let on_existing_seg = tab_bar_segmented("conn-on-existing")
            .selected_index(ex_idx)
            .child(tab_item(
                crate::ui::tr(cx).providers().prov_connect_sheet_rotate(),
            ))
            .child(tab_item(
                crate::ui::tr(cx).providers().prov_connect_sheet_add_2nd(),
            ))
            .child(tab_item(
                crate::ui::tr(cx).providers().prov_connect_sheet_replace(),
            ))
            .on_click(move |idx, _, cx| {
                let next = match *idx {
                    1 => OnExisting::SecondClient,
                    2 => OnExisting::ReplaceRevoked,
                    _ => OnExisting::Rotate,
                };
                ent_ex.update(cx, |this, cx| {
                    this.on_existing = next;
                    cx.notify();
                });
            });

        let mut body = v_col()
            .size_full()
            .gap(px(14.0))
            .overflow_y_scrollbar()
            .child(
                v_col()
                    .gap(px(6.0))
                    .child(section_heading(
                        crate::ui::tr(cx)
                            .providers()
                            .prov_connect_sheet_1_choose_tool(),
                        cx,
                    ))
                    .child(tool_bar),
            )
            .child(
                card(cx).p(px(12.0)).child(
                    v_col()
                        .gap(px(10.0))
                        .child(field_row(
                            crate::ui::tr(cx)
                                .providers()
                                .prov_connect_sheet_default_model(),
                            Some(SharedString::from(
                                crate::ui::tr(cx)
                                    .providers()
                                    .prov_connect_default_model_desc(),
                            )),
                            model_picker,
                            cx,
                        ))
                        .child(field_row(
                            crate::ui::tr(cx).providers().connect_name(),
                            None,
                            div()
                                .w(px(210.0))
                                .child(text_input(&self.client_name_input)),
                            cx,
                        ))
                        .children(matches!(self.tool, ConnectToolKind::File(_)).then(|| {
                            field_row(
                                crate::ui::tr(cx)
                                    .providers()
                                    .prov_connect_sheet_if_already_connected(),
                                None,
                                on_existing_seg,
                                cx,
                            )
                        }))
                        .children(matches!(self.tool, ConnectToolKind::File(_)).then(|| {
                            field_row(
                                crate::ui::tr(cx)
                                    .providers()
                                    .prov_connect_sheet_map_unknown_model_names(),
                                Some(SharedString::from(
                                    crate::ui::tr(cx)
                                        .providers()
                                        .prov_connect_unknown_model_desc(),
                                )),
                                toggle_switch("conn-unknown-sw", self.unknown_to_default).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.unknown_to_default = !this.unknown_to_default;
                                        cx.notify();
                                    }),
                                ),
                                cx,
                            )
                        })),
                ),
            );

        if let Some((tone, msg)) = &self.flash {
            body = body.child(badge(msg.clone(), *tone, cx));
        }
        if let Some(err) = &self.plan_error {
            body = body.child(badge(err.clone(), Tone::Danger, cx));
        }

        body.child(super::connect_sheet_body::render_connect_details(self, cx))
    }
}
