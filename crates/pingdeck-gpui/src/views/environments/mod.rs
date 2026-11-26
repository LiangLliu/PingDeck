//! Complete Environments & Workspace Resources module (`NavModule::Environments`).
//!
//! Unifies all four workspace-level configuration resources:
//! - **Environments**: variable & secret management, direct-to-clipboard secret copy, resolved
//!   precedence + unresolved `{{var}}` scan, side-by-side environment diff, production guards,
//!   declared secret host allowlist, duplicate, Postman JSON import/export, and 60s delete undo
//! - **Globals**: workspace-wide fallback variables with optimistic revision checks
//! - **Cookies**: searchable cookie jar editor, Secure/HttpOnly/SameSite flags, delete undo,
//!   clean expired, and Netscape `cookies.txt` / JSON import-export
//! - **Certificates**: mTLS client certificates and custom CA bundles.

pub mod actions;
pub mod actions_resources;
pub mod modals;
pub mod modals_resources;
pub mod sidebar;
pub mod state;
pub mod tab_analysis;
pub mod tab_certs;
pub mod tab_cookies;
pub mod tab_vars;

use std::cell::Cell;

use gpui_kit::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Window, div, px,
};
use pingdeck_desktop::core::app::settings_get_impl;

use crate::bridge::runtime;
use crate::ui::{
    EditorState, InputEvent, InputState, Tone, UiIcon, alert_banner, btn_icon, btn_primary,
    btn_secondary, dismissible_banner, empty_state_with_actions, h_resizable, h_row, icon_xs,
    palette, resizable_panel, subtab_strip, tab_bar_underline, tab_item, text_input, to_hsla,
    toolbar_strip, v_col,
};

pub use state::EnvironmentsView;
use state::{EnvDetailTab, EnvModal, EnvResourceTab};

impl EnvironmentsView {
    pub fn new(workspace_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let lang = settings_get_impl(runtime(cx).state()).general.language;
        let e_tr = crate::ui::tr(cx).environments();
        let env_search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(e_tr.ph_filter_env()));
        let new_env_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. staging / prod"));
        let env_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Environment name"));
        let var_filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(e_tr.ph_filter_vars()));
        let new_var_key =
            cx.new(|cx| InputState::new(window, cx).placeholder("Key (e.g. baseUrl)"));
        let new_var_val = cx.new(|cx| InputState::new(window, cx).placeholder("Value / Secret"));
        let new_var_note = cx.new(|cx| InputState::new(window, cx).placeholder("Note (optional)"));
        let edit_var_key = cx.new(|cx| InputState::new(window, cx).placeholder("Key"));
        let edit_var_val = cx.new(|cx| InputState::new(window, cx).placeholder("Value"));
        let edit_var_note = cx.new(|cx| InputState::new(window, cx).placeholder("Note"));
        let host_input = cx
            .new(|cx| InputState::new(window, cx).placeholder("api.example.com or localhost:8080"));
        let import_editor = cx.new(|cx| {
            let mut ed = EditorState::new(window, cx)
                .soft_wrap(true)
                .placeholder("Paste Postman environment JSON…");
            ed.set_highlighter("json", cx);
            ed
        });
        let cookie_search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(e_tr.ph_filter_cookies()));
        let cookie_edit_val = cx.new(|cx| InputState::new(window, cx).placeholder("Cookie value"));
        let ck_domain_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("api.example.com"));
        let ck_name_input = cx.new(|cx| InputState::new(window, cx).placeholder("session_id"));
        let ck_value_input = cx.new(|cx| InputState::new(window, cx).placeholder("value"));
        let ck_path_input = cx.new(|cx| InputState::new(window, cx).placeholder("/"));
        let cert_host_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("api.example.com or *"));
        let cert_path_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("/path/to/cert.pem"));
        let cert_key_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("/path/to/key.pem"));
        let cert_pass_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Passphrase")
                .masked(true)
        });

        let mut subs = Vec::new();
        subs.push(
            cx.subscribe(&env_name_input, |this, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) && !this.syncing {
                    this.env_dirty = true;
                    cx.notify();
                }
            }),
        );
        subs.push(
            cx.subscribe(&env_search_input, |_, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify();
                }
            }),
        );
        subs.push(
            cx.subscribe(&var_filter_input, |this, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.vars_page = 0;
                    cx.notify();
                }
            }),
        );
        subs.push(
            cx.subscribe(&cookie_search_input, |this, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.cookies_page = 0;
                    cx.notify();
                }
            }),
        );

        let mut view = Self {
            workspace_id: workspace_id.clone(),
            lang,
            resource_tab: EnvResourceTab::Environments,
            detail_tab: EnvDetailTab::Variables,
            environments: Vec::new(),
            active_env_id: None,
            pending_active_env: Cell::new(None),
            selected_env_id: None,
            env_draft_name: String::new(),
            env_draft_vars: Vec::new(),
            env_draft_confirm: false,
            env_draft_confirm_safe: false,
            env_draft_hosts: Vec::new(),
            env_draft_revision: None,
            env_dirty: false,
            env_undo: None,
            resolved_vars: Vec::new(),
            unresolved_vars: Vec::new(),
            declared_hosts: Vec::new(),
            compare_target_id: None,
            compare_include_globals: false,
            compare_rows: Vec::new(),
            globals_dto: None,
            globals_draft: Vec::new(),
            globals_dirty: false,
            cookies: Vec::new(),
            selected_cookie_id: None,
            deleted_cookies_undo: None,
            cookies_page: 0,
            vars_page: 0,
            certificates: Vec::new(),
            env_search_input,
            new_env_name_input,
            env_name_input,
            var_filter_input,
            new_var_key,
            new_var_val,
            new_var_note,
            new_var_secret: false,
            edit_var_key,
            edit_var_val,
            edit_var_note,
            host_input,
            import_editor,
            cookie_search_input,
            cookie_edit_val,
            ck_domain_input,
            ck_name_input,
            ck_value_input,
            ck_path_input,
            cert_host_input,
            cert_path_input,
            cert_key_input,
            cert_pass_input,
            active_modal: None,
            status_msg: None,
            pending_sync: false,
            syncing: false,
            _subscriptions: subs,
            _tasks: Vec::new(),
        };
        if let Some(ws) = workspace_id {
            view.reload_all(ws, cx);
        }
        view
    }

    pub fn set_workspace(&mut self, workspace_id: Option<String>, cx: &mut Context<Self>) {
        if self.workspace_id == workspace_id {
            return;
        }
        self.workspace_id = workspace_id.clone();
        self.environments.clear();
        self.selected_env_id = None;
        self.env_draft_vars.clear();
        self.env_dirty = false;
        self.env_undo = None;
        self.resolved_vars.clear();
        self.unresolved_vars.clear();
        self.declared_hosts.clear();
        self.compare_rows.clear();
        self.globals_dto = None;
        self.globals_draft.clear();
        self.globals_dirty = false;
        self.cookies.clear();
        self.selected_cookie_id = None;
        self.deleted_cookies_undo = None;
        self.cookies_page = 0;
        self.vars_page = 0;
        self.certificates.clear();
        self.active_modal = None;
        if let Some(ws) = workspace_id {
            self.reload_all(ws, cx);
        }
        cx.notify();
    }

    pub fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        if let Some(ws) = self.workspace_id.clone() {
            self.reload_all(ws, cx);
        }
    }

    pub fn dismiss_modal_if_open(&mut self, cx: &mut Context<Self>) -> bool {
        if self.active_modal.take().is_some() {
            cx.notify();
            true
        } else {
            false
        }
    }

    pub fn set_active_environment(&mut self, env_id: Option<String>, cx: &mut Context<Self>) {
        if self.active_env_id != env_id {
            self.active_env_id = env_id;
            cx.notify();
        }
    }

    #[must_use]
    pub fn take_pending_active_env(&self) -> Option<Option<String>> {
        self.pending_active_env.take()
    }
}

