//! Modal dialogs owned by [`SettingsView`]: Reset-All confirmation, Folder Sync secret export
//! confirmation (with Git/Cloud typed-phrase safeguard), Sync Import preview, and storage cleanups.

use gpui_kit::{
    AnyElement, App, ClickEvent, Context, IntoElement as _, ParentElement as _, SharedString,
    Styled as _, Window, div, px,
};
use spanbay_desktop::core::app::SyncFolderKind;
use spanbay_desktop::core::sync::SyncKind;

use crate::ui::{
    Disableable as _, ScrollableElement as _, Tone, alert_banner, badge, btn_danger, btn_primary,
    btn_secondary, data_row, h_row, modal_dialog, palette, text_input, to_hsla, toggle_switch,
    v_col,
};

use super::SettingsView;
use super::state_inputs::{SettingsModal, format_bytes};

pub fn render_settings_modal(
    view: &SettingsView,
    window: &Window,
    cx: &mut Context<SettingsView>,
) -> Option<AnyElement> {
    let modal = view.modal.as_ref()?;
    let p = palette(cx);

    let entity = cx.entity().clone();
    let on_close = move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
        entity.update(cx, |this, cx| {
            this.modal = None;
            cx.notify();
        });
    };

    match modal {
        SettingsModal::ConfirmResetAll => {
            let body = v_col()
                .gap(px(10.0))
                .child(alert_banner(
                    Tone::Warning,
                    crate::ui::tr(cx)
                        .settings()
                        .modals_every_settings_section_will_return(),
                    None,
                    cx,
                ))
                .child(
                    div().text_xs().text_color(to_hsla(p.ink_2)).child(
                        crate::ui::tr(cx)
                            .settings()
                            .modals_preserved_all_workspaces_collections_chats(),
                    ),
                )
                .into_any_element();

            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("modal-reset-cancel", crate::ui::tr(cx).common().cancel())
                        .on_click(on_close.clone()),
                )
                .child(
                    btn_danger(
                        "modal-reset-confirm",
                        crate::ui::tr(cx).settings().modals_reset_all_settings(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.modal = None;
                        this.reset_all(cx);
                    })),
                )
                .into_any_element();

            Some(modal_dialog(
                "settings-reset-all-modal",
                crate::ui::tr(cx).settings().modals_reset_all_settings_2(),
                460.0,
                body,
                footer,
                window,
                cx,
                on_close,
            ))
        }

        SettingsModal::ConfirmSyncSecrets {
            kind,
            cloud_name,
            directory,
        } => {
            let risky = !matches!(kind, SyncFolderKind::Plain);
            let typed = view
                .inputs
                .sync_secrets_confirm
                .read(cx)
                .value()
                .trim()
                .to_owned();
            let phrase_ok = !risky
                || typed.eq_ignore_ascii_case("INCLUDE SECRETS")
                || typed == crate::ui::tr(cx).settings().modals_secrets_phrase_zh();

            let place_desc = match kind {
                SyncFolderKind::Git => crate::ui::tr(cx)
                    .settings()
                    .modals_this_folder_is_inside_a(),
                SyncFolderKind::Cloud => crate::ui::tr(cx)
                    .settings()
                    .modals_this_folder_is_inside_a_2(),
                SyncFolderKind::Plain => crate::ui::tr(cx)
                    .settings()
                    .modals_exports_will_write_plaintext_environment(),
            };

            let mut body_col = v_col()
                .gap(px(10.0))
                .child(alert_banner(
                    if risky { Tone::Danger } else { Tone::Warning },
                    place_desc,
                    None,
                    cx,
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(SharedString::from(match cloud_name {
                            Some(name) => crate::ui::tr(cx)
                                .settings()
                                .modals_folder_with_cloud(directory, name),
                            None => crate::ui::tr(cx).settings().modals_folder_plain(directory),
                        })),
                );

            if risky {
                body_col = body_col
                    .child(
                        div().text_xs().text_color(to_hsla(p.ink_2)).child(
                            crate::ui::tr(cx)
                                .settings()
                                .modals_auto_export_refuses_writing_secrets(),
                        ),
                    )
                    .child(text_input(&view.inputs.sync_secrets_confirm));
            }

            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("modal-sec-cancel", crate::ui::tr(cx).common().cancel())
                        .on_click(on_close.clone()),
                )
                .child(
                    btn_danger(
                        "modal-sec-confirm",
                        crate::ui::tr(cx).settings().modals_include_secrets(),
                    )
                    .disabled(!phrase_ok)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_sync_include_secrets(cx);
                    })),
                )
                .into_any_element();

            Some(modal_dialog(
                "settings-sync-secrets-modal",
                crate::ui::tr(cx)
                    .settings()
                    .modals_include_plaintext_secrets_in_sync(),
                500.0,
                body_col.into_any_element(),
                footer,
                window,
                cx,
                on_close,
            ))
        }

        SettingsModal::SyncImportPreview {
            preview,
            apply_removals,
        } => {
            let mut body_col = v_col().gap(px(10.0)).child(
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(SharedString::from(
                        crate::ui::tr(cx)
                            .settings()
                            .modals_source_folder(&preview.directory),
                    )),
            );

            if preview.counts.is_empty() {
                body_col = body_col.child(alert_banner(
                    Tone::Success,
                    crate::ui::tr(cx)
                        .settings()
                        .modals_local_data_already_matches_the(),
                    None,
                    cx,
                ));
            } else {
                let mut rows = v_col().gap(px(4.0)).max_h(px(220.0)).overflow_y_scrollbar();
                for count in &preview.counts {
                    rows = rows.child(
                        data_row(cx)
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(p.ink))
                                    .child(sync_kind_label(
                                        count.kind,
                                        crate::ui::active_language(cx),
                                    )),
                            )
                            .child(
                                h_row()
                                    .gap(px(6.0))
                                    .children((count.added > 0).then(|| {
                                        badge(format!("+{}", count.added), Tone::Success, cx)
                                    }))
                                    .children((count.changed > 0).then(|| {
                                        badge(format!("~{}", count.changed), Tone::Accent, cx)
                                    }))
                                    .children((count.removed > 0).then(|| {
                                        badge(format!("-{}", count.removed), Tone::Warning, cx)
                                    }))
                                    .children((count.conflicts > 0).then(|| {
                                        badge(
                                            crate::ui::tr(cx)
                                                .settings()
                                                .modals_kept_local(count.conflicts),
                                            Tone::Danger,
                                            cx,
                                        )
                                    })),
                            ),
                    );
                }
                body_col = body_col.child(rows);
            }

            if !preview.warnings.is_empty() {
                body_col = body_col.child(alert_banner(
                    Tone::Warning,
                    preview.warnings.join(" · "),
                    None,
                    cx,
                ));
            }

            let has_removals = preview.counts.iter().any(|c| c.removed > 0);
            if has_removals {
                let checked = *apply_removals;
                body_col = body_col.child(
                    h_row()
                        .justify_between()
                        .gap(px(12.0))
                        .pt(px(4.0))
                        .child(
                            div().flex_1().text_xs().text_color(to_hsla(p.ink_2)).child(
                                crate::ui::tr(cx)
                                    .settings()
                                    .modals_apply_removals_locally_off_by(),
                            ),
                        )
                        .child(toggle_switch("sync-import-removals", checked).on_change(
                            cx.listener(|this, val: &bool, _, cx| {
                                if let Some(SettingsModal::SyncImportPreview {
                                    apply_removals,
                                    ..
                                }) = &mut this.modal
                                {
                                    *apply_removals = *val;
                                    cx.notify();
                                }
                            }),
                        )),
                );
            }

            let can_import = !preview.counts.is_empty();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("modal-imp-cancel", crate::ui::tr(cx).common().close())
                        .on_click(on_close.clone()),
                )
                .child(
                    btn_primary(
                        "modal-imp-confirm",
                        crate::ui::tr(cx).settings().modals_import_now(),
                    )
                    .disabled(!can_import || view.sync_busy)
                    .on_click(cx.listener(|this, _, _, cx| this.confirm_sync_import(cx))),
                )
                .into_any_element();

            Some(modal_dialog(
                "settings-sync-import-modal",
                crate::ui::tr(cx).settings().modals_preview_sync_import(),
                540.0,
                body_col.into_any_element(),
                footer,
                window,
                cx,
                on_close,
            ))
        }

        SettingsModal::ConfirmClearAttachments {
            older_than_days,
            report,
        } => {
            let scope = match older_than_days {
                Some(d) => crate::ui::tr(cx).settings().modals_att_scope_days(d),
                None => crate::ui::tr(cx)
                    .settings()
                    .modals_att_scope_all()
                    .to_owned(),
            };
            let summary = crate::ui::tr(cx)
                .settings()
                .modals_this_will_remove_attachment_blob(
                    report.removed,
                    format_bytes(report.freed_bytes),
                    report.chats,
                );

            let body = v_col()
                .gap(px(10.0))
                .child(alert_banner(
                    if report.removed > 0 {
                        Tone::Warning
                    } else {
                        Tone::Neutral
                    },
                    if report.removed > 0 {
                        summary
                    } else {
                        crate::ui::tr(cx)
                            .settings()
                            .modals_no_clearable_found(scope)
                    },
                    None,
                    cx,
                ))
                .into_any_element();

            let days = *older_than_days;
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("modal-att-cancel", crate::ui::tr(cx).common().cancel())
                        .on_click(on_close.clone()),
                )
                .child(
                    btn_danger(
                        "modal-att-confirm",
                        crate::ui::tr(cx).settings().modals_clear_blobs(),
                    )
                    .disabled(report.removed == 0 || view.data_busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.confirm_clear_attachments(days, cx);
                    })),
                )
                .into_any_element();

            Some(modal_dialog(
                "settings-clear-att-modal",
                crate::ui::tr(cx).settings().modals_clear_chat_attachments(),
                460.0,
                body,
                footer,
                window,
                cx,
                on_close,
            ))
        }

        SettingsModal::ConfirmClearCapturedBodies { count, bytes } => {
            let body = v_col()
                .gap(px(10.0))
                .child(alert_banner(
                    Tone::Warning,
                    crate::ui::tr(cx)
                        .settings()
                        .modals_clears_captured_gateway_body_record(count, format_bytes(*bytes)),
                    None,
                    cx,
                ))
                .into_any_element();

            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("modal-cap-cancel", crate::ui::tr(cx).common().cancel())
                        .on_click(on_close.clone()),
                )
                .child(
                    btn_danger(
                        "modal-cap-confirm",
                        crate::ui::tr(cx).settings().modals_clear_bodies(),
                    )
                    .disabled(view.data_busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_clear_captured_bodies(cx);
                    })),
                )
                .into_any_element();

            Some(modal_dialog(
                "settings-clear-cap-modal",
                crate::ui::tr(cx)
                    .settings()
                    .modals_clear_captured_gateway_bodies(),
                460.0,
                body,
                footer,
                window,
                cx,
                on_close,
            ))
        }
    }
}

fn sync_kind_label(
    kind: SyncKind,
    lang: spanbay_desktop::core::model::settings::Language,
) -> &'static str {
    let s = crate::ui::Tr(lang).settings();
    match kind {
        SyncKind::Workspace => s.sync_kind_workspace(),
        SyncKind::Collection => s.sync_kind_collection(),
        SyncKind::LooseRequests => s.sync_kind_loose_requests(),
        SyncKind::Environment => s.sync_kind_environment(),
        SyncKind::Globals => s.sync_kind_globals(),
        SyncKind::Mock => s.sync_kind_mock(),
        SyncKind::Flow => s.sync_kind_flow(),
        SyncKind::LlmProvider => s.sync_kind_llm_provider(),
        SyncKind::LlmAlias => s.sync_kind_llm_alias(),
        SyncKind::Settings => s.sync_kind_settings(),
        SyncKind::Skills => s.sync_kind_skills(),
        SyncKind::McpServer => s.sync_kind_mcp_server(),
    }
}
