//! Settings › Shortcuts page: searchable live-rebinding table for all 19 GPUI commands with
//! inline keystroke validation, conflict detection, and per-command / restore-all resets.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::model::settings::Language;

use crate::app::actions::{SHORTCUT_SPECS, ShortcutCategory, effective_key, format_key_display};
use crate::ui::{
    Disableable as _, Tone, UiIcon, badge, btn_ghost, btn_icon, btn_primary, btn_secondary, card,
    empty_inline, h_row, icon_xs, kbd_chip, palette, search_input, setting_row, settings_section,
    text_input, to_hsla, tr, v_col,
};

use super::SettingsView;

pub fn render_shortcuts(view: &SettingsView, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let p = palette(cx);
    let t = tr(cx);
    let lang = t.lang();
    let s = t.settings();
    let c = t.common();
    let overrides = &view.settings.shortcuts.overrides;
    let query = view
        .inputs
        .shortcut_filter
        .read(cx)
        .value()
        .trim()
        .to_lowercase();
    let custom_count = overrides.len();

    let mut root = v_col().w_full().gap(px(18.0)).child(
        h_row()
            .w_full()
            .justify_between()
            .gap(px(10.0))
            .child(
                div()
                    .w(px(280.0))
                    .child(search_input(&view.inputs.shortcut_filter, cx)),
            )
            .child(
                h_row()
                    .gap(px(8.0))
                    .children(
                        (custom_count > 0)
                            .then(|| badge(s.shortcut_customized(custom_count), Tone::Accent, cx)),
                    )
                    .child(
                        btn_secondary("sc-reset-all", s.shortcut_restore_all())
                            .icon(icon_xs(UiIcon::RotateCcw))
                            .disabled(custom_count == 0)
                            .on_click(cx.listener(|this, _, _, cx| this.reset_all_shortcuts(cx))),
                    ),
            ),
    );

    let mut total_visible = 0usize;
    for category in ShortcutCategory::ALL {
        let specs: Vec<_> = SHORTCUT_SPECS
            .iter()
            .filter(|sp| sp.category == category)
            .filter(|sp| {
                if query.is_empty() {
                    return true;
                }
                let active = effective_key(sp, overrides);
                sp.title(Language::ZhCn).to_lowercase().contains(&query)
                    || sp.title(Language::En).to_lowercase().contains(&query)
                    || sp.desc(Language::ZhCn).to_lowercase().contains(&query)
                    || sp.desc(Language::En).to_lowercase().contains(&query)
                    || sp.id.to_lowercase().contains(&query)
                    || active.to_lowercase().contains(&query)
                    || format_key_display(&active).to_lowercase().contains(&query)
            })
            .collect();
        if specs.is_empty() {
            continue;
        }
        total_visible += specs.len();

        let mut rows = v_col().w_full();

        for spec in specs {
            let active_key = effective_key(spec, overrides);
            let is_custom = overrides.contains_key(spec.id);
            let is_editing = view.editing_shortcut == Some(spec.id);
            let spec_id = spec.id;

            let control = if is_editing {
                h_row()
                    .gap(px(6.0))
                    .child(
                        div()
                            .w(px(165.0))
                            .child(text_input(&view.inputs.shortcut_key)),
                    )
                    .child(
                        btn_primary(SharedString::from(format!("sc-save-{spec_id}")), c.save())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.save_shortcut_override(spec_id, window, cx);
                            })),
                    )
                    .child(
                        btn_ghost(
                            SharedString::from(format!("sc-cancel-{spec_id}")),
                            c.cancel(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_edit_shortcut(cx))),
                    )
                    .into_any_element()
            } else {
                let mut row = h_row()
                    .gap(px(6.0))
                    .child(if is_custom {
                        badge(format_key_display(&active_key), Tone::Accent, cx)
                    } else {
                        kbd_chip(format_key_display(&active_key), cx)
                    })
                    .child(
                        btn_ghost(SharedString::from(format!("sc-edit-{spec_id}")), c.edit())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.start_edit_shortcut(spec_id, window, cx);
                            })),
                    );
                if is_custom {
                    row = row.child(
                        btn_icon(
                            SharedString::from(format!("sc-reset-{spec_id}")),
                            UiIcon::RotateCcw,
                        )
                        .tooltip(s.shortcut_restore_default())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.reset_shortcut_override(spec_id, cx);
                        })),
                    );
                }
                row.into_any_element()
            };

            rows = rows.child(setting_row(
                spec.title(lang),
                is_custom.then(|| SharedString::from(c.custom())),
                Some(SharedString::from(spec.desc(lang))),
                control,
                cx,
            ));
        }
        root = root.child(settings_section(category.label(lang), None, rows, cx));
    }

    if total_visible == 0 {
        root = root.child(
            card(cx).p(px(20.0)).rounded(px(10.0)).child(empty_inline(
                s.shortcut_empty_title(),
                Some(SharedString::from(s.shortcut_empty_desc())),
                Some(
                    btn_secondary("sc-clear-filter", s.shortcut_clear_filter())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.inputs
                                .shortcut_filter
                                .update(cx, |i, cx| i.set_value("", window, cx));
                            cx.notify();
                        }))
                        .into_any_element(),
                ),
                cx,
            )),
        );
    } else {
        root = root.child(
            div()
                .px(px(4.0))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(s.shortcut_syntax_hint()),
        );
    }

    root
}