impl Render for EnvironmentsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.lang = crate::ui::active_language(cx);
        if self.pending_sync {
            self.sync_inputs(window, cx);
        }
        let p = palette(cx);

        let left = sidebar::render_env_sidebar(self, cx);

        let mut banners = v_col().w_full().gap(px(6.0));
        if let Some((_, deleted_name)) = &self.env_undo {
            banners = banners.child(
                div().px(px(12.0)).pt(px(8.0)).child(alert_banner(
                    Tone::Warning,
                    crate::ui::tr(cx)
                        .environments()
                        .environments_deleted_environment_undo_available_for(deleted_name),
                    Some(
                        btn_secondary(
                            "env-undo-del",
                            crate::ui::Tr(self.lang).environments().environments_undo(),
                        )
                        .icon(icon_xs(UiIcon::RotateCcw))
                        .on_click(cx.listener(|this, _, _, cx| this.undo_delete_env(cx)))
                        .into_any_element(),
                    ),
                    cx,
                )),
            );
        }
        if let Some((tone, msg)) = &self.status_msg {
            banners = banners.child(div().px(px(12.0)).pt(px(8.0)).child(dismissible_banner(
                "env-banner-close",
                *tone,
                msg.clone(),
                cx.listener(|this, _, _, cx| {
                    this.status_msg = None;
                    cx.notify();
                }),
                cx,
            )));
        }

        let main_body = match self.resource_tab {
            EnvResourceTab::Environments => self.render_environment_detail(cx),
            EnvResourceTab::Globals => tab_vars::render_variables_panel(self, true, cx),
            EnvResourceTab::Cookies => tab_cookies::render_cookies_tab(self, cx),
            EnvResourceTab::Certificates => tab_certs::render_certs_tab(self, cx),
        };

        let right = v_col()
            .size_full()
            .min_h_0()
            .child(banners)
            .child(div().flex_1().min_h_0().w_full().child(main_body));

        let mut root = div().relative().size_full().bg(to_hsla(p.surface)).child(
            h_resizable("env-split")
                .child(
                    resizable_panel()
                        .size(px(250.0))
                        .size_range(px(205.0)..px(360.0))
                        .child(left),
                )
                .child(resizable_panel().child(right)),
        );

        if let Some(modal) = modals::render_modal(self, window, cx) {
            root = root.child(modal);
        }
        root
    }
}

