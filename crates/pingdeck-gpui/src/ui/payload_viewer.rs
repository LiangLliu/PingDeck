//! Unified response & payload viewer primitives (`PayloadViewer` + `payload_block`).
//!
//! Single visual and formatting contract for every HTTP/AI/Mock/Flow/Gateway/Agent payload across
//! the entire application:
//! - Automatic format detection (`JSON`, `XML`, `HTML`, `YAML`, `Text`, `Hex`, `Base64`)
//! - Strict zero-rewrite pretty-printing via `pingdeck_response::response_viewer`
//! - Tree-sitter syntax highlighting synced with PingDeck's 16 OKLCH palettes
//! - Two cohesive forms sharing the same frame, micro-header, typography, and copy action:
//!   1. [`PayloadViewer`]: full Rope `EditorState(readonly)` workbench view (Pretty/Raw, Format
//!      picker, Soft Wrap, JSONPath/text filter, 10 MiB+ virtualization).
//!   2. [`payload_block`]: lightweight embedded card block (auto-sized, selectable highlighted
//!      code block via `TextView::markdown`, unified micro-header + 1-click Copy).

use gpui_kit::component::{
    input::{EditorState, InputEvent, InputState},
    scroll::ScrollableElement as _,
    text::{TextView, TextViewStyle},
    theme::Theme,
};
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StyleRefinement, Styled as _,
    Subscription, Window, div, px,
};
use pingdeck_desktop::core::app::{
    BodySource, response_filter_jsonpath_impl, response_format_body_impl,
};
use pingdeck_desktop::core::response_viewer::{
    ALL_RESPONSE_FORMATS, JsonPathFilterOutcomeDto, ResponseBodyFormat, detect_response_format,
    format_label, format_response_body,
};
use pingdeck_desktop::services::clipboard;

use crate::bridge::runtime;

use super::data::choice_chip;
use super::icons::{UiIcon, icon_xs};
use super::primitives::{
    Tone, badge, btn_ghost, btn_icon, btn_secondary, code_editor, h_row, list_row, text_input,
    v_col,
};
use super::theme::{palette, to_hsla};
use super::widgets::{popover, select_pill, tab_bar_segmented, tab_item};

/// Default inline preview ceiling so multi-card inspector drawers stay snappy while Copy keeps full
/// bytes.
const DEFAULT_COMPACT_PREVIEW_CHARS: usize = 24_000;
/// Maximum byte length to run inline synchronous pretty-printing in [`payload_block`].
const COMPACT_FORMAT_CAP: usize = 256 * 1024;

/// Maps a [`ResponseBodyFormat`] to a compiled Tree-sitter language identifier in `gpui-kit`.
///
/// XML maps to `"html"` because `tree-sitter-html` is compiled in and highlights XML tags,
/// attributes, and string values cleanly.
#[must_use]
pub const fn highlighter_for_format(format: ResponseBodyFormat) -> &'static str {
    match format {
        ResponseBodyFormat::Json => "json",
        ResponseBodyFormat::Xml | ResponseBodyFormat::Html => "html",
        ResponseBodyFormat::Yaml => "yaml",
        ResponseBodyFormat::Text | ResponseBodyFormat::Hex | ResponseBodyFormat::Base64 => "plain",
    }
}

/// Human-readable byte size helper shared across payload headers.
#[must_use]
pub fn format_payload_bytes(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Configuration for the embedded [`payload_block`] card.
pub struct PayloadBlockOptions<'a> {
    /// Optional left heading shown before the format badge (e.g. `"Response Body"`).
    pub title: Option<&'a str>,
    /// Optional HTTP `Content-Type` header hint used for automatic format detection.
    pub content_type: Option<&'a str>,
    /// Explicit format override bypassing auto-detection when `Some`.
    pub format_override: Option<ResponseBodyFormat>,
    /// Whether to auto-format structured formats (JSON/XML/HTML/YAML). Defaults to `true`.
    pub pretty: bool,
    /// Optional vertical scroll cap in logical pixels (e.g. `Some(280.0)` inside cards).
    pub max_height_px: Option<f32>,
    /// Maximum character budget rendered inline (Copy always copies full `raw`).
    pub max_preview_chars: usize,
    /// Whether to display the unified micro-header bar (format badge, line count, copy).
    pub show_header: bool,
    /// Whether to show the built-in 1-click Copy button in the micro-header.
    pub show_copy: bool,
    /// Optional extra action element placed beside the Copy button (e.g., Expand / Raw toggle).
    pub extra_actions: Option<AnyElement>,
}

