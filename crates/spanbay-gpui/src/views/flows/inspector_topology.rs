//! Topology, Swimlane, Upstream Dependencies, and Multi-Exit Conditional Branches inspector tab
//! for `FlowsView`.

use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
};
use spanbay_desktop::core::model::items::{FlowStep, FlowStepKind};

use crate::ui::{
    Tone, UiIcon, badge, btn_icon, btn_secondary, card, choice_chip, data_row, empty_inline, h_row,
    icon_xs, meta_pill, palette, section_header, text_input, to_hsla, v_col,
};

use super::state::FlowsView;

pub fn render_topology_tab(
    view: &FlowsView,
    step: &FlowStep,
    cx: &mut Context<FlowsView>,
) -> AnyElement {
    let p = palette(cx);
    let fl = crate::ui::Tr(view.lang).flows();
    let Some(flow) = &view.draft else {
        return div().into_any_element();
    };

    // 1. Node Archetype / Kind switcher.
    let kinds = [
        (FlowStepKind::Request, fl.kind_request()),
        (FlowStepKind::Condition, fl.kind_condition()),
        (FlowStepKind::Join, fl.kind_join()),
    ];
    let mut kind_row = h_row().gap(px(5.0)).flex_wrap();
    for (k, lbl) in kinds {
        kind_row = kind_row.child(
            choice_chip(
                SharedString::from(format!("flow-skind-{k:?}")),
                lbl,
                step.step_kind == k,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.set_selected_step_kind(k, cx))),
        );
    }

    let sid_fork = step.id.clone();
    let kind_card = card(cx)
        .p(px(12.0))
        .gap(px(8.0))
        .child(section_header(fl.topo_node_kind(), None, cx))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(8.0))
                .flex_wrap()
                .child(kind_row)
                .child(
                    btn_secondary("flow-topo-fork", fl.graph_add_parallel())
                        .icon(icon_xs(UiIcon::Branch))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.add_parallel_sibling(&sid_fork, cx);
                        })),
                ),
        );

    // 2. Swimlane assignment.
    let mut lane_row = h_row().gap(px(5.0)).flex_wrap().child(
        choice_chip(
            "flow-lane-none",
            fl.topo_lane_none(),
            step.lane.is_none(),
            cx,
        )
        .on_click(cx.listener(|this, _, _, cx| this.set_selected_step_lane(None, cx))),
    );
    for lane in &flow.options.lanes {
        let lid = lane.id.clone();
        let active = step.lane.as_deref() == Some(lane.id.as_str());
        lane_row = lane_row.child(
            choice_chip(
                SharedString::from(format!("flow-lane-pick-{}", lane.id)),
                lane.title.clone(),
                active,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_selected_step_lane(Some(lid.clone()), cx);
            })),
        );
    }
    let lane_card = card(cx)
        .p(px(12.0))
        .gap(px(8.0))
        .child(section_header(fl.topo_lane_heading(), None, cx))
        .child(lane_row)
        .children(flow.options.lanes.is_empty().then(|| {
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(fl.topo_lane_hint())
        }));

    // 3. Upstream dependencies (`depends_on`) multi-select chips (cycle-guarded).
    let mut deps_row = h_row().gap(px(5.0)).flex_wrap();
    let other_steps: Vec<&FlowStep> = flow.steps.iter().filter(|s| s.id != step.id).collect();
    if other_steps.is_empty() {
        deps_row = deps_row.child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(fl.topo_no_other_steps()),
        );
    } else {
        for other in &other_steps {
            let oid = other.id.clone();
            let active = step.depends_on.iter().any(|d| d == &other.id);
            deps_row = deps_row.child(
                choice_chip(
                    SharedString::from(format!("flow-dep-{}", other.id)),
                    format!("#{} {}", other.position + 1, other.name),
                    active,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_selected_step_dependency(oid.clone(), cx);
                })),
            );
        }
    }
    let deps_card = card(cx)
        .p(px(12.0))
        .gap(px(8.0))
        .child(section_header(fl.topo_depends_on(), None, cx))
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(fl.topo_depends_on_hint()),
        )
        .child(deps_row);

    let mut root = v_col()
        .w_full()
        .gap(px(10.0))
        .child(kind_card)
        .child(lane_card)
        .child(deps_card);

    // 4. Conditional multi-exit branches editor (visible when `FlowStepKind::Condition`).
    if step.step_kind == FlowStepKind::Condition {
        let mut br_card = card(cx).p(px(12.0)).gap(px(8.0)).child(section_header(
            fl.topo_branches_heading(),
            None,
            cx,
        ));
        if step.branches.is_empty() {
            br_card = br_card.child(empty_inline(fl.topo_no_branches(), None, None, cx));
        } else {
            for br in &step.branches {
                let bid_rm = br.id.clone();
                let mut target_chips = h_row().gap(px(4.0)).flex_wrap();
                for other in &other_steps {
                    let bid_t = br.id.clone();
                    let oid = other.id.clone();
                    let linked = br.target_step_ids.iter().any(|t| t == &other.id);
                    target_chips = target_chips.child(
                        choice_chip(
                            SharedString::from(format!("flow-brt-{}-{}", br.id, other.id)),
                            format!("→ #{} {}", other.position + 1, other.name),
                            linked,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_branch_target(&bid_t, oid.clone(), cx);
                        })),
                    );
                }
                br_card = br_card.child(
                    v_col()
                        .w_full()
                        .gap(px(5.0))
                        .p(px(8.0))
                        .rounded(px(6.0))
                        .bg(to_hsla(p.sunken))
                        .child(
                            data_row(cx)
                                .child(badge(br.label.clone(), Tone::Accent, cx))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .text_color(to_hsla(p.ink_2))
                                        .truncate()
                                        .child(if br.expression.trim().is_empty() {
                                            fl.branch_default_else().to_owned()
                                        } else {
                                            br.expression.clone()
                                        }),
                                )
                                .child(meta_pill(format!("→ {}", br.target_step_ids.len()), cx))
                                .child(
                                    btn_icon(
                                        SharedString::from(format!("flow-br-rm-{}", br.id)),
                                        UiIcon::Trash,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.remove_branch_from_selected(&bid_rm, cx);
                                        },
                                    )),
                                ),
                        )
                        .child(target_chips),
                );
            }
        }
        br_card = br_card.child(
            h_row()
                .w_full()
                .gap(px(6.0))
                .child(
                    div()
                        .w(px(110.0))
                        .child(text_input(&view.branch_label_input)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(text_input(&view.branch_expr_input)),
                )
                .child(btn_secondary("flow-br-add", fl.topo_add_branch()).on_click(
                    cx.listener(|this, _, window, cx| this.add_branch_to_selected(window, cx)),
                )),
        );
        root = root.child(br_card);
    }

    root.into_any_element()
}
