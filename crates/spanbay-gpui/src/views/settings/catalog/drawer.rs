//! Stateful GPUI entity mounted inside `gpui-component`'s native slide-over `Sheet` for:
//! 1. Editing an existing catalog model's pricing, distributor offer, and provider overrides;
//! 2. Creating a custom user model (`UserModelInput`);
//! 3. Binding an unlisted provider model to a catalog alias or setting a direct price.

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Task, Window, div, px,
};
use spanbay_desktop::core::app::{
    ModelPriceDetail, UnlistedModel, llm_model_catalog_list_impl, llm_model_price_detail_impl,
};
use spanbay_desktop::core::model::model_catalog::{
    CatalogKind, CatalogSource, DEFAULT_CURRENCY, ModelCatalogQuery, ModelCatalogRow, PricingMode,
};

use crate::bridge::spawn_async_into;
use crate::ui::{
    InputEvent, InputState, ScrollableElement as _, StatusShape, WindowExt as _, h_row, palette,
    section_heading, status_indicator, to_hsla, v_col,
};

use super::CatalogSettingsView;
use super::drawer_forms::{
    render_model_facts, render_pricing_editor_fields, render_provider_overrides_list,
};
use super::drawer_forms_extra::{
    render_add_user_model_body, render_drawer_footer, render_offers_table, render_unlisted_body,
};
use super::format::opt_f64_input;

pub enum DrawerKind {
    ModelDetail {
        row: Box<ModelCatalogRow>,
        detail: Option<Box<ModelPriceDetail>>,
    },
    AddUserModel,
    Unlisted {
        item: Box<UnlistedModel>,
    },
}

pub struct CatalogDrawer {
    pub(super) parent: Entity<CatalogSettingsView>,
    pub kind: DrawerKind,
    pub pricing_mode: PricingMode,
    pub currency: String,
    pub selected_catalog_family: Option<String>,
    pub in_price: Entity<InputState>,
    pub out_price: Entity<InputState>,
    pub cache_read_price: Entity<InputState>,
    pub cache_write_price: Entity<InputState>,
    pub reasoning_price: Entity<InputState>,
    pub per_call_price: Entity<InputState>,
    pub user_family: Entity<InputState>,
    pub user_model_id: Entity<InputState>,
    pub user_name: Entity<InputState>,
    pub user_context: Entity<InputState>,
    pub user_max_output: Entity<InputState>,
    pub user_kind: CatalogKind,
    pub user_reasoning: bool,
    pub user_vision: bool,
    pub user_tools: bool,
    pub alias_search: Entity<InputState>,
    pub alias_candidates: Vec<ModelCatalogRow>,
    pub selected_alias_key: Option<String>,
    /// When set inside `ModelDetail`, edits a per-provider price override `(provider_id, provider_name, model_id)`.
    pub editing_provider_override: Option<(String, String, String)>,
    pub busy: bool,
    pub error_msg: Option<String>,
    pub(super) pending_close: bool,
    _subscriptions: Vec<Subscription>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl CatalogDrawer {
    fn base(
        parent: Entity<CatalogSettingsView>,
        kind: DrawerKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let make_input = |ph: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(ph))
        };
        let in_price = make_input("0.00", window, cx);
        let out_price = make_input("0.00", window, cx);
        let cache_read_price = make_input("optional", window, cx);
        let cache_write_price = make_input("optional", window, cx);
        let reasoning_price = make_input("optional", window, cx);
        let per_call_price = make_input("0.00", window, cx);
        let user_family = make_input("custom", window, cx);
        let user_model_id = make_input("my-model-v1", window, cx);
        let user_name = make_input("Display name", window, cx);
        let user_context = make_input("128000", window, cx);
        let user_max_output = make_input("16384", window, cx);
        let alias_search = make_input(
            crate::ui::tr(cx).settings().catalog_search_placeholder(),
            window,
            cx,
        );

