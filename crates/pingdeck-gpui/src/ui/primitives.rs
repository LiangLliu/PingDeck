//! Semantic PingDeck UI primitives wrapping `gpui-kit`'s component library.
//!
//! `src/ui/` is the ONLY directory allowed to import `gpui_kit::component::*` (enforced by
//! `cargo xtask check-arch`). All feature views in `src/views/` import solely from `crate::ui::*`.

use gpui_kit::component::{
    Sizable as _, TitleBar,
    button::{Button, ButtonVariants as _},
    input::{Editor, EditorState, Input, InputState, Textarea, TextareaState},
    switch::Switch,
    table::{Column, DataTable, TableDelegate, TableState},
    text::TextView,
};
use gpui_kit::{
    App, ClickEvent, Div, ElementId, Entity, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    TitlebarOptions, Window, div, px,
};

use super::icons::{UiIcon, icon};
use super::theme::{palette, to_hsla};

/// Semantic status tone for badges, pills, and banners.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
}

/// Returns `TitlebarOptions` placing macOS traffic lights directly inside the left sidebar's
/// dedicated 38px top drag band (`--bg-shell`) so neither expanded brand rows nor the collapsed
/// rail collide with the window controls.
#[must_use]
pub fn title_bar_options() -> TitlebarOptions {
    TitlebarOptions {
        title: None,
        appears_transparent: true,
        traffic_light_position: Some(gpui_kit::point(px(14.0), px(13.0))),
    }
}

/// Native draggable overlay title bar container.
#[must_use]
pub fn app_title_bar() -> TitleBar {
    TitleBar::new()
}

/// Single-line text input widget.
#[must_use]
pub fn text_input(state: &Entity<InputState>) -> Input {
    Input::new(state).small()
}

/// Multi-line ordinary text area widget (chat composer, instructions, notes).
#[must_use]
pub fn multiline_input(state: &Entity<TextareaState>) -> Textarea {
    Textarea::new(state).small()
}

/// Tree-sitter syntax-highlighted source code editor widget (JSON, JS, YAML, SQL, etc.).
#[must_use]
pub fn code_editor(state: &Entity<EditorState>) -> Editor {
    Editor::new(state).small()
}

/// Virtualized high-performance data table (10,000+ rows at 120fps).
#[must_use]
pub fn virtual_table<D: TableDelegate>(state: &Entity<TableState<D>>) -> DataTable<D> {
    DataTable::new(state)
}

/// Builds a virtual table column specification.
#[must_use]
pub fn table_column(key: impl Into<SharedString>, name: impl Into<SharedString>) -> Column {
    Column::new(key, name)
}

/// Selectable Markdown rich-text renderer.
#[must_use]
pub fn markdown_view(id: impl Into<ElementId>, source: impl Into<SharedString>) -> TextView {
    TextView::markdown(id, source).selectable(true)
}

/// Streaming Markdown rich-text renderer with smooth token fade and text selection.
#[must_use]
pub fn streaming_markdown_view(
    id: impl Into<ElementId>,
    source: impl Into<SharedString>,
) -> TextView {
    TextView::markdown(id, source)
        .selectable(true)
        .stream_fade(true)
}

/// Primary accent CTA button.
#[must_use]
pub fn btn_primary(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id).primary().small().label(label)
}

/// Standard secondary button.
#[must_use]
pub fn btn_secondary(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id).small().label(label)
}

/// Subtle ghost button (toolbars, inline row actions).
#[must_use]
pub fn btn_ghost(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id).ghost().small().label(label)
}

/// Destructive danger button.
#[must_use]
pub fn btn_danger(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id).danger().small().label(label)
}

/// Compact icon-only ghost button.
#[must_use]
pub fn btn_icon(id: impl Into<ElementId>, glyph: UiIcon) -> Button {
    Button::new(id).ghost().small().icon(icon(glyph))
}

/// Native boolean switch control.
#[must_use]
pub fn toggle_switch(id: impl Into<ElementId>, checked: bool) -> Switch {
    Switch::new(id).checked(checked).small()
}

/// Horizontal flex container with centered items.
#[must_use]
pub fn h_row() -> Div {
    div().flex().flex_row().items_center()
}

/// Vertical flex container.
#[must_use]
pub fn v_col() -> Div {
    div().flex().flex_col()
}

/// Elevated vertical flex card container styled from the active OKLCH palette.
#[must_use]
pub fn card(cx: &App) -> Div {
    let p = palette(cx);
    v_col()
        .bg(to_hsla(p.surface))
        .border_1()
        .border_color(to_hsla(p.line_strong.composite_over(p.surface)))
        .rounded(px(8.0))
}

/// Subtle pane container (`--pane`) for sidebars and secondary columns.
#[must_use]
pub fn pane_box(cx: &App) -> Div {
    let p = palette(cx);
    div()
        .bg(to_hsla(p.pane))
        .border_color(to_hsla(p.line.composite_over(p.pane)))
}

/// Section header label (12px semibold muted).
#[must_use]
pub fn section_heading(text: impl Into<SharedString>, cx: &App) -> Div {
    let p = palette(cx);
    div()
        .text_xs()
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(to_hsla(p.muted))
        .child(text.into())
}

