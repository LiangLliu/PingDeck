//! Native OS platform adapters implementing [`pingdeck_desktop::services`] ports on GPUI.

pub mod window_host;

#[cfg(target_os = "macos")]
pub mod dock_macos;
#[cfg(target_os = "macos")]
pub mod tray_macos;

pub use window_host::{GpuiHostWindow, HostCommand};

use std::sync::Arc;

use gpui_kit::App;
use pingdeck_desktop::runtime::DesktopRuntime;
use pingdeck_desktop::services::HostDock;
#[cfg(not(target_os = "macos"))]
use pingdeck_desktop::services::NoopDock;

/// Applies PingDeck's embedded application icon to the OS shell (macOS Dock tile in dev/release).
pub fn apply_app_icon() {
    #[cfg(target_os = "macos")]
    dock_macos::apply_dock_icon();
}

/// Creates the platform [`HostDock`] implementation (AppKit dock tile on macOS, no-op elsewhere).
#[must_use]
pub fn create_host_dock(runtime: &Arc<DesktopRuntime>, cx: &mut App) -> Arc<dyn HostDock> {
    #[cfg(target_os = "macos")]
    {
        dock_macos::MacOsDock::start(runtime, cx)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (runtime, cx);
        Arc::new(NoopDock)
    }
}
