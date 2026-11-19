//! Embedded PingDeck application icon & brand-mark assets merged with `gpui-kit` Lucide icons.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

/// Asset path for PingDeck's full-color Retina PNG application icon (`assets/icons/128x128@2x.png`).
pub const APP_ICON_PATH: &str = "icons/pingdeck-app.png";

/// Raw PNG bytes for PingDeck's Retina application icon (also used for macOS Dock integration).
pub const APP_ICON_PNG_BYTES: &[u8] = include_bytes!("../../../../assets/icons/128x128@2x.png");

/// Asset path for the monochrome "p" bowl + stem vector layer of PingDeck's PingMark.
pub const PINGMARK_BODY_SVG_PATH: &str = "icons/pingdeck-mark-body.svg";

/// Asset path for the origin-dot vector layer of PingDeck's PingMark.
pub const PINGMARK_DOT_SVG_PATH: &str = "icons/pingdeck-mark-dot.svg";

const PINGMARK_BODY_SVG_BYTES: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="270 224 458 576"><rect x="282" y="236" width="104" height="552" rx="52" fill="#000"/><path fill-rule="evenodd" fill="#000" d="M520 236a196 196 0 1 1 0 392a196 196 0 1 1 0-392zm0 98a98 98 0 1 0 0 196a98 98 0 1 0 0-196z"/></svg>"##;

const PINGMARK_DOT_SVG_BYTES: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="270 224 458 576"><circle cx="520" cy="432" r="54" fill="#000"/></svg>"##;

const CUSTOM_ASSET_PATHS: &[&str] = &[APP_ICON_PATH, PINGMARK_BODY_SVG_PATH, PINGMARK_DOT_SVG_PATH];

/// Composite GPUI [`AssetSource`] serving PingDeck's brand icons alongside `gpui-kit`'s full
/// Lucide icon catalog.
#[derive(Clone, Copy, Debug, Default)]
pub struct PingDeckAssets;

impl AssetSource for PingDeckAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match path {
            APP_ICON_PATH => Ok(Some(Cow::Borrowed(APP_ICON_PNG_BYTES))),
            PINGMARK_BODY_SVG_PATH => Ok(Some(Cow::Borrowed(PINGMARK_BODY_SVG_BYTES))),
            PINGMARK_DOT_SVG_PATH => Ok(Some(Cow::Borrowed(PINGMARK_DOT_SVG_BYTES))),
            _ => gpui_kit::assets::AllAssets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut items = gpui_kit::assets::AllAssets.list(path)?;
        for &custom in CUSTOM_ASSET_PATHS {
            if custom.starts_with(path) {
                items.push(SharedString::from(custom));
            }
        }
        Ok(items)
    }
}
