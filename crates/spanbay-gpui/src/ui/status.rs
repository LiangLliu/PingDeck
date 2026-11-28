//! The one status vocabulary (`docs/design.md` §2.2): every run, tool, sub-agent and todo status
//! lands on a [`GraphNodeStatus`] and is drawn through this table as shape, word and word color.
//! Normal and running words use the secondary text color; only warnings and errors are tinted.

use gpui_kit::{
    AnyElement, App, Div, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Stateful, Styled as _, div, px,
};
use spanbay_desktop::core::agent::dto::BoardWait;
use spanbay_desktop::core::agent::model::AgentRunStatus;
use spanbay_desktop::state::{GraphNodeStatus, map_run_status};

use super::primitives::{Tone, h_row};
use super::theme::{palette, to_hsla};
use super::widgets::{StatusShape, status_indicator};

/// What a waiting run waits for; picks the waiting word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitKind {
    Approval,
    Plan,
    Question,
    Cost,
}

/// Derives the wait reason from the board's wait tag and the run's status detail.
#[must_use]
pub fn wait_kind(wait: Option<BoardWait>, status_detail: Option<&str>) -> WaitKind {
    if wait == Some(BoardWait::CostLimit) || status_detail == Some("cost limit") {
        WaitKind::Cost
    } else if status_detail == Some("plan") {
        WaitKind::Plan
    } else if status_detail == Some("question") {
        WaitKind::Question
    } else {
        WaitKind::Approval
    }
}

/// Shape and word tone of a status.
#[must_use]
pub const fn status_visual(status: GraphNodeStatus) -> (StatusShape, Tone) {
    match status {
        GraphNodeStatus::Pending | GraphNodeStatus::Queued => (StatusShape::Idle, Tone::Neutral),
        GraphNodeStatus::Running => (StatusShape::Run, Tone::Neutral),
        GraphNodeStatus::Waiting
        | GraphNodeStatus::Interrupted
        | GraphNodeStatus::Blocked
        | GraphNodeStatus::TimedOut => (StatusShape::Warn, Tone::Warning),
        GraphNodeStatus::Completed => (StatusShape::Ok, Tone::Neutral),
        GraphNodeStatus::Failed => (StatusShape::Bad, Tone::Danger),
        GraphNodeStatus::Denied
        | GraphNodeStatus::Cancelled
        | GraphNodeStatus::Expired
        | GraphNodeStatus::Superseded
        | GraphNodeStatus::Skipped => (StatusShape::Stopped, Tone::Neutral),
    }
}

/// Localized status word; `wait` refines `Waiting` (defaults to awaiting approval).
#[must_use]
pub fn status_label(status: GraphNodeStatus, wait: Option<WaitKind>, cx: &App) -> &'static str {
    let ch = super::i18n::tr(cx).chat();
    match status {
        GraphNodeStatus::Pending => ch.run_status_pending(),
        GraphNodeStatus::Queued => ch.run_status_queued(),
        GraphNodeStatus::Running => ch.run_status_running(),
        GraphNodeStatus::Waiting => match wait.unwrap_or(WaitKind::Approval) {
            WaitKind::Approval => ch.run_status_waiting_approval(),
            WaitKind::Plan => ch.run_status_waiting_plan(),
            WaitKind::Question => ch.run_status_waiting_question(),
            WaitKind::Cost => ch.run_status_waiting_cost(),
        },
        GraphNodeStatus::Completed => ch.run_status_completed(),
        GraphNodeStatus::Failed => ch.run_status_failed(),
        GraphNodeStatus::Cancelled => ch.run_status_cancelled(),
        GraphNodeStatus::Interrupted => ch.run_status_interrupted(),
        GraphNodeStatus::Skipped => ch.run_status_skipped(),
        GraphNodeStatus::Denied => ch.run_status_denied(),
        GraphNodeStatus::Blocked => ch.run_status_blocked(),
        GraphNodeStatus::TimedOut => ch.run_status_timed_out(),
        GraphNodeStatus::Expired => ch.run_status_expired(),
        GraphNodeStatus::Superseded => ch.run_status_superseded(),
    }
}

