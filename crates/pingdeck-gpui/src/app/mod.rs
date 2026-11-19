//! Application entry, native menus, keybindings, window lifecycle, and graceful shutdown.

pub mod actions;

use std::sync::Arc;

use gpui_kit::{
    App, AppContext as _, Bounds, KeyBinding, Menu, MenuItem, SystemMenuType, WindowBounds,
    WindowOptions, px, size,
};
use pingdeck_desktop::core::app::settings_get_impl;
use pingdeck_desktop::core::logging::{LogLevel, log_line};
use pingdeck_desktop::runtime::{DesktopRuntime, SET_ASIDE_DIALOG_TITLE};
use pingdeck_desktop::services::{
    DialogKind, HostDialog as _, RfdDialog, start_dock, start_notifications,
};

use crate::bridge::CoreBridge;
use crate::platform::{GpuiHostWindow, HostCommand, apply_app_icon, create_host_dock};
use crate::ui::{
    PingDeckAssets, apply_palette, resolve_is_dark, sync_window_scale, title_bar_options, wrap_root,
};
use crate::views::shell::AppShell;

use actions::{
    CloseOverlay, HideWindow, Nav1, Nav2, Nav3, Nav4, Nav5, Nav6, Nav7, Nav8, Nav9, OpenSettings,
    QuitApp, TogglePalette, ToggleSidebar,
};

const BUNDLE_IDENTIFIER: &str = "com.pingdeck.desktop";

/// Launches the PingDeck GPUI desktop application; exits cleanly on shutdown.
pub fn run() {
    let dialog = RfdDialog;
    let runtime = match DesktopRuntime::open_default() {
        Ok(rt) => Arc::new(rt),
        Err(failure) => {
            log_line(
                LogLevel::Error,
                "startup",
                &format!("{}: {}", failure.title, failure.message),
            );
            let kind = if failure.status == 0 {
                DialogKind::Info
            } else {
                DialogKind::Error
            };
            dialog.show_message(kind, failure.title, &failure.message);
            std::process::exit(failure.status);
        }
    };

    if let Some(notice) = runtime.set_aside_notice() {
        log_line(
            LogLevel::Warn,
            "startup",
            &format!("{SET_ASIDE_DIALOG_TITLE}: {notice}"),
        );
        dialog.show_message(DialogKind::Warning, SET_ASIDE_DIALOG_TITLE, notice);
    }

    let (host_window, host_cmd_rx) = GpuiHostWindow::pair();
    let app_runtime = Arc::clone(&runtime);
    let quit_runtime = Arc::clone(&runtime);
    let quit_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let quit_flag_close = Arc::clone(&quit_flag);

    let reopen_host = Arc::clone(&host_window);
    let app = gpui_kit::application().with_assets(PingDeckAssets);
    app.on_reopen(move |cx| {
        cx.activate(true);
        reopen_host.send(HostCommand::ShowAndFocus);
    });
    app.run(move |cx: &mut App| {
        gpui_kit::init(cx);
        apply_app_icon();
        cx.set_global(CoreBridge::new(Arc::clone(&app_runtime)));

        let initial_settings = settings_get_impl(app_runtime.state());
        let initial_ui = app_runtime.load_ui_state();
        let initial_dark = resolve_is_dark(initial_settings.appearance.theme, false);
        apply_palette(
            initial_ui.palette,
            initial_dark,
            false,
            &initial_settings.appearance,
            cx,
        );

        bind_keys_and_menus(cx);

        // Graceful core shutdown whenever the GPUI app exits (Cmd+Q, NSRunningApplication.terminate,
        // Linux window close, or tray Quit).
        let shutdown_rt = Arc::clone(&quit_runtime);
        cx.on_app_quit(move |_cx| {
            let rt = Arc::clone(&shutdown_rt);
            async move {
                let _ = std::thread::Builder::new()
                    .name("pingdeck-shutdown".into())
                    .spawn(move || rt.shutdown_blocking())
                    .map(|h| h.join());
            }
        })
        .detach();

        cx.on_action::<QuitApp>({
            let flag = Arc::clone(&quit_flag);
            move |_, cx| {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                cx.quit();
            }
        });

        let bounds = Bounds::centered(None, size(px(1280.0), px(820.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(title_bar_options()),
            window_min_size: Some(size(px(900.0), px(600.0))),
            ..WindowOptions::default()
        };

        let host_for_window = Arc::clone(&host_window);
        let close_flag = Arc::clone(&quit_flag_close);

        let open_res = gpui_kit::open_window(options, cx, move |window, cx| {
            // Sync system dark-mode appearance once the window exists.
            let sys_dark = matches!(
                window.appearance(),
                gpui_kit::WindowAppearance::Dark | gpui_kit::WindowAppearance::VibrantDark
            );
            let dark = resolve_is_dark(initial_settings.appearance.theme, sys_dark);
            apply_palette(
                initial_ui.palette,
                dark,
                false,
                &initial_settings.appearance,
                cx,
            );
            sync_window_scale(&initial_settings.appearance, window);

            // macOS red-button hides the main window while gateway/mock/MCP/agent services
            // keep serving; Cmd+Q / terminate quits cleanly.
            #[cfg(target_os = "macos")]
            window.on_window_should_close(cx, move |_window, cx| {
                if close_flag.load(std::sync::atomic::Ordering::SeqCst) {
                    true
                } else {
                    cx.hide();
                    false
                }
            });
            #[cfg(not(target_os = "macos"))]
            let _ = close_flag;

            let shell =
                cx.new(|cx| AppShell::new(host_cmd_rx, Arc::clone(&host_for_window), window, cx));
            wrap_root(shell, window, cx)
        });

        if let Err(error) = open_res {
            log_line(
                LogLevel::Error,
                "startup",
                &format!("failed to open main window: {error}"),
            );
            cx.quit();
            return;
        }

        // Start OS background services after window creation.
        let dock = create_host_dock(&app_runtime, cx);
        start_dock(&app_runtime, Arc::clone(&host_window) as _, dock);
        start_notifications(
            &app_runtime,
            Arc::clone(&host_window) as _,
            BUNDLE_IDENTIFIER,
        );

        #[cfg(target_os = "macos")]
        crate::platform::tray_macos::start(&app_runtime, Arc::clone(&host_window) as _, cx);

        cx.activate(true);
    });
}

fn bind_keys_and_menus(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", QuitApp, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("cmd-k", TogglePalette, None),
        KeyBinding::new("cmd-1", Nav1, None),
        KeyBinding::new("cmd-2", Nav2, None),
        KeyBinding::new("cmd-3", Nav3, None),
        KeyBinding::new("cmd-4", Nav4, None),
        KeyBinding::new("cmd-5", Nav5, None),
        KeyBinding::new("cmd-6", Nav6, None),
        KeyBinding::new("cmd-7", Nav7, None),
        KeyBinding::new("cmd-8", Nav8, None),
        KeyBinding::new("cmd-9", Nav9, None),
        KeyBinding::new("escape", CloseOverlay, None),
    ]);

    cx.set_menus(vec![
        Menu {
            name: "PingDeck".into(),
            items: vec![
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide PingDeck", HideWindow),
                MenuItem::separator(),
                MenuItem::action("Quit PingDeck", QuitApp),
            ],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Toggle Sidebar", ToggleSidebar),
                MenuItem::action("Search or Jump To…", TogglePalette),
            ],
            disabled: false,
        },
    ]);
}
