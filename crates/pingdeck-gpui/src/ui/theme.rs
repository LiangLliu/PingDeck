//! Theme adapter mapping [`pingdeck_desktop::theme::ResolvedPalette`] into GPUI & `gpui-component`.
//!
//! This module lives inside `src/ui/` — the sole boundary permitted to touch `gpui_kit::component`.

use std::sync::Arc;

use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::{App, Global, Hsla, Window, hsla, px};
use pingdeck_desktop::core::model::settings::{
    AppearanceSettings, Density, Theme as SettingsTheme,
};
use pingdeck_desktop::theme::{PaletteId, ResolvedPalette, RgbaColor, resolve_palette};

/// GPUI global storing the resolved PingDeck OKLCH semantic palette and density mode so views
/// can style domain visuals without importing `gpui-component`.
#[derive(Clone)]
pub struct PingDeckPalette {
    pub colors: Arc<ResolvedPalette>,
    pub density: Density,
    pub ui_font_size: u8,
}

impl Global for PingDeckPalette {}

/// Converts a framework-agnostic [`RgbaColor`] to GPUI [`Hsla`].
#[must_use]
pub fn to_hsla(color: RgbaColor) -> Hsla {
    let (h, s, l, a) = color.to_hsla();
    hsla(h, s, l, a)
}

/// Reads a cheaply cloned handle to the currently active PingDeck [`ResolvedPalette`],
/// avoiding holding an immutable borrow of `App` / `Context` across `cx.listener(...)` calls.
#[must_use]
pub fn palette(cx: &App) -> Arc<ResolvedPalette> {
    Arc::clone(&cx.global::<PingDeckPalette>().colors)
}

/// Returns whether compact UI density is currently active.
#[must_use]
pub fn is_compact(cx: &App) -> bool {
    cx.global::<PingDeckPalette>().density == Density::Compact
}

/// Synchronizes GPUI's window root `rem` scale with `AppearanceSettings::ui_font_size`
/// so all `rems`-based component typography and padding scale cleanly in real time.
pub fn sync_window_scale(appearance: &AppearanceSettings, window: &mut Window) {
    let base_px = f32::from(appearance.ui_font_size.clamp(11, 18));
    // GPUI default rem_size is 16px for 14px UI font scale; map proportionally.
    let rem_px = (base_px / 14.0) * 16.0;
    window.set_rem_size(px(rem_px));
}

/// Resolves whether dark mode should be active from `AppearanceSettings::theme` and window hint.
#[must_use]
pub fn resolve_is_dark(setting: SettingsTheme, system_is_dark: bool) -> bool {
    match setting {
        SettingsTheme::Light => false,
        SettingsTheme::Dark => true,
        SettingsTheme::System => system_is_dark,
    }
}