impl Default for PayloadBlockOptions<'_> {
    fn default() -> Self {
        Self {
            title: None,
            content_type: None,
            format_override: None,
            pretty: true,
            max_height_px: Some(300.0),
            max_preview_chars: DEFAULT_COMPACT_PREVIEW_CHARS,
            show_header: true,
            show_copy: true,
            extra_actions: None,
        }
    }
}

/// Renders a unified, auto-detected, syntax-highlighted, selectable payload block for embedded
/// panels (History Sent Request, AI Raw, Flow Step Output, Mock Traffic & Buckets, Usage/Gateway
/// Call Payloads, Agent Tool Args/Output, Diagnostics).
#[must_use]
pub fn payload_block(
    id: impl Into<SharedString>,
    raw: &str,
    opts: PayloadBlockOptions<'_>,
    cx: &App,
) -> AnyElement {
    let id_str = id.into();
    let p = palette(cx);
    let ex = super::i18n::tr(cx).exchange();

    let fmt = opts
        .format_override
        .unwrap_or_else(|| detect_response_format(opts.content_type, raw));
    let is_diff = fmt == ResponseBodyFormat::Text && looks_like_diff(raw);

    let formatted =
        if opts.pretty && fmt != ResponseBodyFormat::Text && raw.len() <= COMPACT_FORMAT_CAP {
            format_response_body(raw, fmt, COMPACT_FORMAT_CAP).text
        } else {
            raw.to_owned()
        };

    let total_chars = formatted.chars().count();
    let truncated = total_chars > opts.max_preview_chars;
    let display_text: String = if truncated {
        formatted.chars().take(opts.max_preview_chars).collect()
    } else {
        formatted
    };

    let line_count = display_text
        .lines()
        .count()
        .max(usize::from(!display_text.is_empty()));
    let meta_str = ex.payload_lines_size(line_count, format_payload_bytes(raw.len()));
    let fmt_badge_label = if is_diff { "Diff" } else { format_label(fmt) };
    let fmt_tone = if is_diff || fmt != ResponseBodyFormat::Text {
        Tone::Accent
    } else {
        Tone::Neutral
    };

    let fence_lang = if is_diff {
        "diff"
    } else {
        match fmt {
            ResponseBodyFormat::Json => "json",
            ResponseBodyFormat::Xml | ResponseBodyFormat::Html => "html",
            ResponseBodyFormat::Yaml => "yaml",
            ResponseBodyFormat::Text | ResponseBodyFormat::Hex | ResponseBodyFormat::Base64 => {
                "text"
            }
        }
    };
    let fenced = fence_code(display_text.trim_end_matches('\n'), fence_lang);

    let code_style = StyleRefinement {
        background: Some(to_hsla(p.sunken).into()),
        ..Default::default()
    };
    let mut tv_style = TextViewStyle::default().code_block(code_style);
    tv_style.highlight_theme = Theme::global(cx).highlight_theme.clone();
    tv_style.is_dark = p.is_dark;
    let tv = TextView::markdown(SharedString::from(format!("{id_str}-tv")), fenced)
        .selectable(true)
        .style(tv_style);

    let mut root = v_col()
        .w_full()
        .rounded(px(7.0))
        .bg(to_hsla(p.sunken))
        .border_1()
        .border_color(to_hsla(p.line_strong.composite_over(p.sunken)))
        .overflow_hidden();

    if opts.show_header {
        let copy_raw = raw.to_owned();
        let mut header = h_row()
            .w_full()
            .justify_between()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(5.0))
            .bg(to_hsla(p.pane))
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)));

        let mut left = h_row().gap(px(6.0)).min_w_0();
        if let Some(title) = opts.title {
            left = left.child(
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(to_hsla(p.ink))
                    .truncate()
                    .child(SharedString::from(title.to_owned())),
            );
        }
        left = left.child(badge(fmt_badge_label, fmt_tone, cx)).child(
            div()
                .text_size(px(11.0))
                .text_color(to_hsla(p.faint))
                .child(SharedString::from(meta_str)),
        );

        let mut right = h_row().gap(px(4.0)).flex_shrink_0();
        if let Some(extra) = opts.extra_actions {
            right = right.child(extra);
        }
        if opts.show_copy {
            right = right.child(
                btn_ghost(
                    SharedString::from(format!("{id_str}-cp")),
                    ex.payload_copy(),
                )
                .icon(icon_xs(UiIcon::Copy))
                .on_click(move |_, _, _| {
                    let _ = clipboard::set_text(&copy_raw);
                }),
            );
        }
        header = header.child(left).child(right);
        root = root.child(header);
    }

    let body_el: AnyElement = if let Some(max_h) = opts.max_height_px {
        div()
            .id(SharedString::from(format!("{id_str}-scroll")))
            .w_full()
            .max_h(px(max_h))
            .px(px(10.0))
            .py(px(6.0))
            .overflow_scrollbar()
            .child(tv)
            .into_any_element()
    } else {
        div()
            .id(SharedString::from(format!("{id_str}-scroll")))
            .w_full()
            .px(px(10.0))
            .py(px(6.0))
            .overflow_x_scrollbar()
            .child(tv)
            .into_any_element()
    };
    root = root.child(body_el);

    if truncated {
        root = root.child(
            div()
                .w_full()
                .px(px(10.0))
                .py(px(4.0))
                .bg(to_hsla(p.pane))
                .border_t_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .text_size(px(11.0))
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(ex.payload_truncated_preview(
                    format_payload_bytes(display_text.len()),
                    format_payload_bytes(raw.len()),
                ))),
        );
    }

    root.into_any_element()
}

