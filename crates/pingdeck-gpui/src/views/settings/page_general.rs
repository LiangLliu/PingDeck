//! Settings › General page: interface language, startup behavior, and background notifications.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::model::settings::{
    Language, NotifyWhenAway, SettingsSection as CoreSection,
};

use crate::ui::{
    Disableable as _, UiIcon, btn_ghost, icon_xs, setting_row, settings_section, tab_bar_segmented,
    tab_item, toggle_switch, v_col,
};

use super::SettingsView;

pub fn render_general(view: &SettingsView, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let general = &view.settings.general;
    let lang_idx = usize::from(general.language == Language::En);
    let notif_idx = match general.notify_when_away {
        NotifyWhenAway::Off => 0,
        NotifyWhenAway::Gates => 1,
        NotifyWhenAway::GatesAndFinished => 2,
    };

    let ent_lang = cx.entity().clone();
    let ent_notif = cx.entity().clone();

    v_col()
        .w_full()
        .gap(px(20.0))
        .child(settings_section(
            crate::ui::tr(cx).settings().page_general_language_startup(),
            Some(
                btn_ghost(
                    "gen-reset-sec",
                    crate::ui::tr(cx).settings().page_defaults_reset(),
                )
                .icon(icon_xs(UiIcon::RotateCcw))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.reset_section(CoreSection::General, cx);
                }))
                .into_any_element(),
            ),
            v_col()
                .w_full()
                .child(setting_row(
                    crate::ui::tr(cx).settings().f_general_language(),
                    None,
                    Some(SharedString::from(
                        crate::ui::tr(cx)
                            .settings()
                            .page_general_switch_ui_copy_immediately_ai(),
                    )),
                    tab_bar_segmented("gen-lang-seg")
                        .selected_index(lang_idx)
                        .child(tab_item(crate::ui::tr(cx).settings().language_zh_label()))
                        .child(tab_item("English"))
                        .on_click(move |idx, _, cx| {
                            let val = if *idx == 1 { "en" } else { "zh-CN" };
                            ent_lang.update(cx, |this, cx| {
                                this.patch_section(
                                    CoreSection::General,
                                    serde_json::json!({ "language": val }),
                                    cx,
                                );
                            });
                        }),
                    cx,
                ))
                .child(setting_row(
                    crate::ui::tr(cx)
                        .settings()
                        .page_general_restore_session_on_launch(),
                    None,
                    Some(SharedString::from(
                        crate::ui::tr(cx)
                            .settings()
                            .page_general_reopen_your_last_active_workspace(),
                    )),
                    toggle_switch("gen-restore", general.restore_session).on_change(cx.listener(
                        |this, checked: &bool, _, cx| {
                            this.patch_section(
                                CoreSection::General,
                                serde_json::json!({ "restore_session": *checked }),
                                cx,
                            );
                        },
                    )),
                    cx,
                ))
                .child(setting_row(
                    crate::ui::tr(cx)
                        .settings()
                        .page_general_check_for_updates_on_launch(),
                    Some(SharedString::from(
                        crate::ui::tr(cx).settings().page_data_off_by_default(),
                    )),
                    Some(SharedString::from(
                        crate::ui::tr(cx)
                            .settings()
                            .page_general_query_the_release_feed_on(),
                    )),
                    toggle_switch("gen-updates", general.check_updates_on_launch).on_change(
                        cx.listener(|this, checked: &bool, _, cx| {
                            this.patch_section(
                                CoreSection::General,
                                serde_json::json!({ "check_updates_on_launch": *checked }),
                                cx,
                            );
                        }),
                    ),
                    cx,
                )),
            cx,
        ))
        .child(settings_section(
            crate::ui::tr(cx)
                .settings()
                .page_general_background_notifications(),
            None,
            v_col()
                .w_full()
                .child(setting_row(
                    crate::ui::tr(cx)
                        .settings()
                        .page_general_notify_when_window_is_unfocused(),
                    None,
                    Some(SharedString::from(
                        crate::ui::tr(cx)
                            .settings()
                            .page_general_sent_only_while_pingdeck_is(),
                    )),
                    tab_bar_segmented("gen-notify-seg")
                        .selected_index(notif_idx)
                        .child(tab_item(crate::ui::tr(cx).settings().notify_off()))
                        .child(tab_item(
                            crate::ui::tr(cx).settings().page_general_gates_only(),
                        ))
                        .child(tab_item(
                            crate::ui::tr(cx).settings().page_general_gates_finished(),
                        ))
                        .on_click(move |idx, _, cx| {
                            let val = match *idx {
                                0 => "off",
                                2 => "gates_and_finished",
                                _ => "gates",
                            };
                            ent_notif.update(cx, |this, cx| {
                                this.patch_section(
                                    CoreSection::General,
                                    serde_json::json!({ "notify_when_away": val }),
                                    cx,
                                );
                            });
                        }),
                    cx,
                ))
                .child(setting_row(
                    crate::ui::tr(cx)
                        .settings()
                        .page_general_dock_attention_bounce(),
                    (general.notify_when_away == NotifyWhenAway::Off).then(|| {
                        SharedString::from(
                            crate::ui::tr(cx)
                                .settings()
                                .page_general_muted_by_setting_above(),
                        )
                    }),
                    Some(SharedString::from(
                        crate::ui::tr(cx)
                            .settings()
                            .page_general_bounce_the_dock_icon_once(),
                    )),
                    div().child(
                        toggle_switch("gen-dock-bounce", general.dock_bounce)
                            .disabled(general.notify_when_away == NotifyWhenAway::Off)
                            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                this.patch_section(
                                    CoreSection::General,
                                    serde_json::json!({ "dock_bounce": *checked }),
                                    cx,
                                );
                            })),
                    ),
                    cx,
                )),
            cx,
        ))
}
