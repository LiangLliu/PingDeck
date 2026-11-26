//! Reusable bounded pagination bars and slice helpers shared across all PingDeck list views.

use gpui_kit::{Context, Div, ParentElement as _, SharedString, Styled as _, div, px};

use super::data::choice_chip;
use super::primitives::{btn_ghost, btn_secondary, h_row};
use super::theme::{palette, to_hsla};
use super::{Disableable as _, tr};

/// Standard page-size presets offered on full-width paginated data tables.
pub const DEFAULT_PAGE_SIZES: &[usize] = &[50, 100, 200];

/// Returns `(slice, clamped_page_index, total_pages)` for client-side paginated slices.
///
/// Guarantees `total_pages >= 1` and never panics regardless of `page` or `page_size`.
#[must_use]
pub fn page_slice<T>(items: &[T], page: usize, page_size: usize) -> (&[T], usize, usize) {
    let size = page_size.max(1);
    let total = items.len();
    let total_pages = total.div_ceil(size).max(1);
    let clamped = page.min(total_pages - 1);
    let start = clamped.saturating_mul(size).min(total);
    let end = start.saturating_add(size).min(total);
    (&items[start..end], clamped, total_pages)
}

/// Configuration for [`pagination_bar`] (random-access page navigation).
#[derive(Debug, Clone, Copy)]
pub struct PaginationBar<'a> {
    pub id_prefix: &'a str,
    /// Zero-based current page index.
    pub page: usize,
    pub page_size: usize,
    pub total: usize,
    /// Page-size chips to show (empty hides the selector).
    pub page_sizes: &'a [usize],
    pub loading: bool,
    /// Compact variant for narrow master sidebars (≤ 340 px).
    pub compact: bool,
}

/// Renders a unified pagination footer bar with range summary, optional page-size pills,
/// and First / Prev / Next / Last navigation controls.
pub fn pagination_bar<V: 'static>(
    cfg: PaginationBar<'_>,
    on_page: fn(&mut V, usize, &mut Context<V>),
    on_page_size: Option<fn(&mut V, usize, &mut Context<V>)>,
    cx: &mut Context<V>,
) -> Div {
    let p = palette(cx);
    let c = tr(cx).common();
    let size = cfg.page_size.max(1);
    let total_pages = cfg.total.div_ceil(size).max(1);
    let page = cfg.page.min(total_pages - 1);
    let start = if cfg.total == 0 {
        0
    } else {
        page.saturating_mul(size) + 1
    };
    let end = if cfg.total == 0 {
        0
    } else {
        (page.saturating_mul(size) + size).min(cfg.total)
    };

    let summary_text = if cfg.total == 0 {
        c.page_total(0)
    } else if cfg.compact {
        c.page_compact(page + 1, total_pages, cfg.total)
    } else {
        c.page_range(start, end, cfg.total)
    };

    let at_first = page == 0;
    let at_last = page + 1 >= total_pages;

    let mut controls = h_row().gap(px(4.0)).flex_shrink_0();

    if !cfg.compact
        && !cfg.page_sizes.is_empty()
        && let Some(on_size) = on_page_size
    {
        let mut size_group = h_row().gap(px(3.0)).mr(px(6.0));
        for &sz in cfg.page_sizes {
            let active = sz == size;
            size_group = size_group.child(
                choice_chip(
                    SharedString::from(format!("{}-sz-{sz}", cfg.id_prefix)),
                    c.page_per_page(sz),
                    active,
                    cx,
                )
                .disabled(cfg.loading)
                .on_click(cx.listener(move |this, _, _, cx| on_size(this, sz, cx))),
            );
        }
        controls = controls.child(size_group);
    }

    if !cfg.compact || page > 1 {
        controls = controls.child(
            btn_ghost(SharedString::from(format!("{}-first", cfg.id_prefix)), "«")
                .tooltip(c.page_first())
                .disabled(cfg.loading || at_first)
                .on_click(cx.listener(move |this, _, _, cx| on_page(this, 0, cx))),
        );
    }

    controls = controls.child(
        btn_secondary(SharedString::from(format!("{}-prev", cfg.id_prefix)), "‹")
            .tooltip(c.page_prev())
            .disabled(cfg.loading || at_first)
            .on_click(cx.listener(move |this, _, _, cx| {
                if page > 0 {
                    on_page(this, page - 1, cx);
                }
            })),
    );

    if !cfg.compact {
        controls = controls.child(
            div()
                .px(px(6.0))
                .text_xs()
                .text_color(to_hsla(p.ink_2))
                .child(SharedString::from(format!("{} / {total_pages}", page + 1))),
        );
    }

    controls = controls.child(
        btn_secondary(SharedString::from(format!("{}-next", cfg.id_prefix)), "›")
            .tooltip(c.page_next())
            .disabled(cfg.loading || at_last)
            .on_click(cx.listener(move |this, _, _, cx| {
                if page + 1 < total_pages {
                    on_page(this, page + 1, cx);
                }
            })),
    );

    if !cfg.compact {
        let last_idx = total_pages - 1;
        controls = controls.child(
            btn_ghost(SharedString::from(format!("{}-last", cfg.id_prefix)), "»")
                .tooltip(c.page_last())
                .disabled(cfg.loading || at_last)
                .on_click(cx.listener(move |this, _, _, cx| on_page(this, last_idx, cx))),
        );
    }

    h_row()
        .w_full()
        .justify_between()
        .gap(px(8.0))
        .flex_wrap()
        .child(
            div()
                .text_size(px(11.0))
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(summary_text)),
        )
        .child(controls)
}

