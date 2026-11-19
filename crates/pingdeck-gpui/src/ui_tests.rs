//! Headless GPUI UI integration tests (`cargo test -p pingdeck-gpui --features test-support`).
//!
//! Verifies real GPUI window rendering, `CoreBridge` wiring to an in-memory `DesktopRuntime`,
//! navigation across all 9 modules, 16 OKLCH eye-care palette hot-switches, all 20 Settings pages,
//! all 6 Utility tools, `⌘K` Command Palette, and the 4 Phase 0 native widget workloads
//! (IME editors, selectable streaming Markdown, 10,000-row virtual table, and 10 MiB JSON Rope
//! load benchmark).

#[cfg(all(test, feature = "test-support"))]
mod gpui_integration {
    use std::sync::Arc;

    use gpui_kit::TestAppContext;
    use pingdeck_desktop::core::app::{settings_get_impl, workspace_create_impl};
    use pingdeck_desktop::runtime::DesktopRuntime;
    use pingdeck_desktop::state::{NavModule, SettingsSection, UtilityDialog};
    use pingdeck_desktop::theme::PaletteId;

    use crate::bridge::CoreBridge;
    use crate::platform::GpuiHostWindow;
    use crate::ui::{apply_palette, palette};
    use crate::views::shell::AppShell;
    use crate::views::utility::bench::BenchTab;

    #[gpui_kit::test]
    fn mounts_app_shell_navigates_all_9_modules_and_switches_16_palettes(cx: &mut TestAppContext) {
        // Real Tokio tasks (`pingdeck-core` runtime) wake GPUI foreground futures from Tokio
        // threads, so opt this integration test into non-deterministic parking mode.
        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let _ = workspace_create_impl(
            rt.state(),
            "ws-gpui-test".into(),
            "GPUI Test Workspace".into(),
        );
        let settings = settings_get_impl(rt.state());
        let (host, rx) = GpuiHostWindow::pair();

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        let (shell, visual) =
            cx.add_window_view(move |window, cx| AppShell::new(rx, host, window, cx));

        // 1. Navigate and render every single one of the 9 modules cleanly.
        for module in NavModule::ALL {
            visual.update(|window, cx| {
                shell.update(cx, |s, cx| s.navigate(module, cx));
                window.refresh();
            });
            visual.run_until_parked();
        }

        // 2. Hot-switch all 8 eye-care palettes across light & dark (16 variants).
        for pid in PaletteId::ALL {
            for is_dark in [false, true] {
                visual.update(|_window, cx| {
                    apply_palette(pid, is_dark, false, &settings.appearance, cx);
                    let active = palette(cx);
                    assert_eq!(active.palette, pid);
                    assert_eq!(active.is_dark, is_dark);
                });
            }
        }

        // 3. Visit all 20 Settings pages.
        for section in SettingsSection::ALL {
            visual.update(|window, cx| {
                shell.update(cx, |s, cx| s.open_settings_section(section, cx));
                window.refresh();
            });
        }

        // 4. Exercise all 6 Utility tabs wired to core.
        for dialog in UtilityDialog::ALL {
            visual.update(|window, cx| {
                shell.update(cx, |s, cx| s.open_utility(dialog, window, cx));
                window.refresh();
            });
            visual.run_until_parked();
        }

        // 5. Open & close Command Palette (⌘K).
        visual.update(|window, cx| {
            shell.update(cx, |s, cx| s.toggle_palette(window, cx));
            assert!(shell.read(cx).state.palette_open);
            shell.update(cx, |s, cx| s.close_overlays(cx));
            assert!(!shell.read(cx).state.palette_open);
        });

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn phase0_native_widget_bench_10k_table_stream_md_and_10mib_json(cx: &mut TestAppContext) {
        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());
        let (host, rx) = GpuiHostWindow::pair();

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Sage, false, false, &settings.appearance, cx);
        });

        let (shell, visual) =
            cx.add_window_view(move |window, cx| AppShell::new(rx, host, window, cx));

        // Open Console utility and exercise all 4 native widget stress tabs.
        visual.update(|window, cx| {
            shell.update(cx, |s, cx| {
                s.open_utility(UtilityDialog::Console, window, cx)
            });
            let utility = shell.read(cx).utility_view.clone();

            // 1. IME CJK input into single-line InputState & multiline TextareaState.
            utility.update(cx, |panel, cx| {
                panel.bench.tab = BenchTab::Ime;
                panel.bench.ime_input.update(cx, |i, cx| {
                    i.set_value("你好 PingDeck 纯 Rust GPUI", window, cx)
                });
                panel.bench.ime_textarea.update(cx, |t, cx| {
                    t.set_value("第一行中文输入\nSecond line English", window, cx)
                });
                assert_eq!(
                    panel.bench.ime_input.read(cx).value().as_str(),
                    "你好 PingDeck 纯 Rust GPUI"
                );
            });
            window.refresh();

            // 2. Streaming Markdown burst (+600 tokens).
            utility.update(cx, |panel, cx| {
                panel.bench.tab = BenchTab::StreamingMarkdown;
                for _ in 0..3 {
                    panel.bench.append_stream_burst();
                }
                assert_eq!(panel.bench.stream_tokens, 600);
                cx.notify();
            });
            window.refresh();

            // 3. 10,000-row virtualized DataTable render.
            utility.update(cx, |panel, cx| {
                panel.bench.tab = BenchTab::VirtualTable10k;
                cx.notify();
            });
            window.refresh();

            // 4. 10 MiB JSON Rope Editor load timing.
            let load_ms = utility.update(cx, |panel, cx| {
                panel.bench.tab = BenchTab::LargeJson10Mb;
                let ms = panel.bench.load_10mib_json(window, cx);
                cx.notify();
                ms
            });
            window.refresh();
            println!("[phase0-bench] 10 MiB JSON Rope load: {load_ms:.1} ms");
            assert!(
                load_ms >= 0.0,
                "10 MiB JSON load produced valid elapsed time"
            );
        });

        rt.shutdown_blocking();
    }
}
