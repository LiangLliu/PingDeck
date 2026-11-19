//! Welcome empty state with clickable prompt starter cards for a fresh AI Chat thread.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};

use crate::ui::{UiIcon, app_icon, h_row, icon_xs, palette, to_hsla, v_col};

use super::ChatView;

pub fn render_welcome_empty(zh: bool, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let starters: [(&str, &str, UiIcon); 3] = if zh {
        [
            (
                "调试最近失败的接口请求",
                "检查最近一次接口请求失败的原因，分析响应头和状态码并给出修复建议。",
                UiIcon::Send,
            ),
            (
                "梳理当前项目架构与关键文件",
                "阅读当前绑定目录的结构，总结核心模块边界和入口点。",
                UiIcon::FolderOpen,
            ),
            (
                "生成接口测试用例与断言",
                "为当前工作区的核心 API 设计边界用例和自动化断言规则。",
                UiIcon::ListChecks,
            ),
        ]
    } else {
        [
            (
                "Debug the latest failing request",
                "Inspect why the latest API request failed, explain headers/status, and propose a fix.",
                UiIcon::Send,
            ),
            (
                "Explore workspace architecture",
                "Read the bound project directory and summarize key modules and entry points.",
                UiIcon::FolderOpen,
            ),
            (
                "Generate API assertions & tests",
                "Design edge-case test assertions and validation checks for my core workspace APIs.",
                UiIcon::ListChecks,
            ),
        ]
    };

    let mut grid = h_row().flex_wrap().gap(px(10.0)).pt(px(8.0));
    for (i, (title, prompt, glyph)) in starters.into_iter().enumerate() {
        grid = grid.child(
            div()
                .id(SharedString::from(format!("chat-starter-{i}")))
                .flex_1()
                .min_w(px(200.0))
                .p(px(12.0))
                .rounded(px(10.0))
                .bg(to_hsla(p.pane))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .cursor_pointer()
                .hover(|s| s.border_color(to_hsla(p.accent_line.composite_over(p.pane))))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.apply_starter(prompt, window, cx);
                }))
                .child(
                    v_col()
                        .gap(px(6.0))
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .child(div().text_color(to_hsla(p.accent)).child(icon_xs(glyph)))
                                .child(title),
                        )
                        .child(div().text_xs().text_color(to_hsla(p.muted)).child(prompt)),
                ),
        );
    }

    v_col()
        .items_center()
        .pt(px(52.0))
        .gap(px(12.0))
        .child(app_icon(52.0))
        .child(
            div()
                .text_base()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(to_hsla(p.ink))
                .child(if zh {
                    "有什么可以帮你？"
                } else {
                    "How can PingDeck help?"
                }),
        )
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .max_w(px(460.0))
                .child(if zh {
                    "原生流式 Markdown 划选复制，支持工具调用追踪、内联代码 Diff 预览、风险门控审批与本地 OS 沙箱隔离。"
                } else {
                    "Native selectable streaming Markdown with tool traces, inline diffs, risk-gated approvals, and OS sandbox isolation."
                }),
        )
        .child(div().w_full().pt(px(12.0)).child(grid))
}
