//! GPUI global language state and zero-cost typed i18n accessors.
//!
//! Keeps the active [`Language`] synchronized in GPUI global state whenever settings load or
//! change so views can read `tr(cx)` without threading `lang` / `zh` booleans everywhere.

use gpui_kit::{App, Global};
use pingdeck_desktop::core::model::settings::Language;
pub use pingdeck_desktop::i18n::Tr;

/// GPUI global holding the active interface [`Language`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PingDeckLanguage(pub Language);

impl Global for PingDeckLanguage {}

/// Updates the active interface [`Language`] in GPUI global state.
pub fn apply_language(lang: Language, cx: &mut App) {
    cx.set_global(PingDeckLanguage(lang));
}

/// Returns the active interface [`Language`], defaulting to [`Language::ZhCn`] if unset.
#[must_use]
pub fn active_language(cx: &App) -> Language {
    cx.try_global::<PingDeckLanguage>()
        .map_or(Language::ZhCn, |g| g.0)
}

/// Returns the compile-time typed i18n facade bound to the active GPUI interface language.
#[must_use]
pub fn tr(cx: &App) -> Tr {
    Tr::new(active_language(cx))
}
