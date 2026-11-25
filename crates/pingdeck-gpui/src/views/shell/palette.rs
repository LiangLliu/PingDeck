//! Interactive Command Palette (`⌘K`) backed by [`pingdeck_desktop::state::PaletteEngine`].

use std::cell::Cell;
use std::collections::BTreeMap;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
    px,
};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::state::{PaletteAction, PaletteCategory, PaletteDynamicItems, PaletteEngine};

use crate::ui::{
    InputEvent, InputState, ScrollableElement as _, Tone, UiIcon, badge, card, h_row, icon,
    kbd_chip, list_row, method_badge, palette, section_heading, text_input, to_hsla, v_col,
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
        let search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("搜索模块、请求、对话、服务商、设置、命令…")
        });
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
            lang: Language::ZhCn,
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
        let placeholder = if lang == Language::ZhCn {
            "搜索模块、请求、对话、Mock、流程、服务商、设置…"
        } else {
            "Search modules, requests, chats, mocks, flows, providers, settings…"
        };
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
        let zh = self.lang == Language::ZhCn;
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
                    .child(if zh {
                        "没有匹配的命令、实体或页面"
                    } else {
                        "No matching commands, items, or views"
                    }),
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
                            .child(section_heading(item.category.label(zh), cx)),
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
                        let tone = match lbl {
                            "LIVE" => Tone::Success,
                            "GATE" | "待批" => Tone::Warning,
                            "AI" | "GW" => Tone::Accent,
                            _ => Tone::Neutral,
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

        let footer_hint = if zh {
            "↑↓ 选择 · ↵ 打开 · Esc 关闭"
        } else {
            "↑↓ Navigate · ↵ Open · Esc Close"
        };

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
                    .justify_between()
                    .px(px(12.0))
                    .py(px(6.0))
                    .border_t_1()
                    .border_color(to_hsla(p.line.composite_over(p.overlay)))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(to_hsla(p.faint))
                            .child(footer_hint),
                    )
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(to_hsla(p.faint))
                            .child(format!(
                                "{} {}",
                                matches.len(),
                                if zh { "项" } else { "items" }
                            )),
                    ),
            )
    }
}