/// Semantic pill badge with OKLCH-paired soft background, border, and foreground.
#[must_use]
pub fn badge(text: impl Into<SharedString>, tone: Tone, cx: &App) -> Div {
    let p = palette(cx);
    let (bg, fg, border) = match tone {
        Tone::Neutral => (
            to_hsla(p.sunken),
            to_hsla(p.ink_2),
            to_hsla(p.line_strong.composite_over(p.sunken)),
        ),
        Tone::Accent => (
            to_hsla(p.accent_soft),
            to_hsla(p.accent_ink),
            to_hsla(p.accent_line.composite_over(p.accent_soft)),
        ),
        Tone::Success => (
            to_hsla(p.ok_soft),
            to_hsla(p.ok),
            to_hsla(p.ok_line.composite_over(p.ok_soft)),
        ),
        Tone::Warning => (
            to_hsla(p.warn_soft),
            to_hsla(p.warn),
            to_hsla(p.warn_line.composite_over(p.warn_soft)),
        ),
        Tone::Danger => (
            to_hsla(p.bad_soft),
            to_hsla(p.bad),
            to_hsla(p.bad_line.composite_over(p.bad_soft)),
        ),
    };
    div()
        .px(px(7.0))
        .py(px(2.0))
        .rounded(px(999.0))
        .bg(bg)
        .border_1()
        .border_color(border)
        .text_xs()
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(fg)
        .child(text.into())
}

/// Colour-coded HTTP method badge matching PingDeck's `--m-*` OKLCH tokens.
#[must_use]
pub fn method_badge(method: &str, cx: &App) -> Div {
    let upper = method.to_ascii_uppercase();
    let fg: Hsla = super::menus::method_color(&upper, cx);
    div()
        .w(px(54.0))
        .text_xs()
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(fg)
        .child(SharedString::from(upper))
}

/// Compact keyboard shortcut keycap (`⌘K`, `⌘1`, etc.).
#[must_use]
pub fn kbd_chip(text: impl Into<SharedString>, cx: &App) -> Div {
    let p = palette(cx);
    div()
        .px(px(5.0))
        .py(px(1.0))
        .rounded(px(4.0))
        .bg(to_hsla(p.surface))
        .border_1()
        .border_color(to_hsla(p.line_strong.composite_over(p.surface)))
        .text_xs()
        .text_color(to_hsla(p.muted))
        .child(text.into())
}

/// Live status dot (e.g., green gateway active / neutral stopped).
#[must_use]
pub fn status_dot(active: bool, cx: &App) -> Div {
    let p = palette(cx);
    let color = if active {
        to_hsla(p.live)
    } else {
        to_hsla(p.faint)
    };
    div().size(px(8.0)).rounded(px(999.0)).bg(color)
}

/// Settings / form field row with label, optional description, and right-aligned control.
#[must_use]
pub fn field_row(
    label: impl Into<SharedString>,
    description: Option<SharedString>,
    control: impl IntoElement,
    cx: &App,
) -> Div {
    let p = palette(cx);
    let mut left = v_col().flex_1().min_w_0().gap(px(2.0)).child(
        div()
            .text_sm()
            .font_weight(gpui_kit::FontWeight::MEDIUM)
            .text_color(to_hsla(p.ink))
            .child(label.into()),
    );
    if let Some(desc) = description {
        left = left.child(div().text_xs().text_color(to_hsla(p.muted)).child(desc));
    }
    h_row()
        .justify_between()
        .gap(px(16.0))
        .py(px(10.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(left)
        .child(div().flex_shrink_0().child(control))
}

/// Centered empty state with icon, title, and centered muted helper copy.
#[must_use]
pub fn empty_state(
    glyph: UiIcon,
    title: impl Into<SharedString>,
    subtitle: impl Into<SharedString>,
    cx: &App,
) -> Div {
    super::feedback::empty_state(glyph, title, subtitle, cx)
}

/// Interactive selectable list/sidebar row with PingDeck's signature 3px inset vertical
/// selection indicator, compact density awareness, and clean hover tint.
pub fn list_row(
    id: impl Into<ElementId>,
    selected: bool,
    cx: &App,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let p = palette(cx);
    let compact = super::theme::is_compact(cx);
    let py_px = if compact { 4.5 } else { 6.0 };
    let hover_bg = to_hsla(p.hover.composite_over(p.pane));
    let active_bg = to_hsla(p.selected);
    let mut row = div()
        .id(id)
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .pl(px(10.0))
        .pr(px(8.0))
        .py(px(py_px))
        .rounded(px(7.0))
        .cursor_pointer()
        .on_click(on_click);
    if selected {
        row = row
            .bg(active_bg)
            .text_color(to_hsla(p.ink))
            .font_weight(gpui_kit::FontWeight::MEDIUM)
            .child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .top(px(5.0))
                    .bottom(px(5.0))
                    .w(px(3.0))
                    .rounded(px(2.0))
                    .bg(to_hsla(p.accent)),
            );
    } else {
        row = row
            .text_color(to_hsla(p.ink_2))
            .hover(move |s| s.bg(hover_bg).text_color(to_hsla(p.ink)));
    }
    row
}