/// A run's shape, word tone and word, waiting reason included (run rows, headers, feeds).
#[must_use]
pub fn run_status_meta(
    status: AgentRunStatus,
    wait: Option<BoardWait>,
    status_detail: Option<&str>,
    cx: &App,
) -> (StatusShape, Tone, &'static str) {
    let s = map_run_status(status);
    let (shape, tone) = status_visual(s);
    (
        shape,
        tone,
        status_label(s, Some(wait_kind(wait, status_detail)), cx),
    )
}

/// Colour of a status word: tinted only for warnings and errors (design §2.2).
#[must_use]
pub fn status_word_color(tone: Tone, cx: &App) -> gpui_kit::Hsla {
    let p = palette(cx);
    match tone {
        Tone::Warning => to_hsla(p.warn),
        Tone::Danger => to_hsla(p.bad),
        _ => to_hsla(p.ink_2),
    }
}

/// Indicator plus status word.
#[must_use]
pub fn status_badge(status: GraphNodeStatus, wait: Option<WaitKind>, cx: &App) -> Div {
    let (shape, tone) = status_visual(status);
    h_row()
        .gap(px(5.0))
        .flex_shrink_0()
        .child(status_indicator(shape, cx))
        .child(
            div()
                .text_xs()
                .text_color(status_word_color(tone, cx))
                .child(status_label(status, wait, cx)),
        )
}

/// One row of a list of steps (tool calls, sub-agent steps, todos, run feed rows): indicator,
/// title, the status word unless the step is running or done, faint meta, then trailing actions.
#[must_use]
pub fn status_row(
    id: impl Into<ElementId>,
    status: GraphNodeStatus,
    title: impl Into<SharedString>,
    meta: Option<SharedString>,
    trailing: Option<AnyElement>,
    cx: &App,
) -> Stateful<Div> {
    status_row_wait(id, status, None, title, meta, trailing, cx)
}

/// [`status_row`] for a run row whose waiting word names what it waits for.
#[must_use]
pub fn status_row_wait(
    id: impl Into<ElementId>,
    status: GraphNodeStatus,
    wait: Option<WaitKind>,
    title: impl Into<SharedString>,
    meta: Option<SharedString>,
    trailing: Option<AnyElement>,
    cx: &App,
) -> Stateful<Div> {
    let p = palette(cx);
    let (shape, tone) = status_visual(status);
    let show_word = !matches!(
        status,
        GraphNodeStatus::Running | GraphNodeStatus::Completed
    );
    h_row()
        .id(id)
        .w_full()
        .gap(px(6.0))
        .px(px(8.0))
        .py(px(4.0))
        .rounded(px(6.0))
        .child(status_indicator(shape, cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(to_hsla(p.ink))
                .child(title.into()),
        )
        .children(show_word.then(|| {
            div()
                .flex_shrink_0()
                .text_xs()
                .text_color(status_word_color(tone, cx))
                .child(status_label(status, wait, cx))
        }))
        .children(meta.map(|m| {
            div()
                .flex_shrink_0()
                .text_xs()
                .text_color(to_hsla(p.faint))
                .child(m)
        }))
        .children(trailing.map(|t| div().flex_shrink_0().child(t).into_any_element()))
}

#[cfg(test)]
mod tests {
    use super::{StatusShape, Tone, status_visual};
    use spanbay_desktop::state::GraphNodeStatus as S;

    #[test]
    fn only_failures_are_red_and_stopped_states_are_hollow_squares() {
        for s in [
            S::Denied,
            S::Cancelled,
            S::Expired,
            S::Superseded,
            S::Skipped,
        ] {
            assert_eq!(
                status_visual(s),
                (StatusShape::Stopped, Tone::Neutral),
                "{s:?}"
            );
        }
        for s in [S::Interrupted, S::Blocked, S::TimedOut, S::Waiting] {
            assert_eq!(
                status_visual(s),
                (StatusShape::Warn, Tone::Warning),
                "{s:?}"
            );
        }
        assert_eq!(status_visual(S::Failed), (StatusShape::Bad, Tone::Danger));
        assert_eq!(status_visual(S::Completed).1, Tone::Neutral);
    }
}
