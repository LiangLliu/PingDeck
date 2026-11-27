//! Shared sidebar section heading (`group_label`) and Recently Deleted (`最近删除`) list.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};

use crate::ui::{
    Disableable as _, ScrollableElement as _, StatusShape, btn_ghost, btn_secondary, empty_inline,
    h_row, palette, section_heading, status_indicator, to_hsla, v_col,
};

use super::ChatView;

pub fn group_label(
    title: &'static str,
    count: Option<usize>,
    shape: Option<StatusShape>,
    cx: &gpui_kit::App,
) -> gpui_kit::Div {
    let p = palette(cx);
    h_row()
        .justify_between()
        .px(px(8.0))
        .pt(px(8.0))
        .pb(px(3.0))
        .child(
            h_row()
                .gap(px(5.0))
                .children(shape.map(|s| status_indicator(s, cx)))
                .child(section_heading(title, cx)),
        )
        .children(count.map(|c| {
            div()
                .text_xs()
                .text_color(to_hsla(p.faint))
                .child(SharedString::from(c.to_string()))
        }))
}

pub fn render_trash_list(view: &ChatView, cx: &mut Context<ChatView>) -> AnyElement {
    let p = palette(cx);
    let mut list = v_col().w_full().gap(px(4.0)).p(px(8.0));
    if view.trash_sessions.is_empty() {
        list = list.child(empty_inline(
            crate::ui::tr(cx).chat().sidebar_board_trash_is_empty(),
            None,
            None,
            cx,
        ));
    } else {
        for s in &view.trash_sessions {
            let sid = s.id.clone();
            list = list.child(
                h_row()
                    .justify_between()
                    .gap(px(6.0))
                    .px(px(8.0))
                    .py(px(6.0))
                    .rounded(px(7.0))
                    .bg(to_hsla(p.surface))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(s.title.clone()),
                    )
                    .child(
                        btn_ghost(
                            SharedString::from(format!("chat-tr-res-{}", s.id)),
                            crate::ui::tr(cx).chat().pane_deleted_restore(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.restore_session(sid.clone(), cx);
                        })),
                    ),
            );
        }
        if view.trash_next_cursor.is_some() {
            list = list.child(
                h_row().w_full().justify_center().pt(px(6.0)).child(
                    btn_secondary(
                        "chat-trash-load-more",
                        if view.trash_loading_more {
                            crate::ui::tr(cx).common().loading()
                        } else {
                            crate::ui::tr(cx).chat().sidebar_board_load_more_trash()
                        },
                    )
                    .disabled(view.trash_loading_more)
                    .on_click(cx.listener(|this, _, _, cx| this.load_more_trash(cx))),
                ),
            );
        }
    }
    div()
        .id("chat-trash-scroll")
        .size_full()
        .overflow_y_scrollbar()
        .child(list)
        .into_any_element()
}
