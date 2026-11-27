//! Translates SpanBay's framework-agnostic [`ResolvedSyntaxPalette`] + [`ResolvedPalette`] into
//! `gpui-component`'s Zed-compatible [`HighlightTheme`] so every `code_editor`, `PayloadViewer`,
//! `payload_block`, `TextView::markdown`, and diff view renders cohesive syntax highlighting
//! harmonized with the active eye-care surface palette.

use std::sync::Arc;

use gpui_kit::component::highlighter::HighlightTheme;
use serde_json::{Map, Value, json};
use spanbay_desktop::theme::{ResolvedPalette, ResolvedSyntaxPalette, RgbaColor};

fn hex_rgb(c: RgbaColor) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

fn hex_rgba(c: RgbaColor) -> String {
    let a = (c.a.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}{a:02X}", c.r, c.g, c.b)
}

fn tok(c: RgbaColor) -> Value {
    json!({ "color": hex_rgb(c) })
}

fn tok_italic(c: RgbaColor) -> Value {
    json!({ "color": hex_rgb(c), "font_style": "italic" })
}

fn tok_semibold(c: RgbaColor) -> Value {
    json!({ "color": hex_rgb(c), "font_weight": 600 })
}

/// Builds a complete [`HighlightTheme`] whose editor surfaces blend seamlessly with `palette` and
/// whose syntax tokens follow `syntax`.
#[must_use]
pub fn build_highlight_theme(
    palette: &ResolvedPalette,
    syntax: &ResolvedSyntaxPalette,
) -> Arc<HighlightTheme> {
    let active_line = palette.hover.composite_over(palette.surface);
    let invisible = RgbaColor::rgba(palette.faint.r, palette.faint.g, palette.faint.b, 0.45);
    let appearance = if palette.is_dark { "dark" } else { "light" };
    let name = format!(
        "SpanBay {} ({})",
        syntax.id.id(),
        syntax.id.variant_pair(palette.is_dark)
    );

    let mut syn_map = Map::new();
    for (k, v) in [
        ("attribute", tok(syntax.attribute)),
        ("boolean", tok_semibold(syntax.boolean)),
        ("comment", tok_italic(syntax.comment)),
        ("comment.doc", tok_italic(syntax.comment)),
        ("constant", tok(syntax.boolean)),
        ("constructor", tok(syntax.function)),
        ("embedded", tok(palette.ink)),
        ("emphasis", json!({ "font_style": "italic" })),
        ("emphasis.strong", json!({ "font_weight": 700 })),
        ("enum", tok(syntax.type_name)),
        ("function", tok(syntax.function)),
        ("hint", tok(syntax.comment)),
        ("keyword", tok_semibold(syntax.keyword)),
        ("label", tok(syntax.attribute)),
        ("link_text", tok(palette.accent_ink)),
        ("link_uri", tok_italic(palette.accent_ink)),
        ("number", tok(syntax.number)),
        ("operator", tok(syntax.operator)),
        ("predictive", tok(palette.faint)),
        ("preproc", tok(syntax.keyword)),
        ("primary", tok(palette.ink)),
        ("property", tok(syntax.key)),
        ("punctuation", tok(syntax.punctuation)),
        ("punctuation.bracket", tok(syntax.punctuation)),
        ("punctuation.delimiter", tok(syntax.punctuation)),
        ("punctuation.list_marker", tok(palette.accent_ink)),
        ("punctuation.special", tok(syntax.operator)),
        ("string", tok(syntax.string)),
        ("string.escape", tok(syntax.escape)),
        ("string.regex", tok(syntax.escape)),
        ("string.special", tok(syntax.escape)),
        ("string.special.symbol", tok(syntax.key)),
        ("tag", tok(syntax.tag)),
        ("tag.doctype", tok_italic(syntax.comment)),
        ("text.code.span", tok(syntax.string)),
        ("text.literal", tok(syntax.string)),
        ("title", tok_semibold(syntax.key)),
        ("type", tok(syntax.type_name)),
        ("variable", tok(syntax.variable)),
        ("variable.special", tok(syntax.boolean)),
        ("variant", tok(syntax.type_name)),
    ] {
        syn_map.insert(k.to_owned(), v);
    }

    let mut style_map = Map::new();
    for (k, v) in [
        ("editor.background", Value::String(hex_rgb(palette.surface))),
        (
            "editor.gutter.background",
            Value::String(hex_rgb(palette.pane)),
        ),
        ("editor.foreground", Value::String(hex_rgb(palette.ink))),
        (
            "editor.active_line.background",
            Value::String(hex_rgb(active_line)),
        ),
        ("editor.line_number", Value::String(hex_rgb(palette.faint))),
        (
            "editor.active_line_number",
            Value::String(hex_rgb(palette.ink_2)),
        ),
        ("editor.invisible", Value::String(hex_rgba(invisible))),
        ("error", Value::String(hex_rgb(palette.bad))),
        ("warning", Value::String(hex_rgb(palette.warn))),
        ("info", Value::String(hex_rgb(palette.accent))),
        ("success", Value::String(hex_rgb(palette.ok))),
        ("hint", Value::String(hex_rgb(palette.accent_ink))),
        ("created", Value::String(hex_rgb(palette.diff_add))),
        ("modified", Value::String(hex_rgb(palette.warn))),
        ("deleted", Value::String(hex_rgb(palette.diff_del))),
        ("syntax", Value::Object(syn_map)),
    ] {
        style_map.insert(k.to_owned(), v);
    }

    let payload = json!({
        "name": name,
        "appearance": appearance,
        "style": Value::Object(style_map),
    });

    match serde_json::from_value::<HighlightTheme>(payload) {
        Ok(theme) => Arc::new(theme),
        Err(_) => {
            if palette.is_dark {
                HighlightTheme::default_dark()
            } else {
                HighlightTheme::default_light()
            }
        }
    }
}
