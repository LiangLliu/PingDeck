//! 3-tab Workspace Import/Export view inside `UtilityPanel`:
//! 1. Import (cURL / Postman / OpenAPI / HAR / Insomnia / Environment) with preview & real save
//! 2. Sanitized Export (Postman v2.1 Collection or Environment JSON without plaintext secrets)
//! 3. Plaintext Export with Secrets (danger banner + mandatory acknowledgment checkbox).

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, Styled as _, div, px,
};
use pingdeck_desktop::core::app::ImportConflict;

use crate::ui::{
    Disableable as _, ScrollableElement as _, Tone, UiIcon, alert_banner, badge, btn_danger,
    btn_primary, btn_secondary, card, checkbox, choice_chip, h_row, icon_xs, multiline_input,
    palette, section_heading, tab_bar_segmented, tab_item, to_hsla, tr, v_col,
};

use super::UtilityPanel;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IoTab {
    #[default]
    Import,
    SanitizedExport,
    SecretsExport,
}

impl IoTab {
    pub const fn index(self) -> usize {
        match self {
            Self::Import => 0,
            Self::SanitizedExport => 1,
            Self::SecretsExport => 2,
        }
    }

    pub const fn from_index(i: usize) -> Self {
        match i {
            1 => Self::SanitizedExport,
            2 => Self::SecretsExport,
            _ => Self::Import,
        }
    }
}

const IMPORT_FORMATS: [(&str, &str); 6] = [
    ("curl", "cURL"),
    ("postman", "Postman"),
    ("openapi", "OpenAPI"),
    ("har", "HAR"),
    ("insomnia", "Insomnia"),
    ("environment", "Env JSON"),
];

pub fn render_import_export(panel: &UtilityPanel, cx: &mut Context<UtilityPanel>) -> AnyElement {
    let p = palette(cx);
    let u = tr(cx).utility();
    let tabs = tab_bar_segmented("util-io-tabs")
        .selected_index(panel.io_tab.index())
        .child(tab_item(u.io_tab_import()))
        .child(tab_item(u.io_tab_sanitized()))
        .child(tab_item(u.io_tab_secrets()))
        .on_click(cx.listener(|this, idx: &usize, _, cx| {
            this.io_tab = IoTab::from_index(*idx);
            cx.notify();
        }));

    let body: AnyElement = match panel.io_tab {
        IoTab::Import => render_import_tab(panel, cx),
        IoTab::SanitizedExport => render_export_tab(panel, false, cx),
        IoTab::SecretsExport => render_export_tab(panel, true, cx),
    };

    v_col()
        .size_full()
        .gap(px(12.0))
        .child(h_row().w_full().justify_between().child(tabs))
        .child(
            div()
                .id("util-io-scroll")
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_y_scrollbar()
                .child(body),
        )
        .text_color(to_hsla(p.ink))
        .into_any_element()
}

fn render_import_tab(panel: &UtilityPanel, cx: &mut Context<UtilityPanel>) -> AnyElement {
    let p = palette(cx);
    let u = tr(cx).utility();
    let mut fmt_row = h_row().gap(px(5.0)).flex_wrap();
    for (id, label) in IMPORT_FORMATS {
        let active = panel.io_format == id;
        let id_s = id.to_owned();
        fmt_row = fmt_row.child(
            choice_chip(
                SharedString::from(format!("uio-fmt-{id}")),
                label,
                active,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.io_format = id_s.clone();
                this.io_preview = None;
                cx.notify();
            })),
        );
    }

    let mut conflict_row = h_row().gap(px(5.0)).child(
        div()
            .text_xs()
            .text_color(to_hsla(p.muted))
            .child(u.io_conflicts_label()),
    );
    for (c, id_tag, label) in [
        (ImportConflict::Update, "update", u.io_conflict_update()),
        (
            ImportConflict::KeepBoth,
            "keep_both",
            u.io_conflict_keep_both(),
        ),
        (ImportConflict::Skip, "skip", u.io_conflict_skip()),
    ] {
        conflict_row = conflict_row.child(
            choice_chip(
                SharedString::from(format!("uio-cf-{id_tag}")),
                label,
                panel.io_conflict == c,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.io_conflict = c;
                cx.notify();
            })),
        );
    }

    let mut col = v_col()
        .w_full()
        .gap(px(10.0))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(8.0))
                .child(fmt_row)
                .child(
                    btn_secondary("uio-pick-file", u.io_load_file())
                        .icon(icon_xs(UiIcon::Upload))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.pick_import_file(window, cx);
                        })),
                ),
        )
        .child(multiline_input(&panel.curl_input))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(8.0))
                .child(conflict_row)
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(
                            btn_secondary("uio-preview", u.io_preview_btn())
                                .on_click(cx.listener(|this, _, _, cx| this.run_io_preview(cx))),
                        )
                        .child(
                            btn_primary("uio-commit", u.io_commit_btn())
                                .on_click(cx.listener(|this, _, _, cx| this.run_io_import(cx))),
                        ),
                ),
        );

    if let Some(prev) = &panel.io_preview {
        let mut pcard = card(cx)
            .p(px(12.0))
            .gap(px(6.0))
            .child(section_heading(u.io_preview_heading(), cx))
            .child(
                h_row()
                    .gap(px(6.0))
                    .flex_wrap()
                    .child(badge(prev.format.clone(), Tone::Accent, cx))
                    .children(
                        prev.name
                            .as_ref()
                            .map(|n| badge(SharedString::from(n.clone()), Tone::Neutral, cx)),
                    )
                    .children(
                        prev.requests
                            .map(|n| badge(u.io_preview_requests(n), Tone::Success, cx)),
                    )
                    .children(
                        prev.folders
                            .map(|n| badge(u.io_preview_folders(n), Tone::Neutral, cx)),
                    )
                    .children(
                        prev.variables
                            .map(|n| badge(u.io_preview_vars(n), Tone::Neutral, cx)),
                    ),
            );
        if !prev.warnings.is_empty() {
            pcard = pcard.child(alert_banner(
                Tone::Warning,
                prev.warnings.join(" · "),
                None,
                cx,
            ));
        }
        col = col.child(pcard);
    }

    col.into_any_element()
}