/// Shared compact format picker + Pretty/Raw + Soft Wrap toolbar cluster reused by both
/// [`PayloadViewer`] and `RequestsView` HTTP response pane so every full viewer shares the exact
/// same visual grammar.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn payload_format_picker(
    id_prefix: &'static str,
    format: ResponseBodyFormat,
    pretty: bool,
    soft_wrap: bool,
    on_toggle_pretty: impl Fn(bool, &mut Window, &mut App) + 'static,
    on_pick_format: impl Fn(ResponseBodyFormat, &mut Window, &mut App) + Clone + 'static,
    on_toggle_wrap: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let ex = super::i18n::tr(cx).exchange();
    h_row()
        .gap(px(6.0))
        .flex_shrink_0()
        .child(
            tab_bar_segmented(SharedString::from(format!("{id_prefix}-pr-seg")))
                .selected_index(usize::from(!pretty))
                .child(tab_item(ex.body_pretty()))
                .child(tab_item(ex.body_raw()))
                .on_click(move |idx, window, cx| {
                    on_toggle_pretty(*idx == 0, window, cx);
                }),
        )
        .child(
            popover(SharedString::from(format!("{id_prefix}-fmt-pop")))
                .anchor(Anchor::TopRight)
                .trigger(select_pill(
                    SharedString::from(format!("{id_prefix}-fmt-btn")),
                    format_label(format),
                    false,
                    cx,
                ))
                .content(move |_, _, pop_cx| {
                    let mut col = v_col().w(px(116.0)).gap(px(2.0));
                    for fmt in ALL_RESPONSE_FORMATS {
                        let sel = fmt == format;
                        let cb = on_pick_format.clone();
                        let ph = pop_cx.entity().clone();
                        col = col.child(
                            list_row(
                                SharedString::from(format!("{id_prefix}-fmt-{fmt:?}")),
                                sel,
                                pop_cx,
                                move |_, window, cx| {
                                    cb(fmt, window, cx);
                                    ph.update(cx, |s, cx| s.dismiss(window, cx));
                                },
                            )
                            .child(div().text_xs().child(format_label(fmt))),
                        );
                    }
                    col
                }),
        )
        .child(
            choice_chip(
                SharedString::from(format!("{id_prefix}-wrap")),
                ex.payload_wrap(),
                soft_wrap,
                cx,
            )
            .on_click(move |_, window, cx| {
                on_toggle_wrap(window, cx);
            }),
        )
        .into_any_element()
}

/// Full interactive Rope-backed payload viewer (`Entity<PayloadViewer>`).
///
/// Used by primary payload panes (`HistoryView` Body, `WorkflowMonitorModal` full-output dialog,
/// and any large read-only payload panel) to provide identical Pretty/Raw, Format switching,
/// Soft Wrap, JSONPath/text filtering, Tree-sitter highlighting, line numbers, and 1-click Copy.
pub struct PayloadViewer {
    id_prefix: &'static str,
    editor: Entity<EditorState>,
    filter_input: Entity<InputState>,
    source: BodySource,
    raw_bytes_len: usize,
    raw_fallback: String,
    format: ResponseBodyFormat,
    pretty: bool,
    soft_wrap: bool,
    formatted_cache: Option<String>,
    filter_result: Option<(bool, String)>,
    copied_flash: bool,
    show_filter: bool,
    _subs: Vec<Subscription>,
}

