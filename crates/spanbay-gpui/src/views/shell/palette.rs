//! Interactive Command Palette (`⌘K`) backed by [`spanbay_desktop::state::PaletteEngine`].

use std::cell::Cell;
use std::collections::BTreeMap;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
    px,
};
use spanbay_desktop::core::app::{
    agent_session_list_impl, flow_list_impl, llm_provider_list_impl, mock_server_list_impl,
    mock_server_status_list_impl, request_list_impl,
};
use spanbay_desktop::core::model::settings::Language;
use spanbay_desktop::i18n::tr as tr_lang;
use spanbay_desktop::state::{PaletteAction, PaletteCategory, PaletteDynamicItems, PaletteEngine};

use crate::bridge::runtime;
use crate::ui::{
    InputEvent, InputState, ScrollableElement as _, Tone, UiIcon, active_language, badge, card,
    h_row, icon, kbd_chip, list_row, method_badge, palette, section_heading, text_input, to_hsla,
    tr, v_col,
};

pub struct CommandPaletteView {
    search_input: Entity<InputState>,
    engine: PaletteEngine,
    frecency: BTreeMap<String, (u32, i64)>,
    selected_idx: usize,
    pending_action: Option<(String, PaletteAction)>,
    dismissed: Cell<bool>,
    lang: Language,
    _subscriptions: Vec<Subscription>,
}

impl CommandPaletteView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let initial_lang = active_language(cx);
        let init_ph = tr(cx).shell().palette_placeholder_init();
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder(init_ph));
        let sub = cx.subscribe(&search_input, |this, _, ev: &InputEvent, cx| match ev {
            InputEvent::Change => {
                this.selected_idx = 0;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                this.trigger_selected(cx);
            }
            _ => {}
        });
        Self {
            search_input,
            engine: PaletteEngine::default(),
            frecency: BTreeMap::new(),
            selected_idx: 0,
            pending_action: None,
            dismissed: Cell::new(false),
            lang: initial_lang,
            _subscriptions: vec![sub],
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &mut self,
        lang: Language,
        workspaces: Vec<(String, String)>,
        attention: Vec<(String, String)>,
        dynamic: PaletteDynamicItems,
        frecency: BTreeMap<String, (u32, i64)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.lang = lang;
        self.engine = PaletteEngine::build_full(lang, &workspaces, &attention, &dynamic);
        self.frecency = frecency;
        self.selected_idx = 0;
        self.pending_action = None;
        self.dismissed.set(false);
        let placeholder = tr_lang(lang).shell().palette_placeholder();
        self.search_input.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx);
            input.set_value("", window, cx);
            input.focus_handle(cx).focus(window, cx);
        });
        cx.notify();
    }

    pub fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).value().to_string();
        let count = self.engine.search(&query, &self.frecency, 24).len();
        if count == 0 {
            self.selected_idx = 0;
        } else {
            let cur = self.selected_idx.min(count.saturating_sub(1)) as isize;
            let next = (cur + delta).rem_euclid(count as isize) as usize;
            self.selected_idx = next;
        }
        cx.notify();
    }

    pub fn trigger_selected(&mut self, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).value().to_string();
        let matches = self.engine.search(&query, &self.frecency, 24);
        if matches.is_empty() {
            return;
        }
        let idx = self.selected_idx.min(matches.len().saturating_sub(1));
        if let Some(item) = matches.get(idx) {
            self.pending_action = Some((item.id.clone(), item.action.clone()));
            cx.notify();
        }
    }

    pub fn has_pending_signal(&self) -> bool {
        self.pending_action.is_some() || self.dismissed.get()
    }

    pub fn take_pending_action(&mut self) -> Option<(String, PaletteAction)> {
        self.pending_action.take()
    }

    pub fn take_dismissed_flag(&self) -> bool {
        self.dismissed.replace(false)
    }
}