impl EnvironmentsView {
    fn render_environment_detail(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let right = if let Some(env_id) = self.selected_env_id.clone() {
            let env_name = self.env_draft_name.clone();
            let env_id_exp = env_id.clone();
            let env_name_exp = env_name.clone();
            let env_id_act = env_id.clone();
            let is_active_env = self.active_env_id.as_deref() == Some(env_id.as_str());

            let header = toolbar_strip(cx)
                .justify_between()
                .child(div().w(px(260.0)).child(text_input(&self.env_name_input)))
                .child(
                    h_row()
                        .gap(px(6.0))
                        .flex_shrink_0()
                        .child(if is_active_env {
                            btn_secondary(
                                "env-active-btn",
                                crate::ui::Tr(self.lang)
                                    .environments()
                                    .environments_active(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.activate_env(None, cx);
                            }))
                        } else {
                            btn_secondary(
                                "env-active-btn",
                                crate::ui::Tr(self.lang)
                                    .environments()
                                    .environments_set_active(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.activate_env(Some(env_id_act.clone()), cx);
                                },
                            ))
                        })
                        .child(if self.env_dirty {
                            btn_primary("env-save-btn", crate::ui::Tr(self.lang).common().save())
                                .icon(icon_xs(UiIcon::Check))
                                .on_click(cx.listener(|this, _, _, cx| this.save_env(cx)))
                        } else {
                            btn_secondary(
                                "env-save-btn",
                                crate::ui::Tr(self.lang).common().save_saved(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.save_env(cx)))
                        })
                        .child(
                            btn_icon("env-dup-btn", UiIcon::Copy)
                                .tooltip(
                                    crate::ui::Tr(self.lang)
                                        .environments()
                                        .environments_duplicate_environment(),
                                )
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.duplicate_selected_env(cx)),
                                ),
                        )
                        .child(
                            btn_icon("env-exp-btn", UiIcon::Download)
                                .tooltip(
                                    crate::ui::Tr(self.lang)
                                        .environments()
                                        .environments_export_environment_json(),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.active_modal = Some(EnvModal::ExportEnv {
                                        id: env_id_exp.clone(),
                                        name: env_name_exp.clone(),
                                        include_secrets: false,
                                        ack_secrets: false,
                                    });
                                    cx.notify();
                                })),
                        )
                        .child(
                            btn_icon("env-del-btn", UiIcon::Trash)
                                .tooltip(
                                    crate::ui::Tr(self.lang)
                                        .environments()
                                        .environments_delete_environment(),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.active_modal = Some(EnvModal::DeleteEnv {
                                        id: env_id.clone(),
                                        name: env_name.clone(),
                                    });
                                    cx.notify();
                                })),
                        ),
                );

            let mut dtabs = tab_bar_underline("env-detail-tabs")
                .selected_index(self.detail_tab.index())
                .on_click(cx.listener(|this, idx: &usize, _, cx| {
                    this.detail_tab = EnvDetailTab::from_index(*idx);
                    if this.detail_tab != EnvDetailTab::Variables {
                        this.refresh_env_analysis(cx);
                    }
                    cx.notify();
                }));
            for t in EnvDetailTab::ALL {
                let lbl = if t == EnvDetailTab::Resolved && !self.unresolved_vars.is_empty() {
                    SharedString::from(format!(
                        "{} ({})",
                        t.label(self.lang),
                        self.unresolved_vars.len()
                    ))
                } else {
                    SharedString::from(t.label(self.lang))
                };
                dtabs = dtabs.child(tab_item(lbl));
            }

            let detail_body = match self.detail_tab {
                EnvDetailTab::Variables => tab_vars::render_variables_panel(self, false, cx),
                EnvDetailTab::Resolved => tab_analysis::render_resolved_tab(self, cx),
                EnvDetailTab::Compare => tab_analysis::render_compare_tab(self, cx),
                EnvDetailTab::Protection => tab_analysis::render_protection_tab(self, cx),
            };

            v_col()
                .size_full()
                .min_h_0()
                .child(header)
                .child(subtab_strip(dtabs, None, cx))
                .child(div().flex_1().min_h_0().w_full().child(detail_body))
        } else {
            v_col().size_full().child(empty_state_with_actions(
                UiIcon::Layers,
                crate::ui::Tr(self.lang)
                    .environments()
                    .environments_no_environment_selected(),
                crate::ui::Tr(self.lang)
                    .environments()
                    .environments_create_environments_to_isolate_base(),
                Some(
                    btn_primary(
                        "env-empty-new",
                        crate::ui::Tr(self.lang)
                            .environments()
                            .environments_new_environment(),
                    )
                    .icon(icon_xs(UiIcon::Plus))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_create_env_modal(window, cx);
                    }))
                    .into_any_element(),
                ),
                cx,
            ))
        };

        right.into_any_element()
    }
}