impl PayloadViewer {
    /// Creates a new empty [`PayloadViewer`] ready to receive payloads via [`Self::set_text`] or
    /// [`Self::set_payload`].
    pub fn new(
        id_prefix: &'static str,
        show_filter: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut ed = EditorState::new(window, cx).soft_wrap(true);
            ed.set_highlighter("plain", cx);
            ed
        });
        let ph = super::i18n::tr(cx).exchange().payload_filter_placeholder();
        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder(ph));
        let sub = cx.subscribe_in(
            &filter_input,
            window,
            |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.apply_filter(window, cx);
                }
            },
        );
        Self {
            id_prefix,
            editor,
            filter_input,
            source: BodySource::Text(String::new()),
            raw_bytes_len: 0,
            raw_fallback: String::new(),
            format: ResponseBodyFormat::Text,
            pretty: true,
            soft_wrap: true,
            formatted_cache: None,
            filter_result: None,
            copied_flash: false,
            show_filter,
            _subs: vec![sub],
        }
    }

    /// Loads plain or structured text with automatic format detection and highlighting.
    pub fn set_text(
        &mut self,
        raw: impl Into<String>,
        content_type: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = raw.into();
        let fmt = detect_response_format(content_type, &text);
        self.raw_bytes_len = text.len();
        self.raw_fallback = text.clone();
        self.source = BodySource::Text(text);
        self.format = fmt;
        self.pretty = true;
        self.copied_flash = false;
        self.filter_result = None;
        self.filter_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.sync_editor(window, cx);
    }

    /// Returns the currently displayed text in the underlying Rope editor.
    #[must_use]
    pub fn displayed_text(&self, cx: &App) -> String {
        self.editor.read(cx).value().to_string()
    }

    /// Currently active [`ResponseBodyFormat`].
    #[must_use]
    pub const fn format(&self) -> ResponseBodyFormat {
        self.format
    }

    /// Whether Pretty formatting is currently enabled.
    #[must_use]
    pub const fn is_pretty(&self) -> bool {
        self.pretty
    }

    /// Whether Soft Wrap is currently enabled.
    #[must_use]
    pub const fn is_soft_wrap(&self) -> bool {
        self.soft_wrap
    }

    pub fn set_format(
        &mut self,
        format: ResponseBodyFormat,
        pretty: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.format = format;
        self.pretty = pretty;
        self.filter_result = None;
        self.copied_flash = false;
        self.sync_editor(window, cx);
    }

    pub fn toggle_wrap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.soft_wrap = !self.soft_wrap;
        let wrap = self.soft_wrap;
        self.editor
            .update(cx, |e, cx| e.set_soft_wrap(wrap, window, cx));
        cx.notify();
    }

    pub fn apply_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let expr = self.filter_input.read(cx).value().trim().to_owned();
        if expr.is_empty() {
            self.filter_result = None;
            self.sync_editor(window, cx);
            return;
        }
        if expr.starts_with('$') {
            match response_filter_jsonpath_impl(runtime(cx).state(), self.source.clone(), &expr) {
                Ok(JsonPathFilterOutcomeDto::Filtered { text, mode }) => {
                    self.filter_result = Some((true, mode));
                    self.editor.update(cx, |e, cx| {
                        e.set_highlighter("json", cx);
                        e.set_value(text, window, cx);
                    });
                }
                Ok(JsonPathFilterOutcomeDto::Error { message }) => {
                    self.filter_result = Some((false, message));
                }
                Err(err) => {
                    self.filter_result = Some((false, err.message().to_owned()));
                }
            }
        } else {
            let base = self
                .formatted_cache
                .as_deref()
                .unwrap_or(&self.raw_fallback);
            let q = expr.to_lowercase();
            let matched: Vec<&str> = base
                .lines()
                .filter(|ln| ln.to_lowercase().contains(&q))
                .collect();
            let mode = super::i18n::tr(cx)
                .requests()
                .act_lines_matched(matched.len());
            self.filter_result = Some((true, mode));
            let out = matched.join("\n");
            self.editor.update(cx, |e, cx| e.set_value(out, window, cx));
        }
        cx.notify();
    }

    fn sync_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = if !self.pretty || self.format == ResponseBodyFormat::Text {
            self.formatted_cache = None;
            self.raw_fallback.clone()
        } else {
            match response_format_body_impl(runtime(cx).state(), self.source.clone(), self.format) {
                Ok(dto) => {
                    self.formatted_cache = Some(dto.text.clone());
                    dto.text
                }
                Err(_) => {
                    self.formatted_cache = None;
                    self.raw_fallback.clone()
                }
            }
        };
        let hl = if !self.pretty && self.format == ResponseBodyFormat::Text {
            "plain"
        } else {
            highlighter_for_format(self.format)
        };
        self.editor.update(cx, |e, cx| {
            e.set_highlighter(hl, cx);
            e.set_value(text, window, cx);
        });
        cx.notify();
    }
}

