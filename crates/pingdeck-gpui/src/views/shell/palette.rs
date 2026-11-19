//! Interactive Command Palette (`⌘K`) backed by [`pingdeck_desktop::state::PaletteEngine`].

use std::cell::Cell;
use std::collections::BTreeMap;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Window, div, px,
};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::state::{PaletteAction, PaletteEngine};

use crate::ui::{
    InputState, UiIcon, card, h_row, icon, kbd_chip, list_row, palette, text_input, to_hsla, v_col,
};

pub struct CommandPaletteView {
    search_input: Entity<InputState>,
    engine: PaletteEngine,
    frecency: BTreeMap<String, (u32, i64)>,
    selected_idx: usize,
    pending_action: Option<(String, PaletteAction)>,
    dismissed: Cell<bool>,
    lang: Language,
}

impl CommandPaletteView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search modules, workspaces, settings, commands…")
        });
        Self {
            search_input,
            engine: PaletteEngine::default(),
            frecency: BTreeMap::new(),
            selected_idx: 0,
            pending_action: None,
            dismissed: Cell::new(false),
            lang: Language::ZhCn,
        }
    }

    pub fn open(
        &mut self,
        lang: Language,
        workspaces: Vec<(String, String)>,
        attention: Vec<(String, String)>,
        frecency: BTreeMap<String, (u32, i64)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.lang = lang;
        self.engine = PaletteEngine::build_default(lang, &workspaces, &attention);
        self.frecency = frecency;
        self.selected_idx = 0;
        self.pending_action = None;
        self.dismissed.set(false);
        self.search_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus_handle(cx).focus(window, cx);
        });
        cx.notify();
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
        let matches = self.engine.search(&query, &self.frecency, 14);
        let clamped_idx = self.selected_idx.min(matches.len().saturating_sub(1));

        let mut list = v_col().gap(px(2.0)).p(px(6.0));
        if matches.is_empty() {
            list = list.child(
                div()
                    .px(px(12.0))
                    .py(px(24.0))
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(if zh {
                        "没有匹配的命令或页面"
                    } else {
                        "No matching commands or views"
                    }),
            );
        } else {
            for (idx, item) in matches.iter().enumerate() {
                let id = item.id.clone();
                let action = item.action.clone();
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
                        .gap(px(8.0))
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .child(SharedString::from(item.title.clone())),
                        )
                        .when_some(item.subtitle.clone(), |el, sub| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(to_hsla(p.muted))
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
            .child(list)
    }
}