/// Applies one of the 16 PingDeck eye-care palettes (`PaletteId` × light/dark) to both
/// [`PingDeckPalette`] and `gpui-component`'s global [`Theme`] atomically.
pub fn apply_palette(
    palette_id: PaletteId,
    is_dark: bool,
    high_contrast: bool,
    appearance: &AppearanceSettings,
    cx: &mut App,
) {
    let resolved = resolve_palette(palette_id, is_dark, high_contrast);
    cx.set_global(PingDeckPalette {
        colors: Arc::new(resolved.clone()),
        density: appearance.density,
        ui_font_size: appearance.ui_font_size,
    });

    let mode = if is_dark {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    Theme::change(mode, None, cx);
    Theme::update(cx, |theme| {
        theme.font_size = px(f32::from(appearance.ui_font_size));
        theme.mono_font_size = px(f32::from(appearance.mono_font_size));
        theme.radius = px(6.0);
        theme.radius_lg = px(10.0);
        // Disable the 3px outer focus ring (which paints a thick unblurred halo and clips inside
        // scroll containers); focused inputs show a crisp 1px accent border via `c.ring` instead.
        theme.focus_ring = false;

        let c = &mut theme.colors;
        c.background = to_hsla(resolved.surface);
        c.foreground = to_hsla(resolved.ink);
        c.border = to_hsla(resolved.line_strong.composite_over(resolved.surface));
        c.input = to_hsla(resolved.line_strong.composite_over(resolved.surface));
        c.ring = to_hsla(resolved.accent);
        c.caret = to_hsla(resolved.accent);
        c.selection = to_hsla(resolved.accent_soft);

        c.primary = to_hsla(resolved.accent);
        c.primary_hover = to_hsla(resolved.accent_hover);
        c.primary_active = to_hsla(resolved.accent_hover);
        c.primary_foreground = to_hsla(resolved.on_accent);

        c.secondary = to_hsla(resolved.surface_2);
        c.secondary_hover = to_hsla(resolved.hover.composite_over(resolved.surface_2));
        c.secondary_active = to_hsla(resolved.press.composite_over(resolved.surface_2));
        c.secondary_foreground = to_hsla(resolved.ink);

        c.accent = to_hsla(resolved.accent_soft);
        c.accent_foreground = to_hsla(resolved.accent_ink);
        c.muted = to_hsla(resolved.sunken);
        c.muted_foreground = to_hsla(resolved.muted);

        c.popover = to_hsla(resolved.overlay);
        c.popover_foreground = to_hsla(resolved.ink);
        c.overlay = to_hsla(resolved.scrim);
        c.window_border = to_hsla(resolved.page_edge.composite_over(resolved.canvas));

        c.sidebar = to_hsla(resolved.canvas);
        c.sidebar_foreground = to_hsla(resolved.ink_2);
        c.sidebar_border = to_hsla(resolved.line.composite_over(resolved.canvas));
        c.sidebar_accent = to_hsla(resolved.selected);
        c.sidebar_accent_foreground = to_hsla(resolved.ink);
        c.sidebar_primary = to_hsla(resolved.accent);
        c.sidebar_primary_foreground = to_hsla(resolved.on_accent);

        c.title_bar = to_hsla(resolved.canvas);
        c.title_bar_border = to_hsla(resolved.line.composite_over(resolved.canvas));
        c.status_bar = to_hsla(resolved.pane);
        c.status_bar_border = to_hsla(resolved.line.composite_over(resolved.pane));

        c.list = to_hsla(resolved.pane);
        c.list_even = to_hsla(resolved.surface);
        c.list_head = to_hsla(resolved.pane);
        c.list_hover = to_hsla(resolved.hover.composite_over(resolved.pane));
        c.list_active = to_hsla(resolved.selected_row);
        c.list_active_border = to_hsla(resolved.accent_line.composite_over(resolved.selected_row));

        c.table = to_hsla(resolved.surface);
        c.table_even = to_hsla(resolved.surface_2);
        c.table_head = to_hsla(resolved.pane);
        c.table_head_foreground = to_hsla(resolved.muted);
        c.table_foot = to_hsla(resolved.pane);
        c.table_foot_foreground = to_hsla(resolved.muted);
        c.table_hover = to_hsla(resolved.hover.composite_over(resolved.surface));
        c.table_active = to_hsla(resolved.selected_row);
        c.table_active_border = to_hsla(resolved.accent_line.composite_over(resolved.selected_row));
        c.table_row_border = to_hsla(resolved.line.composite_over(resolved.surface));

        c.tab_bar = to_hsla(resolved.pane);
        c.tab_bar_segmented = to_hsla(resolved.sunken);
        c.tab = to_hsla(resolved.pane);
        c.tab_foreground = to_hsla(resolved.muted);
        c.tab_active = to_hsla(resolved.surface);
        c.tab_active_foreground = to_hsla(resolved.ink);

        c.button = to_hsla(resolved.surface);
        c.button_hover = to_hsla(resolved.hover.composite_over(resolved.surface));
        c.button_active = to_hsla(resolved.press.composite_over(resolved.surface));
        c.button_foreground = to_hsla(resolved.ink);
        c.button_primary = to_hsla(resolved.accent);
        c.button_primary_hover = to_hsla(resolved.accent_hover);
        c.button_primary_active = to_hsla(resolved.accent_hover);
        c.button_primary_foreground = to_hsla(resolved.on_accent);
        c.button_danger = to_hsla(resolved.bad_fill);
        c.button_danger_hover = to_hsla(resolved.bad);
        c.button_danger_active = to_hsla(resolved.bad);
        c.button_danger_foreground = to_hsla(resolved.on_accent);

        c.success = to_hsla(resolved.ok);
        c.success_foreground = to_hsla(resolved.on_accent);
        c.warning = to_hsla(resolved.warn);
        c.warning_foreground = to_hsla(resolved.on_accent);
        c.danger = to_hsla(resolved.bad_fill);
        c.danger_foreground = to_hsla(resolved.on_accent);
        c.info = to_hsla(resolved.accent);
        c.info_foreground = to_hsla(resolved.on_accent);
        c.link = to_hsla(resolved.accent_ink);
        c.link_hover = to_hsla(resolved.accent);
        c.link_active = to_hsla(resolved.accent_hover);

        c.switch = to_hsla(resolved.line_control.composite_over(resolved.surface));
        c.switch_thumb = to_hsla(resolved.on_accent);
        c.group_box = to_hsla(resolved.pane);
        c.group_box_foreground = to_hsla(resolved.ink);

        c.chart_1 = to_hsla(resolved.accent);
        c.chart_2 = to_hsla(resolved.ok);
        c.chart_3 = to_hsla(resolved.warn);
        c.chart_4 = to_hsla(resolved.m_patch);
        c.chart_5 = to_hsla(resolved.bad);
        c.chart_grid = to_hsla(resolved.line.composite_over(resolved.surface));
    });
}
