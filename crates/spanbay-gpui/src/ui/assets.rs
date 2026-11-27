//! Embedded SpanBay application icon & brand-mark assets merged with `gpui-kit` Lucide icons.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

/// Asset path for SpanBay's full-color Retina PNG application icon (`assets/icons/128x128@2x.png`).
pub const APP_ICON_PATH: &str = "icons/spanbay-app.png";

/// Raw PNG bytes for SpanBay's Retina application icon (also used for macOS Dock integration).
pub const APP_ICON_PNG_BYTES: &[u8] = include_bytes!("../../../../assets/icons/128x128@2x.png");

/// Asset path for the monochrome three-span vector layer of SpanBay's SpanMark.
pub const SPANMARK_BODY_SVG_PATH: &str = "icons/spanbay-mark-body.svg";

/// Asset path for the origin-dot vector layer of SpanBay's SpanMark.
pub const SPANMARK_DOT_SVG_PATH: &str = "icons/spanbay-mark-dot.svg";

const SPANMARK_BODY_SVG_BYTES: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="240 240 544 544"><rect x="428" y="280" width="332" height="120" rx="60" fill="#000"/><rect x="264" y="452" width="496" height="120" rx="60" fill="#000"/><rect x="264" y="624" width="332" height="120" rx="60" fill="#000"/></svg>"##;

const SPANMARK_DOT_SVG_BYTES: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="240 240 544 544"><circle cx="324" cy="340" r="60" fill="#000"/></svg>"##;

const CUSTOM_ASSET_PATHS: &[&str] = &[APP_ICON_PATH, SPANMARK_BODY_SVG_PATH, SPANMARK_DOT_SVG_PATH];

/// Composite GPUI [`AssetSource`] serving SpanBay's brand icons alongside `gpui-kit`'s full
/// Lucide icon catalog.
#[derive(Clone, Copy, Debug, Default)]
pub struct SpanBayAssets;

impl AssetSource for SpanBayAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match path {
            APP_ICON_PATH => Ok(Some(Cow::Borrowed(APP_ICON_PNG_BYTES))),
            SPANMARK_BODY_SVG_PATH => Ok(Some(Cow::Borrowed(SPANMARK_BODY_SVG_BYTES))),
            SPANMARK_DOT_SVG_PATH => Ok(Some(Cow::Borrowed(SPANMARK_DOT_SVG_BYTES))),
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
