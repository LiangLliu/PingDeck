//! Modular `Settings › Appearance` studio organized into four non-stacked domain bands:
//! 1. **Theme & Accent Studio** (`theme_studio`): visual light/dark/system mode cards,
//!    customizable theme accent color (follow palette, 8 OKLCH swatches, or `#RRGGBB` hex),
//!    and 8 miniature OKLCH eye-care surface palettes.
//! 2. **Code Syntax Studio** (`syntax_studio`): side-by-side 6 curated light/dark code themes
//!    paired with a live Tree-sitter highlighted JSON / XML / Diff preview.
//! 3. **Typography** (`typography_layout`): side-by-side UI & code font scale sliders with a
//!    live API workbench specimen.
//! 4. **Workspace Layout & Motion** (`typography_layout`): 3-column visual domain cards for
//!    density, sidebar layout, and reduced motion.

mod palette_grid;
mod syntax_studio;
mod theme_studio;
mod typography_layout;

use gpui_kit::{Context, ParentElement as _, Styled as _, px};
use spanbay_desktop::theme::{AccentPreset, PaletteId, SyntaxThemeId};

use crate::ui::{palette, v_col};

use super::SettingsView;

/// Active sample language shown in the Code Syntax Studio live preview.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SyntaxPreviewSample {
    #[default]
    Json,
    Xml,
    Diff,
}

/// Renders the refactored `Settings › Appearance` page.
pub fn render_appearance(view: &SettingsView, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let is_dark = palette(cx).is_dark;
    v_col()
        .w_full()
        .gap(px(20.0))
        .child(theme_studio::render_theme_studio(view, is_dark, cx))
        .child(syntax_studio::render_syntax_studio(view, is_dark, cx))
        .child(typography_layout::render_typography_section(view, cx))
        .child(typography_layout::render_layout_motion_section(view, cx))
}

pub(crate) fn palette_name(pid: PaletteId, cx: &gpui_kit::App) -> &'static str {
    let s = crate::ui::tr(cx).settings();
    match pid {
        PaletteId::Slate => s.palette_slate_name(),
        PaletteId::Ocean => s.palette_ocean_name(),
        PaletteId::Nord => s.palette_nord_name(),
        PaletteId::Sage => s.palette_sage_name(),
        PaletteId::Emerald => s.palette_emerald_name(),
        PaletteId::Amber => s.palette_amber_name(),
        PaletteId::Violet => s.palette_violet_name(),
        PaletteId::Graphite => s.palette_graphite_name(),
    }
}

pub(crate) fn palette_subtitle(pid: PaletteId, cx: &gpui_kit::App) -> &'static str {
    let s = crate::ui::tr(cx).settings();
    match pid {
        PaletteId::Slate => s.palette_slate_sub(),
        PaletteId::Ocean => s.palette_ocean_sub(),
        PaletteId::Nord => s.palette_nord_sub(),
        PaletteId::Sage => s.palette_sage_sub(),
        PaletteId::Emerald => s.palette_emerald_sub(),
        PaletteId::Amber => s.palette_amber_sub(),
        PaletteId::Violet => s.palette_violet_sub(),
        PaletteId::Graphite => s.palette_graphite_sub(),
    }
}

pub(crate) fn accent_preset_name(preset: AccentPreset, cx: &gpui_kit::App) -> &'static str {
    let s = crate::ui::tr(cx).settings();
    match preset {
        AccentPreset::Indigo => s.appearance_accent_indigo(),
        AccentPreset::Ocean => s.appearance_accent_ocean(),
        AccentPreset::Teal => s.appearance_accent_teal(),
        AccentPreset::Emerald => s.appearance_accent_emerald(),
        AccentPreset::Amber => s.appearance_accent_amber(),
        AccentPreset::Coral => s.appearance_accent_coral(),
        AccentPreset::Rose => s.appearance_accent_rose(),
        AccentPreset::Violet => s.appearance_accent_violet(),
    }
}

pub(crate) fn syntax_theme_name(id: SyntaxThemeId, cx: &gpui_kit::App) -> &'static str {
    let s = crate::ui::tr(cx).settings();
    match id {
        SyntaxThemeId::Catppuccin => s.appearance_syntax_catppuccin_name(),
        SyntaxThemeId::RosePine => s.appearance_syntax_rose_pine_name(),
        SyntaxThemeId::TokyoNight => s.appearance_syntax_tokyo_night_name(),
        SyntaxThemeId::GitHub => s.appearance_syntax_github_name(),
        SyntaxThemeId::OnePro => s.appearance_syntax_one_pro_name(),
        SyntaxThemeId::FollowPalette => s.appearance_syntax_follow_name(),
    }
}

pub(crate) fn syntax_theme_sub(id: SyntaxThemeId, cx: &gpui_kit::App) -> &'static str {
    let s = crate::ui::tr(cx).settings();
    match id {
        SyntaxThemeId::Catppuccin => s.appearance_syntax_catppuccin_sub(),
        SyntaxThemeId::RosePine => s.appearance_syntax_rose_pine_sub(),
        SyntaxThemeId::TokyoNight => s.appearance_syntax_tokyo_night_sub(),
        SyntaxThemeId::GitHub => s.appearance_syntax_github_sub(),
        SyntaxThemeId::OnePro => s.appearance_syntax_one_pro_sub(),
        SyntaxThemeId::FollowPalette => s.appearance_syntax_follow_sub(),
    }
}
