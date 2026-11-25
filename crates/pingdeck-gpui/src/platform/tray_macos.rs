//! macOS menu-bar status item driven by [`pingdeck_desktop::services::tray`].
//!
//! Creates and updates `tray-icon` / `muda` menu items strictly on the GPUI AppKit main thread
//! while polling status snapshots off-thread via Tokio.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::App;
use pingdeck_desktop::core::logging::{LogLevel, log_line};
use pingdeck_desktop::runtime::DesktopRuntime;
use pingdeck_desktop::services::{HostWindow, tray_snapshot};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

const SHOW_ID: &str = "pingdeck-tray-show";
const QUIT_ID: &str = "pingdeck-tray-quit";
const STATUS_ID: &str = "pingdeck-tray-status";
const REFRESH: Duration = Duration::from_secs(3);

/// Rasterizes PingDeck's official PingMark (`p` stem + bowl ring + origin dot from
/// `assets/icons/app-icon.svg`) as an anti-aliased 36×36 (18pt @2x) macOS template icon.
fn build_pingmark_tray_icon() -> Option<Icon> {
    const SIZE: u32 = 36;
    const SUB: usize = 4; // 4×4 supersampling for crisp Retina edges
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];

    for py in 0..SIZE {
        for px in 0..SIZE {
            let mut hits = 0u32;
            for sy in 0..SUB {
                for sx in 0..SUB {
                    // Map 36×36 canvas onto the PingMark viewBox (padding ~3px on all sides)
                    let nx = (px as f32 + (sx as f32 + 0.5) / SUB as f32) / SIZE as f32;
                    let ny = (py as f32 + (sy as f32 + 0.5) / SUB as f32) / SIZE as f32;
                    let x = 220.0 + nx * 584.0;
                    let y = 180.0 + ny * 664.0;

                    // Vertical capsule stem: rect x=282..386, y=236..788, rx=52
                    let stem_cx = 334.0f32;
                    let stem_cy = y.clamp(288.0, 736.0);
                    let in_stem = (x - stem_cx).hypot(y - stem_cy) <= 52.0;

                    // Bowl ring: center (520, 432), outer r=196, inner r=98
                    let d = (x - 520.0).hypot(y - 432.0);
                    let in_ring = (98.0..=196.0).contains(&d);

                    // Ping origin dot: center (520, 432), r=54
                    let in_dot = d <= 54.0;

                    if in_stem || in_ring || in_dot {
                        hits += 1;
                    }
                }
            }
            if hits > 0 {
                let alpha = ((hits * 255) / (SUB * SUB) as u32).min(255) as u8;
                let idx = ((py * SIZE + px) * 4) as usize;
                rgba[idx] = 0;
                rgba[idx + 1] = 0;
                rgba[idx + 2] = 0;
                rgba[idx + 3] = alpha;
            }
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).ok()
}

/// Starts the macOS menu bar item on the main thread and refreshes its localized status every 3s.
/// Never fatal: without it the Dock icon still brings the hidden window back.
pub fn start(runtime: &Arc<DesktopRuntime>, window: Arc<dyn HostWindow>, cx: &mut App) {
    let initial = tray_snapshot(runtime.state());
    let status_item = MenuItem::with_id(STATUS_ID, &initial.status, false, None);
    let show_item = MenuItem::with_id(SHOW_ID, initial.show_label, true, None);
    let quit_item = MenuItem::with_id(QUIT_ID, initial.quit_label, true, None);
    let menu = Menu::new();
    let _ = menu.append(&status_item);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&show_item);
    let _ = menu.append(&quit_item);

    let mut builder = TrayIconBuilder::new()
        .with_tooltip(&initial.status)
        .with_menu(Box::new(menu));
    if let Some(icon) = build_pingmark_tray_icon() {
        builder = builder.with_icon_templated(icon);
    } else {
        builder = builder.with_title("PD");
    }
    let tray = match builder.build() {
        Ok(tray) => tray,
        Err(error) => {
            log_line(
                LogLevel::Warn,
                "tray",
                &format!("menu bar item not shown: {error}"),
            );
            return;
        }
    };

    // Listen for tray menu clicks on a lightweight worker thread.
    let click_window = Arc::clone(&window);
    let _ = std::thread::Builder::new()
        .name("pingdeck-tray-events".into())
        .spawn(move || {
            let rx = MenuEvent::receiver();
            while let Ok(event) = rx.recv() {
                match event.id().0.as_str() {
                    SHOW_ID => click_window.show_and_focus(),
                    QUIT_ID => click_window.request_quit(),
                    _ => {}
                }
            }
        });

    // Keep `tray` and its `MenuItem`s owned on GPUI's AppKit main thread and refresh every 3s.
    let rt = Arc::clone(runtime);
    cx.spawn(async move |cx| {
        let _keep_tray_alive = tray;
        let mut last = initial;
        loop {
            cx.background_executor().timer(REFRESH).await;
            let next = match rt.run_state(tray_snapshot).await {
                Ok(snap) => snap,
                Err(_) => break,
            };
            if next == last {
                continue;
            }
            status_item.set_text(&next.status);
            if next.language != last.language {
                show_item.set_text(next.show_label);
                quit_item.set_text(next.quit_label);
            }
            last = next;
        }
    })
    .detach();
}
