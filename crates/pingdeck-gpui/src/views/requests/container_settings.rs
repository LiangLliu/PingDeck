//! Collection and Folder Settings tab UI (General, Inherited Auth, Collection Variables, Scripts).

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::app::EnvironmentVariableDto;

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_icon, btn_primary, btn_secondary, checkbox,
    code_editor, field_row, h_row, icon_xs, multiline_input, palette, section_heading,
    tab_bar_segmented, tab_bar_underline, tab_item, text_input, to_hsla, v_col,
};

use super::model::{AuthKind, ContainerSection, OpenTab, ScriptSubTab};
use super::{RequestsView, default_auth_for_kind};

pub fn render_container_settings_tab(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let Some(OpenTab::Container(c)) = view.active_tab() else {
        return v_col().size_full();
    };
    let is_col = c.is_collection;
    let section = c.section;
    let ent = cx.entity().clone();

    let mut tabs = tab_bar_underline("container-set-tabs")
        .selected_index(section.index())
        .child(tab_item(if zh { "常规" } else { "General" }))
        .child(tab_item(if zh { "继承认证" } else { "Auth" }));
    if is_col {
        tabs = tabs.child(tab_item(if zh {
            format!("集合变量 ({})", c.variables.len())
        } else {
            format!("Variables ({})", c.variables.len())
        }));
    }
    tabs = tabs.child(tab_item(if zh { "脚本" } else { "Scripts" }));
    let tabs = tabs.on_click(move |idx, _, cx| {
        ent.update(cx, |this, cx| {
            this.snapshot_active_tab(cx);
            if let Some(OpenTab::Container(st)) = this.active_tab_mut() {
                st.section = if !st.is_collection && *idx == 2 {
                    ContainerSection::Scripts
                } else {
                    ContainerSection::from_index(*idx)
                };
            }
            cx.notify();
        });
    });

    let body: gpui_kit::AnyElement = match section {
        ContainerSection::General => v_col()
            .gap(px(10.0))
            .p(px(14.0))
            .child(field_row(
                if zh { "名称" } else { "Name" },
                None,
                div()
                    .w(px(320.0))
                    .child(text_input(&view.container_name_input)),
                cx,
            ))
            .child(section_heading(
                if zh {
                    "说明（Markdown）"
                } else {
                    "Description"
                },
                cx,
            ))
            .child(
                div()
                    .h(px(140.0))
                    .child(multiline_input(&view.container_desc_input)),
            )
            .into_any_element(),
        ContainerSection::Auth => {
            let auth_kind = AuthKind::from_auth(&c.auth);
            let ent_a = cx.entity().clone();
            v_col()
                .gap(px(10.0))
                .p(px(14.0))
                .child(section_heading(
                    if zh {
                        "此集合/文件夹下所有「继承」认证的请求将使用这里的配置"
                    } else {
                        "Shared authentication inherited by child requests"
                    },
                    cx,
                ))
                .child(
                    tab_bar_segmented("container-auth-seg")
                        .selected_index(auth_kind.index())
                        .child(tab_item(if zh { "继承" } else { "Inherit" }))
                        .child(tab_item(if zh { "无" } else { "None" }))
                        .child(tab_item("Bearer"))
                        .child(tab_item("Basic"))
                        .child(tab_item("API Key"))
                        .child(tab_item("OAuth 2.0"))
                        .child(tab_item("Digest"))
                        .child(tab_item("SigV4"))
                        .on_click(move |idx, _, cx| {
                            let next = default_auth_for_kind(AuthKind::from_index(*idx));
                            ent_a.update(cx, |this, cx| {
                                if let Some(OpenTab::Container(st)) = this.active_tab_mut() {
                                    st.auth = next;
                                    st.dirty = true;
                                }
                                cx.notify();
                            });
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(SharedString::from(format!("当前配置: {:?}", c.auth))),
                )
                .into_any_element()
        }
        ContainerSection::Variables => {
            let mut vlist = v_col()
                .gap(px(6.0))
                .p(px(14.0))
                .overflow_y_scrollbar()
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(
                            div()
                                .w(px(180.0))
                                .child(text_input(&view.container_var_key)),
                        )
                        .child(div().flex_1().child(text_input(&view.container_var_val)))
                        .child(
                            checkbox("col-var-secret", view.container_var_secret)
                                .label(if zh { "机密" } else { "Secret" })
                                .on_click(cx.listener(|this, ch: &bool, _, cx| {
                                    this.container_var_secret = *ch;
                                    cx.notify();
                                })),
                        )
                        .child(
                            btn_secondary("col-var-add", if zh { "添加" } else { "Add" }).on_click(
                                cx.listener(|this, _, window, cx| {
                                    let k =
                                        this.container_var_key.read(cx).value().trim().to_owned();
                                    let v = this.container_var_val.read(cx).value().to_string();
                                    let sec = this.container_var_secret;
                                    if k.is_empty() {
                                        return;
                                    }
                                    if let Some(OpenTab::Container(st)) = this.active_tab_mut() {
                                        st.variables.push(EnvironmentVariableDto {
                                            id: format!(
                                                "var-{}",
                                                pingdeck_desktop::core::app::current_time_ms()
                                            ),
                                            key: k,
                                            value: if sec { None } else { Some(v.clone()) },
                                            is_secret: sec,
                                            has_secret: sec && !v.is_empty(),
                                            secret_hint: None,
                                            enabled: true,
                                            note: String::new(),
                                        });
                                        st.pending_var_values.push(Some(v));
                                        st.dirty = true;
                                    }
                                    this.container_var_key
                                        .update(cx, |i, cx| i.set_value("", window, cx));
                                    this.container_var_val
                                        .update(cx, |i, cx| i.set_value("", window, cx));
                                    cx.notify();
                                }),
                            ),
                        ),
                );
            for (i, var) in c.variables.iter().enumerate() {
                let disp = if var.is_secret {
                    var.secret_hint
                        .clone()
                        .unwrap_or_else(|| "•••••• (0600)".into())
                } else {
                    var.value.clone().unwrap_or_default()
                };
                vlist =
                    vlist.child(
                        h_row()
                            .gap(px(8.0))
                            .px(px(8.0))
                            .py(px(4.0))
                            .rounded(px(5.0))
                            .bg(to_hsla(p.pane))
                            .child(
                                checkbox(SharedString::from(format!("cv-chk-{i}")), var.enabled)
                                    .on_click(cx.listener(move |this, ch: &bool, _, cx| {
                                        if let Some(OpenTab::Container(st)) = this.active_tab_mut()
                                            && let Some(row) = st.variables.get_mut(i)
                                        {
                                            row.enabled = *ch;
                                            st.dirty = true;
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .w(px(180.0))
                                    .text_xs()
                                    .child(SharedString::from(var.key.clone())),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(to_hsla(p.muted))
                                    .truncate()
                                    .child(SharedString::from(disp)),
                            )
                            .children(var.is_secret.then(|| {
                                badge(if zh { "机密" } else { "Secret" }, Tone::Warning, cx)
                            }))
                            .child(
                                btn_icon(SharedString::from(format!("cv-del-{i}")), UiIcon::Trash)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(OpenTab::Container(st)) = this.active_tab_mut()
                                            && i < st.variables.len()
                                        {
                                            st.variables.remove(i);
                                            if i < st.pending_var_values.len() {
                                                st.pending_var_values.remove(i);
                                            }
                                            st.dirty = true;
                                        }
                                        cx.notify();
                                    })),
                            ),
                    );
            }
            vlist.into_any_element()
        }
        ContainerSection::Scripts => {
            let sub = c.script_sub;
            let ent_s = cx.entity().clone();
            v_col()
                .size_full()
                .p(px(12.0))
                .gap(px(8.0))
                .child(
                    tab_bar_segmented("container-script-seg")
                        .selected_index(usize::from(sub == ScriptSubTab::Post))
                        .child(tab_item(if zh {
                            "前置脚本 (Pre)"
                        } else {
                            "Pre-request"
                        }))
                        .child(tab_item(if zh {
                            "后置脚本 (Post)"
                        } else {
                            "Post-response"
                        }))
                        .on_click(move |idx, _, cx| {
                            ent_s.update(cx, |this, cx| {
                                if let Some(OpenTab::Container(st)) = this.active_tab_mut() {
                                    st.script_sub = if *idx == 0 {
                                        ScriptSubTab::Pre
                                    } else {
                                        ScriptSubTab::Post
                                    };
                                }
                                cx.notify();
                            });
                        }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(code_editor(if sub == ScriptSubTab::Pre {
                            &view.container_pre_editor
                        } else {
                            &view.container_post_editor
                        })),
                )
                .into_any_element()
        }
    };

    v_col()
        .size_full()
        .w_full()
        .bg(to_hsla(p.surface))
        .child(
            h_row()
                .justify_between()
                .px(px(14.0))
                .py(px(8.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(
                    h_row()
                        .gap(px(8.0))
                        .child(icon_xs(UiIcon::Settings))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child(SharedString::from(if is_col {
                                    format!("集合设置 · {}", c.name)
                                } else {
                                    format!("文件夹设置 · {}", c.name)
                                })),
                        )
                        .children(c.dirty.then(|| {
                            badge(if zh { "未保存" } else { "Unsaved" }, Tone::Warning, cx)
                        })),
                )
                .child(
                    btn_primary(
                        "container-save",
                        if zh { "保存设置" } else { "Save settings" },
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.save_container_settings_tab(cx);
                    })),
                ),
        )
        .child(
            div()
                .px(px(12.0))
                .pt(px(4.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(tabs),
        )
        .child(div().flex_1().min_h_0().w_full().child(body))
}