/// Configuration for [`cursor_pagination_bar`] (keyset cursor navigation with known or estimated total).
#[derive(Debug, Clone, Copy)]
pub struct CursorPaginationBar<'a> {
    pub id_prefix: &'a str,
    /// Zero-based current page index.
    pub page: usize,
    pub page_size: usize,
    pub shown: usize,
    pub total: Option<u64>,
    pub has_next: bool,
    pub loading: bool,
    pub compact: bool,
}

/// Renders a keyset-cursor pagination bar (`«` First / `‹` Prev / `›` Next) for SQLite
/// cursor-paged streams such as Execution History.
pub fn cursor_pagination_bar<V: 'static>(
    cfg: CursorPaginationBar<'_>,
    on_first: fn(&mut V, &mut Context<V>),
    on_prev: fn(&mut V, &mut Context<V>),
    on_next: fn(&mut V, &mut Context<V>),
    cx: &mut Context<V>,
) -> Div {
    let p = palette(cx);
    let c = tr(cx).common();
    let size = cfg.page_size.max(1);
    let total_usize = cfg.total.map(|t| t as usize);
    let total_pages = total_usize
        .map(|t| t.div_ceil(size).max(1))
        .unwrap_or_else(|| cfg.page + 1 + usize::from(cfg.has_next));

    let summary_text = match total_usize {
        Some(0) => c.page_total(0),
        Some(total) if cfg.compact => c.page_compact(cfg.page + 1, total_pages, total),
        Some(total) => {
            let start = cfg.page.saturating_mul(size) + 1;
            let end = (cfg.page.saturating_mul(size) + cfg.shown).min(total);
            c.page_range(start.min(total), end, total)
        }
        None => c.page_total(cfg.shown),
    };

    let at_first = cfg.page == 0;
    let mut controls = h_row().gap(px(4.0)).flex_shrink_0();
    if cfg.page > 0 {
        controls = controls.child(
            btn_ghost(SharedString::from(format!("{}-first", cfg.id_prefix)), "«")
                .tooltip(c.page_first())
                .disabled(cfg.loading)
                .on_click(cx.listener(move |this, _, _, cx| on_first(this, cx))),
        );
    }
    controls = controls
        .child(
            btn_secondary(SharedString::from(format!("{}-prev", cfg.id_prefix)), "‹")
                .tooltip(c.page_prev())
                .disabled(cfg.loading || at_first)
                .on_click(cx.listener(move |this, _, _, cx| on_prev(this, cx))),
        )
        .child(
            btn_secondary(SharedString::from(format!("{}-next", cfg.id_prefix)), "›")
                .tooltip(c.page_next())
                .disabled(cfg.loading || !cfg.has_next)
                .on_click(cx.listener(move |this, _, _, cx| on_next(this, cx))),
        );

    h_row()
        .w_full()
        .justify_between()
        .gap(px(6.0))
        .child(
            div()
                .text_size(px(11.0))
                .text_color(to_hsla(p.muted))
                .truncate()
                .child(SharedString::from(summary_text)),
        )
        .child(controls)
}

#[cfg(test)]
mod tests {
    use super::page_slice;

    #[test]
    fn page_slice_handles_empty_exact_partial_and_out_of_range() {
        let empty: [u32; 0] = [];
        let (slice, page, total_pages) = page_slice(&empty, 3, 50);
        assert!(slice.is_empty());
        assert_eq!((page, total_pages), (0, 1));

        let items: Vec<usize> = (0..120).collect();
        let (s0, p0, tp) = page_slice(&items, 0, 50);
        assert_eq!((p0, tp), (0, 3));
        assert_eq!(s0.len(), 50);
        assert_eq!(s0[0], 0);

        let (s2, p2, _) = page_slice(&items, 2, 50);
        assert_eq!(p2, 2);
        assert_eq!(s2.len(), 20);
        assert_eq!(s2[0], 100);

        // Out-of-range page clamps to the last page.
        let (sc, pc, _) = page_slice(&items, 99, 50);
        assert_eq!(pc, 2);
        assert_eq!(sc.len(), 20);

        // Zero page size clamps to 1 without panicking.
        let (sz, pz, tpz) = page_slice(&items[..3], 1, 0);
        assert_eq!((pz, tpz, sz), (1, 3, &[1][..]));
    }
}
