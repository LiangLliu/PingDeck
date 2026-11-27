//! Safe macOS Dock icon, badge & attention bounce adapter (`#![forbid(unsafe_code)]`).
//!
//! Applies the embedded SpanBay Retina PNG onto `NSApplication.dockTile` on startup (so even
//! unbundled `cargo run` sessions display the real app icon) and routes badge/bounce updates across
//! a `Send + Sync` channel onto GPUI's AppKit main thread with zero `unsafe`.

use std::sync::Arc;

use gpui_kit::App;
use objc2::{AnyThread as _, MainThreadMarker};
use objc2_app_kit::{
    NSApplication, NSImage, NSImageScaling, NSImageView, NSRequestUserAttentionType,
};
use objc2_foundation::{NSData, NSString};
use spanbay_desktop::core::model::events::EventSink as _;
use spanbay_desktop::runtime::DesktopRuntime;
use spanbay_desktop::services::HostDock;
use spanbay_desktop::streams::ChannelSink;

use crate::ui::APP_ICON_PNG_BYTES;

/// Sets SpanBay's real Retina application icon on the macOS Dock tile (100% safe AppKit).
pub fn apply_dock_icon() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(APP_ICON_PNG_BYTES);
    let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
        return;
    };
    let image_view = NSImageView::imageViewWithImage(&image, mtm);
    image_view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
    let ns_app = NSApplication::sharedApplication(mtm);
    let dock_tile = ns_app.dockTile();
    dock_tile.setContentView(Some(&image_view));
    dock_tile.display();
}

enum DockCommand {
    SetBadge(Option<i64>),
    Bounce,
}

pub struct MacOsDock {
    tx: ChannelSink<DockCommand>,
}

impl MacOsDock {
    #[must_use]
    pub fn start(runtime: &Arc<DesktopRuntime>, cx: &mut App) -> Arc<Self> {
        let (tx, mut rx) = ChannelSink::pair();
        let handle = runtime.tokio_handle();
        cx.spawn(async move |_cx| {
            loop {
                let next = handle
                    .spawn(async move {
                        let batch = rx.recv_batch(16).await;
                        (batch, rx)
                    })
                    .await;
                let Ok((Some(batch), next_rx)) = next else {
                    break;
                };
                rx = next_rx;
                if let Some(mtm) = MainThreadMarker::new() {
                    let ns_app = NSApplication::sharedApplication(mtm);
                    for cmd in batch {
                        match cmd {
                            DockCommand::SetBadge(count) => {
                                let dock_tile = ns_app.dockTile();
                                match count.filter(|&n| n > 0) {
                                    Some(n) => {
                                        let ns = NSString::from_str(&n.to_string());
                                        dock_tile.setBadgeLabel(Some(&ns));
                                    }
                                    None => dock_tile.setBadgeLabel(None),
                                }
                                dock_tile.display();
                            }
                            DockCommand::Bounce => {
                                let _ = ns_app.requestUserAttention(
                                    NSRequestUserAttentionType::InformationalRequest,
                                );
                            }
                        }
                    }
                }
            }
        })
        .detach();
        Arc::new(Self { tx })
    }
}

impl HostDock for MacOsDock {
    fn set_badge_count(&self, count: Option<i64>) {
        let _ = self.tx.emit(DockCommand::SetBadge(count));
    }

    fn request_informational_attention(&self) {
        let _ = self.tx.emit(DockCommand::Bounce);
    }
}