fn render_export_tab(
    panel: &UtilityPanel,
    include_secrets: bool,
    cx: &mut Context<UtilityPanel>,
) -> AnyElement {
    let p = palette(cx);
    let u = tr(cx).utility();
    let can_export = !include_secrets || panel.io_ack_secrets;
    let mut col = v_col().w_full().gap(px(12.0));

    if include_secrets {
        col = col
            .child(alert_banner(
                Tone::Danger,
                u.io_export_secrets_warning(),
                None,
                cx,
            ))
            .child(
                h_row()
                    .gap(px(8.0))
                    .child(
                        checkbox("uio-exp-ack", panel.io_ack_secrets).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.io_ack_secrets = !this.io_ack_secrets;
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.ink))
                            .child(u.io_export_secrets_ack()),
                    ),
            );
    } else {
        col = col.child(alert_banner(
            Tone::Success,
            u.io_export_sanitized_notice(),
            None,
            cx,
        ));
    }

    let mut col_chips = h_row().gap(px(5.0)).flex_wrap();
    for c in &panel.collections {
        let cid = c.id.as_str().to_owned();
        let active = panel.io_export_col_id.as_deref() == Some(c.id.as_str());
        col_chips = col_chips.child(
            choice_chip(
                SharedString::from(format!("uio-ecol-{include_secrets}-{}", c.id.as_str())),
                c.name.clone(),
                active,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.io_export_col_id = Some(cid.clone());
                cx.notify();
            })),
        );
    }
    let has_col = panel.io_export_col_id.is_some();
    col = col.child(
        card(cx)
            .p(px(14.0))
            .gap(px(8.0))
            .child(section_heading(u.io_export_col_heading(), cx))
            .child(if panel.collections.is_empty() {
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(u.io_export_no_cols())
            } else {
                col_chips
            })
            .child(if include_secrets {
                btn_danger("uio-exp-col-sec", u.io_export_col_plaintext())
                    .disabled(!can_export || !has_col)
                    .on_click(cx.listener(|this, _, _, cx| this.export_collection(true, cx)))
            } else {
                btn_primary("uio-exp-col-safe", u.io_export_col_sanitized())
                    .disabled(!has_col)
                    .on_click(cx.listener(|this, _, _, cx| this.export_collection(false, cx)))
            }),
    );

    let mut env_chips = h_row().gap(px(5.0)).flex_wrap();
    for e in &panel.environments {
        let eid = e.id.clone();
        let active = panel.io_export_env_id.as_deref() == Some(e.id.as_str());
        env_chips = env_chips.child(
            choice_chip(
                SharedString::from(format!("uio-eenv-{include_secrets}-{}", e.id)),
                e.name.clone(),
                active,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.io_export_env_id = Some(eid.clone());
                cx.notify();
            })),
        );
    }
    let has_env = panel.io_export_env_id.is_some();
    col = col.child(
        card(cx)
            .p(px(14.0))
            .gap(px(8.0))
            .child(section_heading(u.io_export_env_heading(), cx))
            .child(if panel.environments.is_empty() {
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(u.io_export_no_envs())
            } else {
                env_chips
            })
            .child(if include_secrets {
                btn_danger("uio-exp-env-sec", u.io_export_env_plaintext())
                    .disabled(!can_export || !has_env)
                    .on_click(cx.listener(|this, _, _, cx| this.export_environment(true, cx)))
            } else {
                btn_primary("uio-exp-env-safe", u.io_export_env_sanitized())
                    .disabled(!has_env)
                    .on_click(cx.listener(|this, _, _, cx| this.export_environment(false, cx)))
            }),
    );

    col.into_any_element()
}