impl Render for CommandPaletteView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let sh = tr_lang(self.lang).shell();
        let gate_badge = sh.palette_badge_gate();
        let query = self.search_input.read(cx).value().to_string();
        let matches = self.engine.search(&query, &self.frecency, 24);
        let clamped_idx = self.selected_idx.min(matches.len().saturating_sub(1));

        let mut list = v_col().w_full().gap(px(2.0)).p(px(6.0));
        if matches.is_empty() {
            list = list.child(
                div()
                    .px(px(12.0))
                    .py(px(24.0))
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(sh.palette_empty()),
            );
        } else {
            let mut last_cat: Option<PaletteCategory> = None;
            for (idx, item) in matches.iter().enumerate() {
                if last_cat != Some(item.category) {
                    last_cat = Some(item.category);
                    list = list.child(
                        div()
                            .px(px(8.0))
                            .pt(if idx == 0 { px(2.0) } else { px(8.0) })
                            .pb(px(2.0))
                            .child(section_heading(item.category.label(self.lang), cx)),
                    );
                }
                let id = item.id.clone();
                let action = item.action.clone();
                let badge_el = item.badge_label.as_deref().map(|lbl| {
                    if matches!(
                        lbl,
                        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
                    ) {
                        method_badge(lbl, cx).into_any_element()
                    } else {
                        let tone = if lbl == "LIVE" {
                            Tone::Success
                        } else if lbl == gate_badge {
                            Tone::Warning
                        } else if matches!(lbl, "AI" | "GW") {
                            Tone::Accent
                        } else {
                            Tone::Neutral
                        };
                        badge(SharedString::from(lbl.to_owned()), tone, cx).into_any_element()
                    }
                });
                let mut row = list_row(
                    SharedString::from(format!("pal-{idx}")),
                    idx == clamped_idx,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        this.pending_action = Some((id.clone(), action.clone()));
                        cx.notify();
                    }),
                )
                .justify_between()
                .child(
                    h_row()
                        .gap(px(7.0))
                        .min_w_0()
                        .flex_1()
                        .children(badge_el)
                        .child(
                            div()
                                .text_xs()
                                .truncate()
                                .child(SharedString::from(item.title.clone())),
                        )
                        .when_some(item.subtitle.clone(), |el, sub| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(to_hsla(p.muted))
                                    .truncate()
                                    .child(SharedString::from(sub)),
                            )
                        }),
                );
                if let Some(sc) = &item.shortcut {
                    row = row.child(kbd_chip(SharedString::from(sc.clone()), cx));
                }
                list = list.child(row);
            }
        }

        card(cx)
            .w_full()
            .bg(to_hsla(p.overlay))
            .overflow_hidden()
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                match ev.keystroke.key.as_str() {
                    "down" => {
                        this.move_selection(1, cx);
                        cx.stop_propagation();
                    }
                    "up" => {
                        this.move_selection(-1, cx);
                        cx.stop_propagation();
                    }
                    "enter" => {
                        this.trigger_selected(cx);
                        cx.stop_propagation();
                    }
                    "escape" => {
                        this.dismissed.set(true);
                        cx.notify();
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .child(
                h_row()
                    .gap(px(8.0))
                    .px(px(12.0))
                    .py(px(10.0))
                    .border_b_1()
                    .border_color(to_hsla(p.line.composite_over(p.overlay)))
                    .child(icon(UiIcon::Search))
                    .child(div().flex_1().child(text_input(&self.search_input)))
                    .child(kbd_chip("ESC", cx)),
            )
            .child(
                div()
                    .id("palette-results-scroll")
                    .w_full()
                    .max_h(px(360.0))
                    .overflow_y_scrollbar()
                    .child(list),
            )
            .child(
                h_row()
                    .w_full()
                    .px(px(12.0))
                    .py(px(6.0))
                    .border_t_1()
                    .border_color(to_hsla(p.line.composite_over(p.overlay)))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(to_hsla(p.faint))
                            .child(sh.palette_footer_hint()),
                    ),
            )
    }
}

impl super::AppShell {
    /// Snapshots dynamic workspace & shared-AI entities for the `⌘K` Command Palette.
    pub fn collect_palette_dynamic_items(&self, cx: &App) -> PaletteDynamicItems {
        let rt = runtime(cx);
        let state = rt.state();
        let mut out = PaletteDynamicItems::default();

        if let Some(ws_id) = self.state.active_workspace_id.as_deref() {
            if let Ok(reqs) = request_list_impl(state, ws_id.to_owned()) {
                out.requests = reqs
                    .into_iter()
                    .take(40)
                    .map(|r| {
                        (
                            r.id.as_str().to_owned(),
                            format!("{:?}", r.method).to_uppercase(),
                            if r.name.trim().is_empty() {
                                r.url
                            } else {
                                r.name
                            },
                        )
                    })
                    .collect();
            }
            if let Ok(servers) = mock_server_list_impl(state, ws_id.to_owned()) {
                let statuses =
                    mock_server_status_list_impl(state, ws_id.to_owned()).unwrap_or_default();
                out.mocks = servers
                    .into_iter()
                    .take(20)
                    .map(|s| {
                        let running = statuses.iter().any(|st| st.server_id == s.id && st.running);
                        (s.id, s.name, running)
                    })
                    .collect();
            }
            if let Ok(flows) = flow_list_impl(state, ws_id.to_owned()) {
                out.flows = flows
                    .into_iter()
                    .take(20)
                    .map(|f| (f.flow.id, f.flow.name))
                    .collect();
            }
        }

        if let Ok(page) = agent_session_list_impl(state, None, Some(20), None) {
            out.chats = page
                .sessions
                .into_iter()
                .map(|c| {
                    (
                        c.id,
                        if c.title.trim().is_empty() {
                            "Untitled Chat".to_owned()
                        } else {
                            c.title
                        },
                    )
                })
                .collect();
        }
        if let Ok(providers) = llm_provider_list_impl(state) {
            out.providers = providers
                .into_iter()
                .take(20)
                .map(|p| (p.id, p.name))
                .collect();
        }
        out
    }
}