impl Render for PayloadViewer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let ex = super::i18n::tr(cx).exchange();
        let ent_pr = cx.entity().clone();
        let ent_fmt = cx.entity().clone();
        let ent_wr = cx.entity().clone();
        let cur_fmt = self.format;
        let line_count = self.editor.read(cx).value().lines().count();
        let meta = ex.payload_lines_size(line_count, format_payload_bytes(self.raw_bytes_len));

        let mut bar = h_row()
            .w_full()
            .flex_shrink_0()
            .justify_between()
            .gap(px(6.0))
            .px(px(10.0))
            .py(px(6.0))
            .bg(to_hsla(p.pane))
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)));

        let mut left = h_row().gap(px(6.0)).flex_wrap();
        left = left.child(payload_format_picker(
            self.id_prefix,
            self.format,
            self.pretty,
            self.soft_wrap,
            move |pretty, window, cx| {
                ent_pr.update(cx, |this, cx| this.set_format(cur_fmt, pretty, window, cx));
            },
            move |fmt, window, cx| {
                ent_fmt.update(cx, |this, cx| this.set_format(fmt, true, window, cx));
            },
            move |window, cx| {
                ent_wr.update(cx, |this, cx| this.toggle_wrap(window, cx));
            },
            cx,
        ));

        if self.show_filter {
            left = left
                .child(div().w(px(210.0)).child(text_input(&self.filter_input)))
                .child(
                    btn_secondary(
                        SharedString::from(format!("{}-flt-go", self.id_prefix)),
                        ex.resp_http_tabs_filter(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.apply_filter(window, cx))),
                );
            if let Some((ok, mode)) = &self.filter_result {
                left = left
                    .child(
                        btn_ghost(
                            SharedString::from(format!("{}-flt-rst", self.id_prefix)),
                            ex.resp_http_tabs_reset(),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.filter_input
                                .update(cx, |i, cx| i.set_value("", window, cx));
                            this.filter_result = None;
                            this.sync_editor(window, cx);
                        })),
                    )
                    .child(badge(
                        mode.clone(),
                        if *ok { Tone::Accent } else { Tone::Danger },
                        cx,
                    ));
            }
        }

        let copy_lbl = if self.copied_flash {
            ex.payload_copied()
        } else {
            ex.payload_copy()
        };
        let right = h_row()
            .gap(px(6.0))
            .flex_shrink_0()
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(to_hsla(p.faint))
                    .child(SharedString::from(meta)),
            )
            .child(
                btn_icon(
                    SharedString::from(format!("{}-copy", self.id_prefix)),
                    UiIcon::Copy,
                )
                .tooltip(copy_lbl)
                .on_click(cx.listener(|this, _, _, cx| {
                    let txt = this.displayed_text(cx);
                    let _ = clipboard::set_text(&txt);
                    this.copied_flash = true;
                    cx.notify();
                })),
            );

        bar = bar.child(left).child(right);

        v_col()
            .size_full()
            .min_h_0()
            .rounded(px(7.0))
            .border_1()
            .border_color(to_hsla(p.line_strong.composite_over(p.surface)))
            .overflow_hidden()
            .child(bar)
            .child(
                v_col()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .bg(to_hsla(p.surface))
                    .p(px(6.0))
                    .child(code_editor(&self.editor).readonly(true)),
            )
    }
}

fn looks_like_diff(text: &str) -> bool {
    text.starts_with("diff --git ")
        || (text.contains("\n@@ ") && (text.contains("--- ") || text.contains("+++ ")))
}

fn fence_code(text: &str, lang: &str) -> String {
    let mut fence = String::from("````");
    while text.contains(&fence) {
        fence.push('`');
    }
    format!("{fence}{lang}\n{text}\n{fence}")
}