        let alias_sub = cx.subscribe(&alias_search, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change | InputEvent::PressEnter { .. }) {
                this.search_alias_candidates(cx);
            }
        });

        Self {
            parent,
            kind,
            pricing_mode: PricingMode::Dynamic,
            currency: DEFAULT_CURRENCY.to_owned(),
            selected_catalog_family: None,
            in_price,
            out_price,
            cache_read_price,
            cache_write_price,
            reasoning_price,
            per_call_price,
            user_family,
            user_model_id,
            user_name,
            user_context,
            user_max_output,
            user_kind: CatalogKind::Chat,
            user_reasoning: false,
            user_vision: false,
            user_tools: true,
            alias_search,
            alias_candidates: Vec::new(),
            selected_alias_key: None,
            editing_provider_override: None,
            busy: false,
            error_msg: None,
            pending_close: false,
            _subscriptions: vec![alias_sub],
            _tasks: Vec::new(),
        }
    }

    pub fn for_model(
        parent: Entity<CatalogSettingsView>,
        row: ModelCatalogRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let model_key = row.model_key.clone();
        let initial_pricing = row.pricing.clone();
        let in_val = initial_pricing.input_per_mtok.or(row.input_per_mtok);
        let out_val = initial_pricing.output_per_mtok.or(row.output_per_mtok);
        let cr_val = initial_pricing
            .cache_read_per_mtok
            .or(row.cache_read_per_mtok);
        let cw_val = initial_pricing
            .cache_write_per_mtok
            .or(row.cache_write_per_mtok);
        let r_val = initial_pricing
            .reasoning_per_mtok
            .or(row.reasoning_per_mtok);

        let mut drawer = Self::base(
            parent,
            DrawerKind::ModelDetail {
                row: Box::new(row),
                detail: None,
            },
            window,
            cx,
        );
        drawer.pricing_mode = initial_pricing.mode;
        drawer.currency = initial_pricing.currency;
        drawer.selected_catalog_family = initial_pricing.catalog_family;
        if let DrawerKind::ModelDetail { row, .. } = &drawer.kind
            && row.source == CatalogSource::User
        {
            drawer.user_kind = row.kind;
            drawer.user_reasoning = row.reasoning;
            drawer.user_vision = row.vision;
            drawer.user_tools = row.tools;
            let fam = row.family.clone();
            let mid = row.model_id.clone();
            let nm = row.name.clone();
            let ctx_s = row
                .context_tokens
                .map(|v| v.to_string())
                .unwrap_or_default();
            let out_s = row
                .max_output_tokens
                .map(|v| v.to_string())
                .unwrap_or_default();
            drawer
                .user_family
                .update(cx, |s, cx| s.set_value(fam, window, cx));
            drawer
                .user_model_id
                .update(cx, |s, cx| s.set_value(mid, window, cx));
            drawer
                .user_name
                .update(cx, |s, cx| s.set_value(nm, window, cx));
            drawer
                .user_context
                .update(cx, |s, cx| s.set_value(ctx_s, window, cx));
            drawer
                .user_max_output
                .update(cx, |s, cx| s.set_value(out_s, window, cx));
        }

        drawer
            .in_price
            .update(cx, |s, cx| s.set_value(opt_f64_input(in_val), window, cx));
        drawer
            .out_price
            .update(cx, |s, cx| s.set_value(opt_f64_input(out_val), window, cx));
        drawer
            .cache_read_price
            .update(cx, |s, cx| s.set_value(opt_f64_input(cr_val), window, cx));
        drawer
            .cache_write_price
            .update(cx, |s, cx| s.set_value(opt_f64_input(cw_val), window, cx));
        drawer
            .reasoning_price
            .update(cx, |s, cx| s.set_value(opt_f64_input(r_val), window, cx));
        drawer.per_call_price.update(cx, |s, cx| {
            s.set_value(opt_f64_input(initial_pricing.per_call), window, cx)
        });

        drawer.load_detail(model_key, cx);
        drawer
    }

    pub fn for_add_user_model(
        parent: Entity<CatalogSettingsView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let drawer = Self::base(parent, DrawerKind::AddUserModel, window, cx);
        drawer
            .user_family
            .update(cx, |s, cx| s.set_value("custom", window, cx));
        drawer
    }

    pub fn for_unlisted(
        parent: Entity<CatalogSettingsView>,
        item: UnlistedModel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut drawer = Self::base(
            parent,
            DrawerKind::Unlisted {
                item: Box::new(item),
            },
            window,
            cx,
        );
        drawer.search_alias_candidates(cx);
        drawer
    }

    pub fn sheet_title(&self, cx: &gpui_kit::App) -> SharedString {
        match &self.kind {
            DrawerKind::ModelDetail { row, .. } => {
                SharedString::from(crate::ui::tr(cx).settings().drawer_edit_pricing(&row.name))
            }
            DrawerKind::AddUserModel => {
                SharedString::from(crate::ui::tr(cx).settings().catalog_add())
            }
            DrawerKind::Unlisted { item } => SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .drawer_unlisted(&item.model_id, &item.provider_name),
            ),
        }
    }

    pub(super) fn load_detail(&mut self, model_key: String, cx: &mut Context<Self>) {
        let task = spawn_async_into(
            cx,
            move |state| async move { llm_model_price_detail_impl(&state, model_key).await },
            |this, res, cx| {
                match res {
                    Ok(detail) => {
                        if let DrawerKind::ModelDetail {
                            detail: ref mut slot,
                            ..
                        } = this.kind
                        {
                            *slot = Some(Box::new(detail));
                        }
                    }
                    Err(err) => {
                        this.error_msg = Some(err.message().to_owned());
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    fn search_alias_candidates(&mut self, cx: &mut Context<Self>) {
        let raw = self.alias_search.read(cx).value().trim().to_owned();
        let query = ModelCatalogQuery {
            search: (!raw.is_empty()).then_some(raw),
            limit: Some(20),
            ..ModelCatalogQuery::default()
        };
        let task = spawn_async_into(
            cx,
            move |state| async move { llm_model_catalog_list_impl(&state, query).await },
            |this, res, cx| {
                if let Ok(page) = res {
                    this.alias_candidates = page.rows;
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.close_sheet(cx);
    }

    pub fn start_edit_provider_override(
        &mut self,
        pm: &spanbay_desktop::core::app::ProviderModelPrice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editing_provider_override = Some((
            pm.provider_id.clone(),
            pm.provider_name.clone(),
            pm.model_id.clone(),
        ));
        if let Some(ov) = &pm.pricing {
            self.pricing_mode = ov.mode;
            self.currency = ov.currency.clone();
            self.selected_catalog_family = ov.catalog_family.clone();
            self.in_price.update(cx, |s, cx| {
                s.set_value(opt_f64_input(ov.input_per_mtok), window, cx)
            });
            self.out_price.update(cx, |s, cx| {
                s.set_value(opt_f64_input(ov.output_per_mtok), window, cx)
            });
            self.cache_read_price.update(cx, |s, cx| {
                s.set_value(opt_f64_input(ov.cache_read_per_mtok), window, cx)
            });
            self.cache_write_price.update(cx, |s, cx| {
                s.set_value(opt_f64_input(ov.cache_write_per_mtok), window, cx)
            });
            self.reasoning_price.update(cx, |s, cx| {
                s.set_value(opt_f64_input(ov.reasoning_per_mtok), window, cx)
            });
            self.per_call_price.update(cx, |s, cx| {
                s.set_value(opt_f64_input(ov.per_call), window, cx)
            });
        } else {
            self.pricing_mode = PricingMode::Custom;
        }
        self.error_msg = None;
        cx.notify();
    }
}

impl Render for CatalogDrawer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_close {
            self.pending_close = false;
            window.close_sheet(cx);
        }
        let p = palette(cx);

        let mut content = v_col()
            .flex_1()
            .min_h_0()
            .gap(px(14.0))
            .overflow_y_scrollbar();

        if let Some(err) = &self.error_msg {
            content = content.child(
                h_row()
                    .gap(px(8.0))
                    .p(px(10.0))
                    .rounded(px(8.0))
                    .bg(to_hsla(p.bad_soft))
                    .child(status_indicator(StatusShape::Bad, cx))
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(to_hsla(p.bad))
                            .child(SharedString::from(err.clone())),
                    ),
            );
        }

        let (show_reset, show_delete_user) = match &self.kind {
            DrawerKind::ModelDetail { row, detail } => {
                let offers = detail
                    .as_deref()
                    .map(|d| d.offers.as_slice())
                    .unwrap_or(&[]);
                let heading = if let Some((_, pname, _)) = &self.editing_provider_override {
                    crate::ui::tr(cx).settings().drawer_provider_override(pname)
                } else {
                    crate::ui::tr(cx)
                        .settings()
                        .drawer_default_pricing_all_providers()
                        .into()
                };
                content = content
                    .child(render_model_facts(
                        row,
                        offers.len().max(row.provider_count as usize),
                        cx,
                    ))
                    .children(
                        self.editing_provider_override
                            .as_ref()
                            .map(|(_, pname, _)| {
                                crate::ui::alert_banner(
                                    crate::ui::Tone::Accent,
                                    crate::ui::tr(cx)
                                        .settings()
                                        .drawer_editing_per_provider_override_for(pname),
                                    Some(
                                        crate::ui::btn_ghost(
                                            "cat-ovr-back",
                                            crate::ui::tr(cx).settings().drawer_back_to_default(),
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.editing_provider_override = None;
                                            cx.notify();
                                        }))
                                        .into_any_element(),
                                    ),
                                    cx,
                                )
                            }),
                    )
                    .children(
                        (row.source == CatalogSource::User
                            && self.editing_provider_override.is_none())
                        .then(|| {
                            super::drawer_forms_extra::render_user_model_meta_editor(self, cx)
                        }),
                    )
                    .child(
                        v_col()
                            .gap(px(6.0))
                            .child(section_heading(heading, cx))
                            .child(render_pricing_editor_fields(self, offers, cx)),
                    );
                if let Some(d) = detail.as_deref() {
                    content = content.child(render_provider_overrides_list(d, cx)).child(
                        render_offers_table(&d.offers, self.selected_catalog_family.as_deref(), cx),
                    );
                }
                (true, row.source == CatalogSource::User)
            }
            DrawerKind::AddUserModel => {
                content = content.child(render_add_user_model_body(self, cx));
                (false, false)
            }
            DrawerKind::Unlisted { item } => {
                content = content.child(render_unlisted_body(self, &item.provider_name, cx));
                (true, false)
            }
        };

        v_col()
            .size_full()
            .gap(px(12.0))
            .child(content)
            .child(render_drawer_footer(
                show_reset,
                show_delete_user,
                self.busy,
                cx,
            ))
    }
}
