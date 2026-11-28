//! Headless GPUI UI integration tests (`cargo test -p spanbay-gpui --features test-support`).
//!
//! Verifies real GPUI window rendering, `CoreBridge` wiring to an in-memory `DesktopRuntime`,
//! navigation across all 10 modules (including dedicated Local Gateway), 16 OKLCH eye-care
//! palette hot-switches, all 20 Settings pages, unified Import/Export modal & Diagnostics
//! slide-over sheet, `⌘K` Command Palette deep-links, and the 4 Phase 0 native widget workloads
//! (IME editors, selectable streaming Markdown, 10,000-row virtual table, and 10 MiB JSON Rope
//! load benchmark).

#[cfg(all(test, feature = "test-support"))]
mod gpui_integration {
    use std::sync::Arc;

    use gpui_kit::{AppContext as _, TestAppContext};
    use spanbay_desktop::core::app::{settings_get_impl, workspace_create_impl};
    use spanbay_desktop::runtime::DesktopRuntime;
    use spanbay_desktop::state::{NavModule, SettingsSection, UtilityDialog};
    use spanbay_desktop::theme::PaletteId;

    use crate::bridge::CoreBridge;
    use crate::platform::GpuiHostWindow;
    use crate::ui::{Root, apply_palette, palette};
    use crate::views::shell::AppShell;
    use crate::views::utility::bench::BenchTab;
    use crate::views::utility::diagnostics::ConsoleMode;

    #[gpui_kit::test]
    fn mounts_app_shell_navigates_all_10_modules_and_switches_16_palettes(cx: &mut TestAppContext) {
        // Real Tokio tasks (`spanbay-core` runtime) wake GPUI foreground futures from Tokio
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

        let mut shell_slot = None;
        let (_root, visual) = cx.add_window_view(|window, cx| {
            let s = cx.new(|cx| AppShell::new(rx, host, window, cx));
            shell_slot = Some(s.clone());
            Root::new(s, window, cx)
        });
        let shell = shell_slot.expect("mounted AppShell");

        // 1. Navigate and render every single one of the 11 modules cleanly (including Runs).
        assert_eq!(NavModule::ALL.len(), 11);
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

        // 4. Exercise both focused shell overlays (Import/Export modal & Diagnostics slide-over sheet).
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

        let mut shell_slot = None;
        let (_root, visual) = cx.add_window_view(|window, cx| {
            let s = cx.new(|cx| AppShell::new(rx, host, window, cx));
            shell_slot = Some(s.clone());
            Root::new(s, window, cx)
        });
        let shell = shell_slot.expect("mounted AppShell");

        // Open Diagnostics slide-over sheet in GPU Bench mode and exercise all 4 native widget stress tabs.
        visual.update(|window, cx| {
            shell.update(cx, |s, cx| {
                s.open_utility(UtilityDialog::Diagnostics, window, cx)
            });
            let diag = shell.read(cx).diagnostics_view.clone();

            // 1. IME CJK input into single-line InputState & multiline TextareaState.
            diag.update(cx, |panel, cx| {
                panel.console_mode = ConsoleMode::GpuBench;
                panel.bench.tab = BenchTab::Ime;
                panel.bench.ime_input.update(cx, |i, cx| {
                    i.set_value("你好 SpanBay 纯 Rust GPUI", window, cx)
                });
                panel.bench.ime_textarea.update(cx, |t, cx| {
                    t.set_value("第一行中文输入\nSecond line English", window, cx)
                });
                assert_eq!(
                    panel.bench.ime_input.read(cx).value().as_str(),
                    "你好 SpanBay 纯 Rust GPUI"
                );
            });
            window.refresh();

            // 2. Streaming Markdown burst (+600 tokens).
            diag.update(cx, |panel, cx| {
                panel.bench.tab = BenchTab::StreamingMarkdown;
                for _ in 0..3 {
                    panel.bench.append_stream_burst();
                }
                assert_eq!(panel.bench.stream_tokens, 600);
                cx.notify();
            });
            window.refresh();

            // 3. 10,000-row virtualized DataTable render.
            diag.update(cx, |panel, cx| {
                panel.bench.tab = BenchTab::VirtualTable10k;
                cx.notify();
            });
            window.refresh();

            // 4. 10 MiB JSON Rope Editor load timing.
            let load_ms = diag.update(cx, |panel, cx| {
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

    #[gpui_kit::test]
    fn requests_workbench_full_lifecycle(cx: &mut TestAppContext) {
        use spanbay_desktop::core::app::snippet_service::SnippetLanguage;
        use spanbay_desktop::core::model::ai_request::RequestMode;
        use spanbay_desktop::core::model::{AuthConfig, RequestBody};

        use crate::views::requests::RequestsView;
        use crate::views::requests::model::{
            AiEdTab, HttpEdTab, NewItemKind, OpenTab, RequestsHandoff,
        };

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let _ = workspace_create_impl(
            rt.state(),
            "ws-req-test".into(),
            "Requests Test Workspace".into(),
        );
        let settings = settings_get_impl(rt.state());

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        let (view, visual) = cx.add_window_view(|window, cx| {
            RequestsView::new(Some("ws-req-test".into()), window, cx)
        });
        visual.run_until_parked();

        // 1. Create a collection and wait for Tokio blocking save + reload to land in GPUI.
        visual.update(|_window, cx| {
            view.update(cx, |v, cx| v.create_collection("Core API".into(), cx));
        });
        for _ in 0..80 {
            visual.run_until_parked();
            let found = visual.update(|_window, cx| {
                view.read(cx)
                    .collections
                    .iter()
                    .any(|c| c.name == "Core API")
            });
            if found {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        visual.update(|window, cx| {
            assert!(
                view.read(cx)
                    .collections
                    .iter()
                    .any(|c| c.name == "Core API"),
                "collection should be created and reloaded (status={:?})",
                view.read(cx).status_msg
            );
            window.refresh();
        });

        // 2. Open isolated HTTP GET, HTTP POST, GraphQL, and AI API tabs.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.open_new_request_tab(NewItemKind::HttpGet, None, None, window, cx);
                v.open_new_request_tab(NewItemKind::HttpPost, None, None, window, cx);
                v.open_new_request_tab(NewItemKind::Graphql, None, None, window, cx);
                v.open_new_request_tab(NewItemKind::AiApi, None, None, window, cx);
            });
            assert!(view.read(cx).tabs.len() >= 4);
            window.refresh();
        });

        // 3. Cycle all 6 AI editor sub-tabs and add an AI message.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.ai_msg_new_input
                    .update(cx, |t, cx| t.set_value("Hello from GPUI test", window, cx));
                v.ai_add_message(window, cx);
            });
            for idx in 0..6 {
                view.update(cx, |v, cx| {
                    if let Some(tab) = v.active_req_mut() {
                        tab.ai_ed_tab = AiEdTab::from_index(idx);
                    }
                    cx.notify();
                });
                window.refresh();
            }
        });

        // 4. Switch active AI request safely back to HTTP mode & verify URL <-> Params/PathVars sync.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.switch_active_request_mode(RequestMode::Http, window, cx);
                v.url_input.update(cx, |i, cx| {
                    i.set_value(
                        "https://api.example.com/users/:id?page=2&limit=20",
                        window,
                        cx,
                    );
                });
                v.on_url_input_changed(cx);
            });
            let v = view.read(cx);
            let tab = v.active_req().expect("active request tab");
            assert_eq!(tab.draft.mode, RequestMode::Http);
            assert_eq!(tab.draft.path_variables.len(), 1);
            assert_eq!(tab.draft.path_variables[0].key, "id");
            assert_eq!(tab.draft.query_params.len(), 2);
            window.refresh();
        });

        // 5. Cycle all 8 HTTP editor sub-tabs (including Auth schemes & Body modes).
        visual.update(|window, cx| {
            for idx in 0..8 {
                view.update(cx, |v, cx| {
                    if let Some(tab) = v.active_req_mut() {
                        tab.http_ed_tab = HttpEdTab::from_index(idx);
                        if idx == HttpEdTab::Auth.index() {
                            tab.draft.auth = AuthConfig::Bearer {
                                token_ref: "{{api_token}}".into(),
                            };
                        } else if idx == HttpEdTab::Body.index() {
                            tab.draft.body = RequestBody::Json {
                                content: "{\"hello\":\"world\"}".into(),
                            };
                        }
                    }
                    cx.notify();
                });
                window.refresh();
            }
        });

        // 6. Code snippets generation across multiple languages.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.open_code_snippets_modal(window, cx);
                for lang in SnippetLanguage::ALL {
                    v.refresh_snippet(lang, false, window, cx);
                    assert!(
                        !v.snippet_editor.read(cx).value().trim().is_empty(),
                        "snippet for {lang:?} should be non-empty"
                    );
                }
                assert!(v.dismiss_modal_if_open(cx));
            });
            window.refresh();
        });

        // 7. Import / Export delegates cleanly to the unified shell ImportExportModal handoff.
        visual.update(|window, cx| {
            let col_id = view.read(cx).collections[0].id.clone();
            view.update(cx, |v, cx| {
                v.pending_handoff
                    .set(Some(RequestsHandoff::OpenImportExport {
                        export_collection_id: Some(col_id.clone()),
                    }));
                cx.notify();
            });
            assert!(matches!(
                view.read(cx).take_pending_handoff(),
                Some(RequestsHandoff::OpenImportExport {
                    export_collection_id: Some(ref id)
                }) if id == &col_id
            ));
            window.refresh();
        });

        // 8. Open Collection Settings & Collection Runner tabs, toggle Console drawer, dirty-close guard.
        visual.update(|window, cx| {
            let col_id = view.read(cx).collections[0].id.clone();
            view.update(cx, |v, cx| {
                v.open_collection_settings_tab(col_id.clone(), window, cx);
            });
            assert!(matches!(
                view.read(cx).active_tab(),
                Some(OpenTab::Container(_))
            ));
            window.refresh();

            view.update(cx, |v, cx| {
                v.open_collection_runner_tab(col_id, None, window, cx);
            });
            assert!(matches!(
                view.read(cx).active_tab(),
                Some(OpenTab::Runner(_))
            ));
            window.refresh();

            view.update(cx, |v, cx| {
                v.handle_toggle_console(cx);
                assert!(v.console_open);
                v.handle_toggle_console(cx);
                assert!(!v.console_open);
            });
            window.refresh();
        });

        // 9. Verify full-window `ui::modal_dialog` rendering + Enter submit for NewItem modal.
        visual.update(|window, cx| {
            let before = view.read(cx).tabs.len();
            view.update(cx, |v, cx| {
                v.open_new_item_modal(NewItemKind::HttpPost, None, None, window, cx);
                v.modal_name_input
                    .update(cx, |i, cx| i.set_value("Create Order", window, cx));
            });
            window.refresh();
            view.update(cx, |v, cx| {
                v.submit_active_modal_on_enter(window, cx);
            });
            assert!(view.read(cx).active_modal.is_none());
            assert_eq!(view.read(cx).tabs.len(), before + 1);
            assert_eq!(
                view.read(cx).active_req().map(|t| t.draft.name.as_str()),
                Some("Create Order")
            );
            window.refresh();
        });

        // 10. Switching to a brand-new empty workspace automatically opens a ready draft tab.
        let _ = workspace_create_impl(
            rt.state(),
            "ws-req-empty".into(),
            "Fresh Empty Workspace".into(),
        );
        visual.update(|_window, cx| {
            view.update(cx, |v, cx| {
                v.set_workspace(Some("ws-req-empty".into()), cx);
            });
        });
        for _ in 0..80 {
            visual.run_until_parked();
            let has_tab = visual.update(|_window, cx| !view.read(cx).tabs.is_empty());
            if has_tab {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        visual.update(|window, cx| {
            assert!(
                !view.read(cx).tabs.is_empty(),
                "fresh empty workspace must auto-open a starter draft tab"
            );
            assert!(view.read(cx).active_req().is_some());
            window.refresh();
        });

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn settings_all_20_pages_and_mutations_lifecycle(cx: &mut TestAppContext) {
        use spanbay_desktop::core::model::settings::{Language, SettingsSection as CoreSection};

        use crate::views::settings::SettingsView;
        use crate::views::settings::state_inputs::{
            ProxyModeChoice, SettingsModal, section_matches_query,
        };

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        let (view, visual) = cx.add_window_view(|window, cx| {
            SettingsView::new(
                SettingsSection::General,
                spanbay_desktop::theme::ThemePreferences::default(),
                window,
                cx,
            )
        });
        visual.run_until_parked();

        // Helper to wait for async blocking settings commits to land in GPUI.
        fn wait_for(
            visual: &mut gpui_kit::VisualTestContext,
            pred: impl Fn(&gpui_kit::App) -> bool,
        ) -> bool {
            for _ in 0..200 {
                visual.run_until_parked();
                if visual.update(|window, cx| {
                    window.refresh();
                    pred(cx)
                }) {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            false
        }

        // 1. Bilingual keyword search finds expected sections.
        assert!(section_matches_query(SettingsSection::Network, "代理"));
        assert!(section_matches_query(SettingsSection::Routing, "circuit"));
        assert!(section_matches_query(SettingsSection::Data, "backup"));
        assert!(section_matches_query(SettingsSection::Shortcuts, "快捷键"));

        // 2. Render all 20 Settings pages cleanly in zh-CN.
        for section in SettingsSection::ALL {
            visual.update(|window, cx| {
                view.update(cx, |v, cx| v.set_section(section, cx));
                window.refresh();
            });
            visual.run_until_parked();
        }

        // 3. Edit Request Defaults, Mock, Routing, and Advanced fields and verify persistence.
        visual.update(|_window, cx| {
            view.update(cx, |v, cx| {
                v.commit_u64(
                    "45000",
                    1,
                    3_600_000,
                    CoreSection::Network,
                    |n| serde_json::json!({ "timeout_ms": n }),
                    cx,
                );
                v.patch_section(
                    CoreSection::Editor,
                    serde_json::json!({ "tab_size": 4 }),
                    cx,
                );
                v.commit_u64(
                    "125",
                    0,
                    60_000,
                    CoreSection::Mock,
                    |n| serde_json::json!({ "default_latency_ms": n }),
                    cx,
                );
                v.commit_u64(
                    "6",
                    0,
                    10,
                    CoreSection::Ai,
                    |n| serde_json::json!({ "gateway": { "max_retries": n } }),
                    cx,
                );
                v.commit_u64(
                    "512",
                    16,
                    4_096,
                    CoreSection::Advanced,
                    |n| serde_json::json!({ "script_recursion_limit": n }),
                    cx,
                );
            });
        });
        let synced = wait_for(visual, |cx| {
            let s = &view.read(cx).settings;
            s.network.timeout_ms == 45_000
                && s.editor.tab_size == 4
                && s.mock.default_latency_ms == 125
                && s.ai.gateway.max_retries == 6
                && s.advanced.script_recursion_limit == 512
        });
        if !synced {
            let (view_s, flash) = visual
                .update(|_, cx| (view.read(cx).settings.clone(), view.read(cx).flash.clone()));
            let core_s = settings_get_impl(rt.state());
            panic!(
                "settings patches must persist and sync back into SettingsView\n\
                 view: timeout={} tab={} mock={} retries={} rec={} flash={flash:?}\n\
                 core: timeout={} tab={} mock={} retries={} rec={}",
                view_s.network.timeout_ms,
                view_s.editor.tab_size,
                view_s.mock.default_latency_ms,
                view_s.ai.gateway.max_retries,
                view_s.advanced.script_recursion_limit,
                core_s.network.timeout_ms,
                core_s.editor.tab_size,
                core_s.mock.default_latency_ms,
                core_s.ai.gateway.max_retries,
                core_s.advanced.script_recursion_limit,
            );
        }

        // 4. Invalid numeric input surfaces error flash & error pill state cleanly.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.commit_u64(
                    "not-a-number",
                    1,
                    100,
                    CoreSection::Network,
                    |n| serde_json::json!({ "timeout_ms": n }),
                    cx,
                );
            });
            assert!(view.read(cx).last_save_error);
            assert!(view.read(cx).flash.is_some());
            window.refresh();
        });

        // 5. Shortcut override: reject conflicting key, accept unique custom key, then restore.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.set_section(SettingsSection::Shortcuts, cx);
                v.start_edit_shortcut("app.palette", window, cx);
                // Conflict with app.sidebar (`cmd-b`) must be rejected.
                v.inputs
                    .shortcut_key
                    .update(cx, |i, cx| i.set_value("cmd-b", window, cx));
                v.save_shortcut_override("app.palette", window, cx);
            });
            assert!(view.read(cx).editing_shortcut == Some("app.palette"));

            view.update(cx, |v, cx| {
                v.inputs
                    .shortcut_key
                    .update(cx, |i, cx| i.set_value("cmd-shift-p", window, cx));
                v.save_shortcut_override("app.palette", window, cx);
            });
            assert!(view.read(cx).editing_shortcut.is_none());
            window.refresh();
        });
        assert!(
            wait_for(visual, |cx| {
                view.read(cx)
                    .settings
                    .shortcuts
                    .overrides
                    .get("app.palette")
                    .map(String::as_str)
                    == Some("cmd-shift-p")
            }),
            "custom shortcut override must persist"
        );

        // 6. Proxy mode toggle (Direct) and no_proxy add/remove.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.set_section(SettingsSection::Network, cx);
                v.select_proxy_mode(ProxyModeChoice::Direct, cx);
                v.inputs
                    .no_proxy_add
                    .update(cx, |i, cx| i.set_value("corp.internal", window, cx));
                v.add_no_proxy_entry(window, cx);
            });
            window.refresh();
        });
        assert!(
            wait_for(visual, |cx| {
                let net = &view.read(cx).settings.network;
                net.proxy_url == "direct" && net.no_proxy.iter().any(|h| h == "corp.internal")
            }),
            "proxy mode and no_proxy edits must persist"
        );

        // 7. Reset-All confirmation modal render + Esc dismiss + Reset All + 60s Undo restore.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.set_section(SettingsSection::Advanced, cx);
                v.modal = Some(SettingsModal::ConfirmResetAll);
                cx.notify();
            });
            window.refresh();
            let dismissed = view.update(cx, |v, cx| v.dismiss_modal_if_open(cx));
            assert!(dismissed);
            assert!(view.read(cx).modal.is_none());

            view.update(cx, |v, cx| v.reset_all(cx));
        });
        assert!(
            wait_for(visual, |cx| {
                let v = view.read(cx);
                v.can_undo_reset_all
                    && v.settings.network.timeout_ms == 30_000
                    && v.settings.editor.tab_size == 2
                    && v.settings.shortcuts.overrides.is_empty()
            }),
            "reset_all must restore factory defaults and arm undo"
        );

        visual.update(|_window, cx| {
            view.update(cx, |v, cx| v.undo_last_reset(cx));
        });
        assert!(
            wait_for(visual, |cx| {
                let v = view.read(cx);
                !v.can_undo_reset_all
                    && v.settings.network.timeout_ms == 45_000
                    && v.settings.editor.tab_size == 4
                    && v.settings
                        .shortcuts
                        .overrides
                        .get("app.palette")
                        .map(String::as_str)
                        == Some("cmd-shift-p")
            }),
            "undo_last_reset must restore prior custom settings"
        );

        // 8. Switch language to English and render key pages cleanly.
        visual.update(|_window, cx| {
            view.update(cx, |v, cx| {
                v.patch_section(
                    CoreSection::General,
                    serde_json::json!({ "language": "en" }),
                    cx,
                );
            });
        });
        assert!(
            wait_for(visual, |cx| {
                view.read(cx).settings.general.language == Language::En
            }),
            "language switch to English must persist"
        );
        for section in [
            SettingsSection::General,
            SettingsSection::Appearance,
            SettingsSection::Shortcuts,
            SettingsSection::Requests,
            SettingsSection::Network,
            SettingsSection::Mock,
            SettingsSection::Data,
            SettingsSection::Routing,
            SettingsSection::Usage,
            SettingsSection::Advanced,
        ] {
            visual.update(|window, cx| {
                view.update(cx, |v, cx| v.set_section(section, cx));
                window.refresh();
            });
        }

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn mock_workbench_full_lifecycle(cx: &mut TestAppContext) {
        use crate::views::mock::state::{MockImportFormat, MockModal, MockSubTab};
        use crate::views::mock::{MockHandoff, MockView};

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());
        workspace_create_impl(rt.state(), "ws-mock-ui".into(), "Mock WS".into())
            .expect("create workspace");

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        let (view, visual) =
            cx.add_window_view(|window, cx| MockView::new(Some("ws-mock-ui".into()), window, cx));
        visual.run_until_parked();

        let wait_for = |visual: &mut gpui_kit::VisualTestContext,
                        pred: &dyn Fn(&gpui_kit::App) -> bool|
         -> bool {
            for _ in 0..80 {
                visual.run_until_parked();
                if visual.update(|window, cx| {
                    window.refresh();
                    pred(cx)
                }) {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            false
        };

        // 1. Create a mock server and verify it populates draft + default GET route.
        visual.update(|_w, cx| {
            view.update(cx, |v, cx| v.create_server(cx));
        });
        assert!(
            wait_for(visual, &|cx| view.read(cx).draft.is_some()),
            "created mock server should become active draft"
        );

        // 2. Add CRUD route, second response branch, matching rule, data bucket, callback, and save.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.add_route("CRUD", cx);
                v.add_route("POST", cx);
                v.add_response(cx);
                v.rule_mod_input
                    .update(cx, |s, cx| s.set_value("role", window, cx));
                v.rule_val_input
                    .update(cx, |s, cx| s.set_value("admin", window, cx));
                v.add_rule_to_response(window, cx);
                v.add_bucket(cx);
                v.add_callback(cx);
                v.save_draft(cx);
            });
            window.refresh();
        });
        assert!(
            wait_for(visual, &|cx| !view.read(cx).dirty),
            "mock draft save should clear dirty state"
        );

        // 3. Cycle all 7 sub-tabs cleanly and verify test-in-Requests handoff.
        visual.update(|window, cx| {
            let route_id = view
                .read(cx)
                .draft
                .as_ref()
                .and_then(|s| s.routes.first())
                .map(|r| r.id.clone())
                .expect("at least one route");
            view.update(cx, |v, cx| {
                for tab in MockSubTab::ALL {
                    v.active_tab = tab;
                    cx.notify();
                }
                v.test_route_in_requests(&route_id, cx);
            });
            window.refresh();
            assert!(matches!(
                view.read(cx).take_pending_handoff(),
                Some(MockHandoff::OpenTestRequest { .. })
            ));
        });

        // 4. Open and dismiss Import & Delete modals, then delete the server.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.active_modal = Some(MockModal::Import {
                    format: MockImportFormat::OpenApi,
                });
                cx.notify();
            });
            window.refresh();
            assert!(view.update(cx, |v, cx| v.dismiss_modal_if_open(cx)));
            let sid = view.read(cx).selected_id.clone().expect("selected server");
            view.update(cx, |v, cx| v.delete_server(sid, cx));
        });
        assert!(
            wait_for(visual, &|cx| view.read(cx).servers.is_empty()),
            "deleted mock server should be removed"
        );

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn flows_workbench_full_lifecycle(cx: &mut TestAppContext) {
        use crate::views::flows::FlowsView;
        use crate::views::flows::state::{FlowModal, FlowStepTab};
        use spanbay_desktop::core::model::items::{FlowLayoutDirectionPref, FlowStepKind};
        use spanbay_desktop::state::FlowViewMode;

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());
        workspace_create_impl(rt.state(), "ws-flow-ui".into(), "Flow WS".into())
            .expect("create workspace");

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        let (view, visual) =
            cx.add_window_view(|window, cx| FlowsView::new(Some("ws-flow-ui".into()), window, cx));
        visual.run_until_parked();

        let wait_for = |visual: &mut gpui_kit::VisualTestContext,
                        pred: &dyn Fn(&gpui_kit::App) -> bool|
         -> bool {
            for _ in 0..80 {
                visual.run_until_parked();
                if visual.update(|window, cx| {
                    window.refresh();
                    pred(cx)
                }) {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            false
        };

        // 1. Create a flow, add a second step, reorder, add extraction & assertion, save.
        visual.update(|_w, cx| {
            view.update(cx, |v, cx| v.create_flow(cx));
        });
        assert!(
            wait_for(visual, &|cx| view.read(cx).draft.is_some()),
            "created flow should load into draft"
        );

        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.add_inline_step(cx);
                v.move_step(1, 0, cx);
                v.ext_var_input
                    .update(cx, |s, cx| s.set_value("auth_token", window, cx));
                v.ext_expr_input
                    .update(cx, |s, cx| s.set_value("$.token", window, cx));
                v.new_ext_secret = true;
                v.add_extraction(window, cx);
                v.assert_val_input
                    .update(cx, |s, cx| s.set_value("200", window, cx));
                v.add_quick_assertion(window, cx);
                v.save_flow(cx);
            });
            window.refresh();
        });
        assert!(
            wait_for(visual, &|cx| {
                let v = view.read(cx);
                !v.dirty && v.draft.as_ref().is_some_and(|f| f.steps.len() == 2)
            }),
            "saved flow should persist 2 steps"
        );

        // 2. Exercise unified ProcessView modes (Graph / Steps / Timeline), DAG Condition & Join
        //    nodes, swimlanes, LR/TB/Wrap layout toggles, hybrid pin/reset, Inspector tabs, and modals.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.add_control_step(FlowStepKind::Condition, cx);
                v.add_control_step(FlowStepKind::Join, cx);
                v.set_layout_direction(FlowLayoutDirectionPref::TopToBottom, cx);
                v.set_layout_direction(FlowLayoutDirectionPref::Serpentine, cx);
                v.toggle_swimlanes(cx);
                v.toggle_center_align(cx);
                v.toggle_data_flow(cx);
                if let Some(sid) = v.selected_step_id.clone()
                    && let Some(flow) = v.draft.as_mut()
                {
                    flow.options.pinned_positions.insert(sid, (380, 140));
                }
                for mode in [
                    FlowViewMode::Graph,
                    FlowViewMode::Steps,
                    FlowViewMode::Timeline,
                ] {
                    v.set_view_mode(mode, cx);
                }
                v.set_view_mode(FlowViewMode::Graph, cx);
                for tab in FlowStepTab::ALL {
                    v.step_tab = tab;
                    cx.notify();
                }
                v.reset_pinned_layout(cx);
                v.active_modal = Some(FlowModal::Variables);
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear(cx);
            assert!(view.update(cx, |v, cx| v.dismiss_modal_if_open(cx)));

            view.update(cx, |v, cx| {
                v.active_modal = Some(FlowModal::Options);
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear(cx);
            assert!(view.update(cx, |v, cx| v.dismiss_modal_if_open(cx)));
            let fid = view.read(cx).selected_flow_id.clone().expect("flow id");
            view.update(cx, |v, cx| v.delete_flow(fid, cx));
        });
        assert!(
            wait_for(visual, &|cx| view.read(cx).entries.is_empty()),
            "deleted flow should be removed"
        );

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn history_workbench_filters_undo_and_handoff(cx: &mut TestAppContext) {
        use spanbay_desktop::core::app::history_restore_many_impl;
        use spanbay_desktop::core::model::{ExecutionHistoryEntry, HistorySource, Request};

        use crate::views::history::state::HistoryDetailTab;
        use crate::views::history::{HistoryHandoff, HistoryView};

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());
        workspace_create_impl(rt.state(), "ws-hist-ui".into(), "Hist WS".into())
            .expect("create workspace");

        let seed_req = Request::new(
            "req_hist_seed",
            "ws-hist-ui",
            "List Users",
            "https://api.example.com/users",
        )
        .expect("seed request");
        let entry = ExecutionHistoryEntry::from_success(
            "hist_ui_1",
            &seed_req,
            None,
            200,
            "OK",
            "https://api.example.com/users",
            42,
            "{\"users\":[]}",
            12,
            1,
            "1700000000000",
        )
        .expect("build history entry");
        history_restore_many_impl(rt.state(), vec![entry]).expect("seed history row");

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        let (view, visual) = cx
            .add_window_view(|window, cx| HistoryView::new(Some("ws-hist-ui".into()), window, cx));
        visual.run_until_parked();

        let wait_for = |visual: &mut gpui_kit::VisualTestContext,
                        pred: &dyn Fn(&gpui_kit::App) -> bool|
         -> bool {
            for _ in 0..80 {
                visual.run_until_parked();
                if visual.update(|window, cx| {
                    window.refresh();
                    pred(cx)
                }) {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            false
        };

        assert!(
            wait_for(visual, &|cx| view.read(cx).entries.len() == 1),
            "seeded history entry should load"
        );

        // 1. Cycle all 6 detail tabs and verify OpenInRequests + AskAgent handoffs.
        visual.update(|window, cx| {
            let active = view.read(cx).active_entry().cloned().expect("active entry");
            view.update(cx, |v, cx| {
                for tab in HistoryDetailTab::ALL {
                    v.detail_tab = tab;
                    cx.notify();
                }
                v.open_in_requests(&active, cx);
            });
            window.refresh();
            assert!(matches!(
                view.read(cx).take_pending_handoff(),
                Some(HistoryHandoff::OpenInRequests(_))
            ));
            view.update(cx, |v, cx| v.ask_agent(&active, cx));
            assert!(matches!(
                view.read(cx).take_pending_handoff(),
                Some(HistoryHandoff::AskAgent { .. })
            ));
        });

        // 2. Filter by Mock source (0 hits), reset filter (1 hit), then delete & undo restore!
        visual.update(|_w, cx| {
            view.update(cx, |v, cx| v.toggle_source(HistorySource::Mock, cx));
        });
        assert!(
            wait_for(visual, &|cx| view.read(cx).entries.is_empty()),
            "filtering by Mock source should yield 0 rows"
        );
        visual.update(|window, cx| {
            view.update(cx, |v, cx| v.reset_filters(window, cx));
        });
        assert!(
            wait_for(visual, &|cx| view.read(cx).entries.len() == 1),
            "resetting filters should bring row back"
        );

        visual.update(|_w, cx| {
            view.update(cx, |v, cx| v.delete_entry("hist_ui_1".into(), cx));
        });
        assert!(
            wait_for(visual, &|cx| {
                let v = view.read(cx);
                v.entries.is_empty() && v.last_undo.is_some()
            }),
            "deleting entry should arm undo token"
        );
        visual.update(|_w, cx| {
            view.update(cx, |v, cx| v.undo_delete(cx));
        });
        assert!(
            wait_for(visual, &|cx| view.read(cx).entries.len() == 1),
            "undo should restore deleted history row"
        );

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn environments_globals_cookies_certs_lifecycle(cx: &mut TestAppContext) {
        use crate::views::environments::EnvironmentsView;
        use crate::views::environments::state::{
            EditableVarRow, EnvDetailTab, EnvModal, EnvResourceTab,
        };

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());
        workspace_create_impl(rt.state(), "ws-env-ui".into(), "Env WS".into())
            .expect("create workspace");

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        let (view, visual) = cx.add_window_view(|window, cx| {
            EnvironmentsView::new(Some("ws-env-ui".into()), window, cx)
        });
        visual.run_until_parked();

        let wait_for = |visual: &mut gpui_kit::VisualTestContext,
                        pred: &dyn Fn(&gpui_kit::App) -> bool|
         -> bool {
            for _ in 0..80 {
                visual.run_until_parked();
                if visual.update(|window, cx| {
                    window.refresh();
                    pred(cx)
                }) {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            false
        };

        // 1. Create two environments; add a plain + secret variable and production guard; verify secret stays redacted.
        visual.update(|_w, cx| {
            view.update(cx, |v, cx| v.create_env(cx));
        });
        assert!(
            wait_for(visual, &|cx| view.read(cx).environments.len() == 1),
            "first environment should be created"
        );
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.env_draft_vars.push(EditableVarRow {
                    id: None,
                    key: "baseUrl".into(),
                    value: Some("https://api.example.com".into()),
                    is_secret: false,
                    has_secret: false,
                    secret_hint: None,
                    enabled: true,
                    note: "primary".into(),
                });
                v.env_draft_vars.push(EditableVarRow {
                    id: None,
                    key: "apiSecret".into(),
                    value: Some("super-secret-value-9999".into()),
                    is_secret: true,
                    has_secret: true,
                    secret_hint: None,
                    enabled: true,
                    note: String::new(),
                });
                v.env_draft_confirm = true;
                v.env_draft_hosts.push("api.example.com".into());
                v.save_env(cx);
            });
            window.refresh();
        });
        assert!(
            wait_for(visual, &|cx| {
                let v = view.read(cx);
                !v.env_dirty
                    && v.environments.first().is_some_and(|e| {
                        e.confirm_before_send
                            && e.variables.iter().any(|var| {
                                var.key == "apiSecret"
                                    && var.is_secret
                                    && var.has_secret
                                    && var.value.is_none()
                            })
                    })
            }),
            "saved secret variable must persist with has_secret=true and value=None (never echoed)"
        );

        // 2. Create second environment, cycle all 4 detail tabs & 4 resource tabs.
        visual.update(|_w, cx| {
            view.update(cx, |v, cx| v.create_env(cx));
        });
        assert!(
            wait_for(visual, &|cx| view.read(cx).environments.len() == 2),
            "second environment should be created"
        );
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                for dt in EnvDetailTab::ALL {
                    v.detail_tab = dt;
                    cx.notify();
                }
            });
            window.refresh();
        });

        // 3. Globals save + Cookies add/delete/undo + Certificates modal render.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.resource_tab = EnvResourceTab::Globals;
                v.globals_draft.push(EditableVarRow {
                    id: None,
                    key: "org".into(),
                    value: Some("spanbay".into()),
                    is_secret: false,
                    has_secret: false,
                    secret_hint: None,
                    enabled: true,
                    note: String::new(),
                });
                v.save_globals(cx);
            });
            window.refresh();
        });
        assert!(
            wait_for(visual, &|cx| {
                !view.read(cx).globals_dirty && view.read(cx).globals_draft.len() == 1
            }),
            "global variable should persist"
        );

        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.resource_tab = EnvResourceTab::Cookies;
                v.ck_domain_input
                    .update(cx, |s, cx| s.set_value("api.example.com", window, cx));
                v.ck_name_input
                    .update(cx, |s, cx| s.set_value("sid", window, cx));
                v.ck_value_input
                    .update(cx, |s, cx| s.set_value("abc123", window, cx));
                v.add_cookie_from_modal(true, true, window, cx);
            });
            window.refresh();
        });
        assert!(
            wait_for(visual, &|cx| view.read(cx).cookies.len() == 1),
            "added cookie should appear in workspace cookie jar"
        );

        // 4. Delete environment and restore it via 60s Undo.
        visual.update(|window, cx| {
            let first = view.read(cx).environments[0].clone();
            view.update(cx, |v, cx| {
                v.resource_tab = EnvResourceTab::Certificates;
                v.active_modal = Some(EnvModal::ImportEnv);
                assert!(v.dismiss_modal_if_open(cx));
                v.resource_tab = EnvResourceTab::Environments;
                v.delete_env(first.id, first.name, cx);
            });
            window.refresh();
        });
        assert!(
            wait_for(visual, &|cx| {
                view.read(cx).environments.len() == 1 && view.read(cx).env_undo.is_some()
            }),
            "deleting environment should arm undo token"
        );
        visual.update(|_w, cx| {
            view.update(cx, |v, cx| v.undo_delete_env(cx));
        });
        assert!(
            wait_for(visual, &|cx| view.read(cx).environments.len() == 2),
            "undo should restore deleted environment"
        );

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn usage_workbench_presets_dims_and_calls_filter(cx: &mut TestAppContext) {
        use crate::views::usage::state::{
            BreakdownDim, CallsQuickFilter, ChartMetric, RangePreset, UsageModal, UsageViewTab,
        };
        use crate::views::usage::{UsageHandoff, UsageView};

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        let (view, visual) = cx.add_window_view(UsageView::new);
        visual.run_until_parked();

        // 1. Populate multi-day series data so BarChart paints real bars, axes, and tooltips across
        //    all 5 metrics, then cycle 9 time-range presets and 6 breakdown dimensions.
        visual.update(|window, cx| {
            use spanbay_desktop::core::llm::usage::{UsageDay, UsageRow, UsageSummary, UsageToday};
            use std::collections::BTreeMap;

            let mut cost = BTreeMap::new();
            cost.insert("USD".to_owned(), 32_500_i64);
            let active_row = UsageRow {
                requests: 6,
                ok_requests: 5,
                failed: 1,
                canceled: 0,
                input_tokens: 43_900,
                output_tokens: 777,
                cached_tokens: 36_100,
                cost_micro: cost,
                ..UsageRow::default()
            };
            let days = [
                ("2026-11-18", UsageRow::default()),
                ("2026-11-19", UsageRow::default()),
                ("2026-11-20", UsageRow::default()),
                ("2026-11-21", UsageRow::default()),
                ("2026-11-22", UsageRow::default()),
                ("2026-11-23", UsageRow::default()),
                ("2026-11-24", active_row.clone()),
                ("2026-11-25", UsageRow::default()),
            ];
            let summary = UsageSummary {
                totals: active_row,
                currencies: vec!["USD".to_owned()],
                models_without_price: Vec::new(),
                series: days
                    .into_iter()
                    .map(|(d, u)| UsageDay {
                        day: d.to_owned(),
                        usage: u,
                    })
                    .collect(),
                by_model: Vec::new(),
                groups: Vec::new(),
                top_failure: None,
                today: UsageToday {
                    day: "2026-11-25".to_owned(),
                    usage: UsageRow::default(),
                    top_failure: None,
                },
            };
            view.update(cx, |v, cx| {
                v.summary = Some(summary);
                v.latency_by_day
                    .insert("2026-11-24".to_owned(), (25_800, 6));
                cx.notify();
            });
            for metric in ChartMetric::ALL {
                view.update(cx, |v, cx| {
                    v.chart_metric = metric;
                    cx.notify();
                });
                window.refresh();
            }
            for preset in RangePreset::ALL {
                view.update(cx, |v, cx| {
                    v.range = preset;
                    cx.notify();
                });
                window.refresh();
            }
            for dim in BreakdownDim::ALL {
                view.update(cx, |v, cx| {
                    v.breakdown_dim = dim;
                    cx.notify();
                });
                window.refresh();
            }
        });
        visual.run_until_parked();

        // 2. Switch to Calls tab, cycle quick filters & live search, open/dismiss ClearLogs modal, emit handoff.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.active_tab = UsageViewTab::Calls;
                for qf in CallsQuickFilter::ALL {
                    v.calls_quick = qf;
                    cx.notify();
                }
                v.calls_search_input.update(cx, |i, cx| {
                    i.set_value("is:failed model:deepseek", window, cx);
                });
                v.active_modal = Some(UsageModal::ClearLogs { count: 3 });
                cx.notify();
            });
            window.refresh();
            assert!(view.update(cx, |v, cx| v.dismiss_modal_if_open(cx)));
            assert!(view.read(cx).active_modal.is_none());

            view.update(cx, |v, cx| {
                v.pending_handoff
                    .set(Some(UsageHandoff::OpenSettings(SettingsSection::Usage)));
                cx.notify();
            });
            assert!(matches!(
                view.read(cx).take_pending_handoff(),
                Some(UsageHandoff::OpenSettings(SettingsSection::Usage))
            ));
        });

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn chat_workbench_modes_gates_and_modals(cx: &mut TestAppContext) {
        use crate::views::chat::{ChatHandoff, ChatModal, ChatSidebarMode, ChatView};

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        let (view, visual) = cx.add_window_view(ChatView::new);
        visual.run_until_parked();

        visual.update(|window, cx| {
            for mode in [ChatSidebarMode::Time, ChatSidebarMode::Trash] {
                view.update(cx, |v, cx| {
                    v.sidebar_mode = mode;
                    cx.notify();
                });
                window.refresh();
            }

            view.update(cx, |v, cx| {
                v.sidebar_mode = ChatSidebarMode::Time;
                v.plan_mode = true;
                cx.notify();
            });
            window.refresh();

            // Exercise FullAccessConfirm and ChatInstructions modals + Esc.
            for modal in [
                ChatModal::FullAccessConfirm {
                    acknowledged: false,
                },
                ChatModal::ChatInstructions,
            ] {
                view.update(cx, |v, cx| {
                    v.active_modal = Some(modal);
                    cx.notify();
                });
                window.refresh();
                assert!(view.update(cx, |v, cx| v.dismiss_modal_if_open(cx)));
                assert!(view.read(cx).active_modal.is_none());
            }

            // Exercise streamlined header Workflow handoff (replaces legacy right-hand details drawer).
            view.update(cx, |v, cx| {
                v.thread.session_id = "s-wf-1".into();
                v.open_workflow_modal(Some("r-wf-1".into()), None, cx);
            });
            assert!(matches!(
                view.read(cx).take_pending_handoff(),
                Some(ChatHandoff::OpenWorkflowModal { .. })
            ));

            // Exercise 2-row header, simplified 6-item metrics footer, and stick-to-bottom toggle.
            view.update(cx, |v, cx| {
                use spanbay_desktop::core::agent::model::{AgentMessage, AgentRole};
                use spanbay_desktop::core::model::hub::{HubPart, HubUsage};
                v.thread.title = "修复网关 429 熔断与退避".into();
                v.thread.context_limit_tokens = Some(128_000);
                let mut msg = AgentMessage {
                    id: "m-assist-stats".into(),
                    session_id: "s-1".into(),
                    run_id: Some("r-wf-1".into()),
                    seq: 2,
                    role: AgentRole::Assistant,
                    parts: vec![HubPart::text("已拆分 429 退避策略。")],
                    attachments: Vec::new(),
                    model: Some("deepseek/deepseek-v4-pro".into()),
                    usage: Some(HubUsage {
                        input_tokens: Some(18_400),
                        output_tokens: Some(1_120),
                        cached_input_tokens: Some(12_000),
                        cache_write_tokens: None,
                        reasoning_tokens: Some(420),
                        ..HubUsage::default()
                    }),
                    parent_id: None,
                    branch_index: 0,
                    partial: false,
                    cost_micro_usd: 18_500,
                    cost_currency: Some("USD".into()),
                    meta: Default::default(),
                    branch_siblings: Vec::new(),
                    error: None,
                    created_at_ms: 1000,
                };
                msg.meta.ttft_ms = Some(620);
                msg.meta.stream_ms = Some(3_680);
                v.thread.messages.push(msg);
                v.turn_inputs_version += 1;
                cx.notify();
            });
            view.update(cx, |v, cx| {
                v.pending_handoff.set(Some(ChatHandoff::OpenProviders));
                cx.notify();
            });
            assert!(matches!(
                view.read(cx).take_pending_handoff(),
                Some(ChatHandoff::OpenProviders)
            ));
        });

        // The thread is a virtual list: it tails new output, leaving the tail keeps the
        // viewport, and Jump to latest follows again.
        visual.run_until_parked();
        visual.update(|_, cx| {
            view.update(cx, |v, cx| {
                assert!(v.list_state.item_count() >= 1);
                assert!(v.list_state.is_following_tail());
                v.list_state.set_follow_mode(gpui_kit::FollowMode::Normal);
                cx.notify();
            });
        });
        visual.run_until_parked();
        assert!(!visual.read(|cx| view.read(cx).list_state.is_following_tail()));
        visual.update(|_, cx| view.update(cx, |v, cx| v.jump_to_latest(cx)));
        visual.run_until_parked();
        assert!(visual.read(|cx| view.read(cx).list_state.is_following_tail()));

        rt.shutdown_blocking();
    }

    /// B2 chat surfaces: structured approval card, cancelled run marker (no error banner), todo
    /// panel with a failed task, search hits, and Esc on the focused approval card denying it.
    #[gpui_kit::test]
    fn chat_approval_preview_cancel_marker_todos_and_search(cx: &mut TestAppContext) {
        use crate::views::chat::ChatView;
        use spanbay_desktop::core::agent::model::permissions::OperatorHit;
        use spanbay_desktop::core::agent::model::{
            AgentMessage, AgentRole, AgentRunKind, AgentRunStatus, AgentRunSummary, AgentTodoItem,
            AgentTodoStatus, ApprovalCard, ApprovalScope, LiveApprovalRequest, RiskLevel, RiskTag,
            SandboxKind, SandboxState, SandboxStatus, SessionSearchHit, ToolApprovalPreview,
            ToolClass,
        };
        use spanbay_desktop::core::model::hub::{HubPart, HubUsage};
        use spanbay_desktop::state::LiveApproval;

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });
        let (view, visual) = cx.add_window_view(ChatView::new);
        visual.run_until_parked();

        let cmd = "echo 你好\n\nrm -rf build && echo 完成\n";
        let request = LiveApprovalRequest {
            arguments: serde_json::json!({ "command": cmd }).to_string(),
            class: ToolClass::Exec,
            read_only: false,
            destructive: true,
            sandbox: SandboxKind::None,
            reason: "deletes files".to_owned(),
            preview: Some(ToolApprovalPreview::Command {
                cmd: cmd.to_owned(),
                cwd: "/tmp/work".to_owned(),
            }),
            network: true,
            risk: RiskLevel::High,
            summary: "Run rm -rf build".to_owned(),
            full_text: None,
            card: Some(Box::new(ApprovalCard {
                risk: RiskLevel::High,
                risk_reason: Some("deletes files".to_owned()),
                tags: vec![RiskTag::ChainedCommand, RiskTag::NoSandbox],
                operators: vec![
                    OperatorHit {
                        op: "\\n".to_owned(),
                        start: 7,
                    },
                    OperatorHit {
                        op: "\\n".to_owned(),
                        start: 8,
                    },
                    OperatorHit {
                        op: "&&".to_owned(),
                        start: 22,
                    },
                    OperatorHit {
                        op: "\\n".to_owned(),
                        start: 32,
                    },
                ],
                why: None,
                rule_not_used: Some("cargo test *".to_owned()),
                sandbox: SandboxStatus {
                    state: SandboxState::Missing,
                    tier: SandboxKind::None,
                },
                summary: "Run rm -rf build".to_owned(),
                offered_scopes: vec![ApprovalScope::Once, ApprovalScope::Session],
                rule_label: None,
                scope_disabled_reason: None,
                layer: Some("forced_ask".to_owned()),
                why_code: Some("no_sandbox".to_owned()),
            })),
            requested_at_ms: 1_000,
        };
        let run = AgentRunSummary {
            run_id: "r-cancel".to_owned(),
            session_id: "s-b2".to_owned(),
            kind: AgentRunKind::Main,
            parent_run_id: None,
            parent_call_id: None,
            status: AgentRunStatus::Cancelled,
            model_label: "deepseek/reasoner".to_owned(),
            target: None,
            iterations: 2,
            usage: HubUsage::default(),
            cost_micro_usd: 0,
            started_at_ms: 1_000,
            finished_at_ms: Some(2_000),
            last_event_ms: Some(2_000),
            last_step: None,
            stop_reason: None,
            error_kind: None,
            error: None,
            last_seq: 0,
            role: None,
            status_detail: None,
            cost_currency: None,
            failure: None,
            cap_inapplicable: None,
        };

        visual.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.thread.session_id = "s-b2".to_owned();
                v.thread.messages = vec![AgentMessage {
                    id: "m-u1".to_owned(),
                    session_id: "s-b2".to_owned(),
                    run_id: Some("r-cancel".to_owned()),
                    seq: 1,
                    role: AgentRole::User,
                    parts: vec![HubPart::text("clean the build folder")],
                    attachments: Vec::new(),
                    model: None,
                    usage: None,
                    parent_id: None,
                    branch_index: 0,
                    partial: false,
                    cost_micro_usd: 0,
                    cost_currency: None,
                    meta: Default::default(),
                    branch_siblings: Vec::new(),
                    error: None,
                    created_at_ms: 1_000,
                }];
                v.thread.active_run = Some(run.clone());
                v.thread.pending_approvals = vec![LiveApproval::new(
                    "r-live".to_owned(),
                    "call-rm".to_owned(),
                    "shell".to_owned(),
                    request.clone(),
                )];
                v.turn_inputs_version += 1;
                v.thread.todos = vec![
                    AgentTodoItem {
                        id: "t1".to_owned(),
                        content: "Find build outputs".to_owned(),
                        status: AgentTodoStatus::Completed,
                        ..AgentTodoItem::default()
                    },
                    AgentTodoItem {
                        id: "t2".to_owned(),
                        content: "Delete them".to_owned(),
                        status: AgentTodoStatus::Failed,
                        error: Some("permission denied".to_owned()),
                        ..AgentTodoItem::default()
                    },
                ];
                v.todos_open = true;
                v.search_hits = vec![SessionSearchHit {
                    session_id: "s-b2".to_owned(),
                    message_id: "m-u1".to_owned(),
                    title: "Build cleanup".to_owned(),
                    snippet: "clean the [build] folder".to_owned(),
                    ts_ms: 1_000,
                    updated_at_ms: 1_000,
                    in_tool_output: false,
                }];
                cx.notify();
            });
        });
        visual.run_until_parked();

        // A cancelled run ends with a marker row and never with an error banner: the user
        // message plus the marker.
        visual.read(|cx| {
            let v = view.read(cx);
            assert!(v.thread.last_error.is_none());
            assert!(v.list_state.item_count() >= 2);
        });

        // Esc on the focused first approval card denies it without a reason and hands focus back
        // to the composer; the card stays until the core reports the resolution.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                window.focus(&v.approval_focus, cx);
            });
        });
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        visual.update(|window, cx| {
            assert!(!view.read(cx).approval_focus.is_focused(window));
        });

        rt.shutdown_blocking();
    }

    /// Prototype interaction contracts: scope modal radio + save, approval result line,
    /// viewer actual-size toggle, Esc on plan card = keep planning, digit keys on question gate.
    #[gpui_kit::test]
    fn chat_prototype_interactions_scope_viewer_and_gate_keys(cx: &mut TestAppContext) {
        use crate::views::chat::{ChatModal, ChatView, ImageViewerSource};
        use gpui_kit::Focusable as _;
        use spanbay_desktop::core::agent::approvals::RuleContext;
        use spanbay_desktop::core::agent::attention::PendingApprovalSummary;
        use spanbay_desktop::core::agent::model::{
            AgentSessionInput, ApprovalCard, ApprovalDecision, ApprovalScope, HumanGateBody,
            HumanGateDto, HumanGateKind, LiveApprovalRequest, QuestionOption, RiskLevel,
            SandboxKind, SandboxState, SandboxStatus, ToolClass,
        };
        use spanbay_desktop::core::app::agent_session_create_impl;
        use spanbay_desktop::state::LiveApproval;
        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });
        let (view, visual) = cx.add_window_view(ChatView::new);
        visual.run_until_parked();

        // 1. Scope modal: radio ignores unoffered scope, selects offered scope, confirm records
        //    the approval result banner and closes the modal.
        let appr = LiveApproval::new(
            "r-scope".to_owned(),
            "call-scope".to_owned(),
            "shell".to_owned(),
            LiveApprovalRequest {
                arguments: r#"{"command":"cargo test"}"#.to_owned(),
                class: ToolClass::Exec,
                read_only: false,
                destructive: false,
                sandbox: SandboxKind::Seatbelt,
                reason: "mode".to_owned(),
                preview: None,
                network: false,
                risk: RiskLevel::Medium,
                summary: "Run cargo test".to_owned(),
                full_text: None,
                card: Some(Box::new(ApprovalCard {
                    risk: RiskLevel::Medium,
                    risk_reason: None,
                    tags: Vec::new(),
                    operators: Vec::new(),
                    why: None,
                    rule_not_used: None,
                    sandbox: SandboxStatus {
                        state: SandboxState::Available,
                        tier: SandboxKind::Seatbelt,
                    },
                    summary: "Run cargo test".to_owned(),
                    offered_scopes: vec![ApprovalScope::Once, ApprovalScope::Session],
                    rule_label: Some("cargo test *".to_owned()),
                    scope_disabled_reason: Some("folder rule unavailable".to_owned()),
                    layer: Some("mode_default".to_owned()),
                    why_code: None,
                })),
                requested_at_ms: 1_000,
            },
        );
        // 1a. P1-10a: When ScopeApproval modal is open and focus is still on the approval card,
        //     Esc dismisses the modal without denying the approval.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.thread.pending_approvals = vec![appr.clone()];
                v.active_modal = Some(ChatModal::ScopeApproval {
                    appr: Box::new(appr.clone()),
                    selected: ApprovalScope::Once,
                });
                window.focus(&v.approval_focus, cx);
                cx.notify();
            });
            window.refresh();
        });
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        visual.read(|cx| {
            let v = view.read(cx);
            assert!(
                v.active_modal.is_none(),
                "Esc with modal open must close the modal"
            );
            assert!(
                v.last_approval_notice.is_none(),
                "Esc with modal open must not deny the underlying approval card"
            );
            assert_eq!(v.thread.pending_approvals.len(), 1);
        });

        // Register a live approval ticket in the core so `agent_approval_answer_impl` returns
        // `Ok(true)` (P1-10: `last_approval_notice` is only recorded on an accepted answer).
        let scope_session =
            agent_session_create_impl(rt.state(), AgentSessionInput::default()).expect("session");
        let _scope_ticket = rt
            .state()
            .agent
            .approvals
            .register(
                PendingApprovalSummary {
                    run_id: "r-scope".to_owned(),
                    session_id: scope_session.id.clone(),
                    call_id: "call-scope".to_owned(),
                    tool: "shell".to_owned(),
                    reason: "mode".to_owned(),
                    preview: None,
                    requested_ms: 1_000,
                    risk: "medium".to_owned(),
                    summary: "Run cargo test".to_owned(),
                    full_text: None,
                    card: appr.request.card.as_deref().cloned(),
                },
                RuleContext {
                    session_id: scope_session.id.clone(),
                    workspace_dir: None,
                    tool: "shell".to_owned(),
                    pattern: Some("cargo test *".to_owned()),
                    offered: vec![ApprovalScope::Once, ApprovalScope::Session],
                },
            )
            .expect("approval ticket");
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.thread.session_id = scope_session.id.clone();
                v.active_modal = Some(ChatModal::ScopeApproval {
                    appr: Box::new(appr.clone()),
                    selected: ApprovalScope::Once,
                });
                cx.notify();
            });
            window.refresh();
            view.update(cx, |v, cx| {
                // Unoffered scope must stay rejected.
                v.select_scope_approval(ApprovalScope::WorkspacePattern, cx);
                assert!(matches!(
                    v.active_modal,
                    Some(ChatModal::ScopeApproval {
                        selected: ApprovalScope::Once,
                        ..
                    })
                ));
                // Offered scope updates selection; Save & allow submits and leaves result line.
                v.select_scope_approval(ApprovalScope::Session, cx);
                assert!(matches!(
                    v.active_modal,
                    Some(ChatModal::ScopeApproval {
                        selected: ApprovalScope::Session,
                        ..
                    })
                ));
                v.confirm_scope_approval(cx);
            });
            window.refresh();
        });
        for _ in 0..50 {
            visual.run_until_parked();
            if visual.read(|cx| view.read(cx).last_approval_notice.is_some()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        visual.read(|cx| {
            let v = view.read(cx);
            assert!(v.active_modal.is_none());
            let notice = v
                .last_approval_notice
                .as_ref()
                .expect("approval result banner should be recorded");
            assert_eq!(notice.call_id, "call-scope");
            assert_eq!(notice.decision, ApprovalDecision::Allow);
            assert_eq!(notice.scope, ApprovalScope::Session);
        });
        // 2. Image viewer actual-size toggle renders cleanly in both Fit and Actual modes.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.active_modal = Some(ChatModal::ImageViewer {
                    key: "img-test".to_owned(),
                    title: "shot.png".to_owned(),
                    meta: "1440×900 · 179 KB".to_owned(),
                    dims: Some((1440, 900)),
                    actual: false,
                    source: Some(ImageViewerSource::Attachment("att-1".to_owned())),
                });
                cx.notify();
            });
            window.refresh();
            view.update(cx, |v, cx| v.set_image_viewer_actual(true, cx));
            assert!(matches!(
                view.read(cx).active_modal,
                Some(ChatModal::ImageViewer { actual: true, .. })
            ));
            window.refresh();
            view.update(cx, |v, cx| v.set_image_viewer_actual(false, cx));
            assert!(matches!(
                view.read(cx).active_modal,
                Some(ChatModal::ImageViewer { actual: false, .. })
            ));
            view.update(cx, |v, cx| {
                v.active_modal = None;
                cx.notify();
            });
        });

        // 3. Esc on focused plan gate triggers Keep planning (sets plan_revising + focuses composer).
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.thread.open_gates = vec![(
                    "r-plan".to_owned(),
                    HumanGateDto {
                        id: "g-plan-1".to_owned(),
                        run_id: "r-plan".to_owned(),
                        call_id: "c-plan-1".to_owned(),
                        kind: HumanGateKind::Plan,
                        created_at_ms: 1_000,
                        body: HumanGateBody::Plan {
                            markdown: "## Plan\n1. Fix router".to_owned(),
                            title: Some("Fix 429 backoff".to_owned()),
                            steps: 1,
                            tasks: Vec::new(),
                        },
                    },
                )];
                cx.notify();
            });
            window.refresh();
            view.update(cx, |v, cx| window.focus(&v.gate_focus, cx));
        });
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        visual.update(|window, cx| {
            let v = view.read(cx);
            assert!(v.plan_revising, "Esc on plan card must enter Keep planning");
            assert!(!v.gate_focus.is_focused(window));
        });

        // 4. Number keys on focused question gate toggle options (1..=N; out-of-range ignored).
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.plan_revising = false;
                v.thread.open_gates = vec![(
                    "r-q".to_owned(),
                    HumanGateDto {
                        id: "g-q-1".to_owned(),
                        run_id: "r-q".to_owned(),
                        call_id: "c-q-1".to_owned(),
                        kind: HumanGateKind::Question,
                        created_at_ms: 1_000,
                        body: HumanGateBody::Question {
                            question: "Switch to fallback?".to_owned(),
                            options: vec![
                                QuestionOption {
                                    label: "Switch".to_owned(),
                                    detail: None,
                                },
                                QuestionOption {
                                    label: "Retry only".to_owned(),
                                    detail: None,
                                },
                            ],
                            multi: false,
                        },
                    },
                )];
                cx.notify();
            });
            window.refresh();
            view.update(cx, |v, cx| window.focus(&v.gate_focus, cx));
        });
        visual.simulate_keystrokes("2");
        visual.run_until_parked();
        visual.read(|cx| {
            assert_eq!(
                view.read(cx).question_draft_choices.get("c-q-1"),
                Some(&vec![2])
            );
        });
        visual.simulate_keystrokes("9");
        visual.run_until_parked();
        visual.read(|cx| {
            assert_eq!(
                view.read(cx).question_draft_choices.get("c-q-1"),
                Some(&vec![2]),
                "out-of-range digit must not alter choices"
            );
        });
        // Pressing the selected single-choice digit again deselects it; pressing it again reselects.
        visual.simulate_keystrokes("2");
        visual.run_until_parked();
        visual.read(|cx| {
            assert_eq!(
                view.read(cx).question_draft_choices.get("c-q-1"),
                Some(&Vec::<u8>::new()),
                "clicking/pressing active single-choice option must deselect it"
            );
        });
        visual.simulate_keystrokes("2");
        visual.run_until_parked();
        // 4b. Esc inside the question's "Other" input leaves the field and focuses `gate_focus`
        //     without skipping the question; a second Esc on the card skips the question.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.question_other_input
                    .update(cx, |inp, cx| inp.focus(window, cx));
                assert!(
                    v.question_other_input
                        .read(cx)
                        .focus_handle(cx)
                        .is_focused(window)
                );
            });
        });
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        visual.update(|window, cx| {
            let v = view.read(cx);
            assert!(
                !v.question_other_input
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window),
                "Esc inside question other input must leave the field"
            );
            assert!(
                v.gate_focus.is_focused(window),
                "Esc inside question other input must return focus to the gate card"
            );
            assert_eq!(
                v.thread.open_gates.len(),
                1,
                "Esc inside question other input must not skip the question"
            );
        });

        // 5. Search scroll (3a): a hit pointing at a Tool message maps to the nearest preceding
        //    rendered message, and a missing target id still clears `pending_scroll_message_id`.
        visual.update(|window, cx| {
            use spanbay_desktop::core::agent::model::{AgentMessage, AgentRole};
            use spanbay_desktop::core::model::hub::HubPart;
            view.update(cx, |v, cx| {
                v.thread.open_gates.clear();
                v.thread.messages = vec![
                    AgentMessage {
                        id: "m-u-hit".to_owned(),
                        session_id: "s-hit".to_owned(),
                        run_id: None,
                        seq: 1,
                        role: AgentRole::User,
                        parts: vec![HubPart::text("check logs")],
                        attachments: Vec::new(),
                        model: None,
                        usage: None,
                        parent_id: None,
                        branch_index: 0,
                        partial: false,
                        cost_micro_usd: 0,
                        cost_currency: None,
                        meta: Default::default(),
                        branch_siblings: Vec::new(),
                        error: None,
                        created_at_ms: 1,
                    },
                    AgentMessage {
                        id: "m-a-hit".to_owned(),
                        session_id: "s-hit".to_owned(),
                        run_id: None,
                        seq: 2,
                        role: AgentRole::Assistant,
                        parts: vec![HubPart::ToolCall {
                            id: "call-1".to_owned(),
                            name: "grep".to_owned(),
                            arguments: r#"{"pattern":"429"}"#.to_owned(),
                            signature: None,
                            origin: None,
                        }],
                        attachments: Vec::new(),
                        model: None,
                        usage: None,
                        parent_id: None,
                        branch_index: 0,
                        partial: false,
                        cost_micro_usd: 0,
                        cost_currency: None,
                        meta: Default::default(),
                        branch_siblings: Vec::new(),
                        error: None,
                        created_at_ms: 2,
                    },
                    AgentMessage {
                        id: "m-tool-hit".to_owned(),
                        session_id: "s-hit".to_owned(),
                        run_id: None,
                        seq: 3,
                        role: AgentRole::Tool,
                        parts: vec![HubPart::ToolResult {
                            call_id: "call-1".to_owned(),
                            name: Some("grep".to_owned()),
                            content: vec![HubPart::text("retry-after: 2")],
                            is_error: false,
                        }],
                        attachments: Vec::new(),
                        model: None,
                        usage: None,
                        parent_id: None,
                        branch_index: 0,
                        partial: false,
                        cost_micro_usd: 0,
                        cost_currency: None,
                        meta: Default::default(),
                        branch_siblings: Vec::new(),
                        error: None,
                        created_at_ms: 3,
                    },
                ];
                // Pad with enough later turns so `m-tool-hit` sits above the viewport bottom
                // and jumping to it genuinely pauses tail-following.
                for seq in 4..=30 {
                    v.thread.messages.push(AgentMessage {
                        id: format!("m-pad-{seq}"),
                        session_id: "s-hit".to_owned(),
                        run_id: None,
                        seq,
                        role: AgentRole::User,
                        parts: vec![HubPart::text(format!("follow-up message {seq}"))],
                        attachments: Vec::new(),
                        model: None,
                        usage: None,
                        parent_id: None,
                        branch_index: 0,
                        partial: false,
                        cost_micro_usd: 0,
                        cost_currency: None,
                        meta: Default::default(),
                        branch_siblings: Vec::new(),
                        error: None,
                        created_at_ms: seq,
                    });
                }
                v.turn_inputs_version += 1;
                v.list_state.set_follow_mode(gpui_kit::FollowMode::Tail);
                v.pending_scroll_message_id = Some("m-tool-hit".to_owned());
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear(cx);
            assert!(view.read(cx).pending_scroll_message_id.is_none());
            assert!(
                !view.read(cx).list_state.is_following_tail(),
                "scrolling to a tool-message hit must switch out of tail-follow and reveal its parent assistant row"
            );

            // Missing target id must still clear `pending_scroll_message_id`.
            view.update(cx, |v, cx| {
                v.pending_scroll_message_id = Some("m-nonexistent".to_owned());
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear(cx);
            assert!(view.read(cx).pending_scroll_message_id.is_none());
        });

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn providers_gateway_bench_and_handoff(cx: &mut TestAppContext) {
        use crate::views::gateway::{GatewayHandoff, GatewayTab, GatewayView};
        use crate::views::providers::{LeftSelection, ProvidersHandoff, ProvidersView};

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Ocean, true, false, &settings.appearance, cx);
        });

        // A. Dedicated Local Gateway cockpit (5 tabs incl. Traffic, centered connect modal, handoffs).
        let (gw_view, gw_visual) = cx.add_window_view(GatewayView::new);
        gw_visual.run_until_parked();

        gw_visual.update(|window, cx| {
            for tab in [
                GatewayTab::Overview,
                GatewayTab::Clients,
                GatewayTab::Rules,
                GatewayTab::Traffic,
                GatewayTab::Access,
            ] {
                gw_view.update(cx, |v, cx| v.set_tab(tab, cx));
                window.refresh();
            }

            // Centered Connect Coding Tool modal opens and dismisses cleanly via Escape.
            gw_view.update(cx, |v, cx| v.open_connect_sheet(None, window, cx));
            assert!(gw_view.read(cx).connect_modal.is_some());
            window.refresh();
            assert!(gw_view.update(cx, |v, cx| v.dismiss_modal_if_open(cx)));
            assert!(gw_view.read(cx).connect_modal.is_none());

            gw_view.update(cx, |v, cx| {
                v.confirm_lan_open = true;
                cx.notify();
            });
            assert!(gw_view.update(cx, |v, cx| v.dismiss_modal_if_open(cx)));
            assert!(!gw_view.read(cx).confirm_lan_open);

            gw_view.update(cx, |v, cx| {
                v.pending_handoff
                    .set(Some(GatewayHandoff::OpenUsageGateway));
                cx.notify();
            });
            assert!(matches!(
                gw_view.read(cx).take_pending_handoff(),
                Some(GatewayHandoff::OpenUsageGateway)
            ));
        });

        // B. Focused Providers view (empty onboarding hero, Model Test Bench, modal dismissal, handoff).
        let (view, visual) = cx.add_window_view(ProvidersView::new);
        visual.run_until_parked();

        visual.update(|window, cx| {
            assert_eq!(view.read(cx).selection, LeftSelection::Empty);
            window.refresh();

            view.update(cx, |v, cx| {
                v.open_bench(None, Vec::new(), window, cx);
            });
            assert_eq!(view.read(cx).selection, LeftSelection::Bench);
            assert!(view.read(cx).bench_view.is_some());
            window.refresh();

            view.update(cx, |v, cx| {
                v.adding_key_open = true;
                cx.notify();
            });
            assert!(view.update(cx, |v, cx| v.dismiss_modal_if_open(cx)));
            assert!(!view.read(cx).adding_key_open);

            view.update(cx, |v, cx| {
                v.pending_handoff
                    .set(Some(ProvidersHandoff::OpenUsageProvider(None)));
                cx.notify();
            });
            assert!(matches!(
                view.read(cx).take_pending_handoff(),
                Some(ProvidersHandoff::OpenUsageProvider(None))
            ));
        });

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn shell_palette_dynamic_entities_handoffs_and_utility_tabs(cx: &mut TestAppContext) {
        use spanbay_desktop::core::app::factory_service::{flow_new, mock_server_new};
        use spanbay_desktop::core::app::{
            SaveRequestInput, flow_save_impl, mock_server_save_impl, request_save_impl,
        };
        use spanbay_desktop::core::model::ai_request::RequestMode;
        use spanbay_desktop::core::model::{
            AuthConfig, HttpMethod, NetworkOptions, ProtocolKind, RequestBody,
        };
        use spanbay_desktop::state::{EnvironmentResourceTarget, PaletteAction, ToastKind};

        use crate::views::utility::diagnostics::ConsoleMode;
        use crate::views::utility::import_export::IoTab;

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());
        workspace_create_impl(rt.state(), "ws-shell-e2e".into(), "Shell E2E WS".into())
            .expect("create workspace");

        let saved_req = request_save_impl(
            rt.state(),
            SaveRequestInput {
                id: "req-pal-1".into(),
                workspace_id: "ws-shell-e2e".into(),
                collection_id: None,
                folder_id: None,
                name: "Get Invoice".into(),
                protocol: ProtocolKind::HttpRest,
                method: HttpMethod::Get,
                url: "https://api.example.com/invoices".into(),
                query_params: Vec::new(),
                path_variables: Vec::new(),
                headers: Vec::new(),
                auth: AuthConfig::None,
                body: RequestBody::None,
                network_options: NetworkOptions::default(),
                assertions: Vec::new(),
                description: String::new(),
                pre_request_script: String::new(),
                test_script: String::new(),
                position: 0,
                mode: RequestMode::Http,
                ai: None,
                examples: None,
            },
        )
        .expect("seed request");
        let mock_srv = mock_server_save_impl(
            rt.state(),
            mock_server_new(rt.state(), "ws-shell-e2e").expect("new mock"),
        )
        .expect("seed mock");
        let flow = flow_save_impl(
            rt.state(),
            flow_new(rt.state(), "ws-shell-e2e").expect("new flow"),
        )
        .expect("seed flow");

        let (host, rx) = GpuiHostWindow::pair();
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        let mut shell_slot = None;
        let (_root, visual) = cx.add_window_view(|window, cx| {
            use gpui_kit::AppContext as _;
            let s = cx.new(|cx| AppShell::new(rx, host, window, cx));
            shell_slot = Some(s.clone());
            crate::ui::Root::new(s, window, cx)
        });
        let shell = shell_slot.expect("mounted AppShell");
        visual.run_until_parked();

        visual.update(|window, cx| {
            // 1. Verify dynamic Command Palette indexing & keyboard selection navigation.
            shell.update(cx, |s, cx| {
                s.switch_workspace("ws-shell-e2e".into(), cx);
                let dyn_items = s.collect_palette_dynamic_items(cx);
                assert!(
                    dyn_items
                        .requests
                        .iter()
                        .any(|(id, _, _)| id == saved_req.id.as_str())
                );
                assert!(dyn_items.mocks.iter().any(|(id, _, _)| id == &mock_srv.id));
                assert!(dyn_items.flows.iter().any(|(id, _)| id == &flow.id));
                s.toggle_palette(window, cx);
            });
            assert!(shell.read(cx).state.palette_open);
            let pal = shell.read(cx).palette_view.clone();
            pal.update(cx, |p, cx| {
                p.move_selection(1, cx);
                p.move_selection(-1, cx);
                p.trigger_selected(cx);
            });
        });
        // Flush observer notification -> AppShell::render() to consume palette action & close overlay.
        visual.run_until_parked();
        visual.update(|window, cx| {
            assert!(!shell.read(cx).state.palette_open);

            // 2. Execute dynamic entity & wizard palette actions cleanly.
            for (id, action, expected_mod) in [
                (
                    "req:1",
                    PaletteAction::OpenRequestById(saved_req.id.as_str().to_owned()),
                    NavModule::Requests,
                ),
                (
                    "mock:1",
                    PaletteAction::OpenMockServer(mock_srv.id.clone()),
                    NavModule::Mock,
                ),
                (
                    "flow:1",
                    PaletteAction::OpenFlow(flow.id.clone()),
                    NavModule::Flows,
                ),
                ("wiz:gw", PaletteAction::OpenGateway, NavModule::Gateway),
                (
                    "deep:cookies",
                    PaletteAction::OpenEnvironmentResource(EnvironmentResourceTarget::Cookies),
                    NavModule::Environments,
                ),
                (
                    "deep:certificates",
                    PaletteAction::OpenEnvironmentResource(EnvironmentResourceTarget::Certificates),
                    NavModule::Environments,
                ),
                (
                    "deep:runner",
                    PaletteAction::OpenCollectionRunner,
                    NavModule::Requests,
                ),
            ] {
                shell.update(cx, |s, cx| {
                    s.execute_palette_action(id.into(), action, window, cx);
                });
                assert_eq!(shell.read(cx).state.active_module, expected_mod);
                window.refresh();
            }

            // 3. Exercise unified Import/Export modal (3 tabs + cURL draft handoff) & Diagnostics slide-over sheet.
            shell.update(cx, |s, cx| {
                s.navigate(NavModule::Gateway, cx);
                s.open_utility(UtilityDialog::ImportExport, window, cx);
            });
            let io_modal = shell.read(cx).import_export_view.clone();
            for tab in [IoTab::Import, IoTab::SanitizedExport, IoTab::SecretsExport] {
                io_modal.update(cx, |u, cx| {
                    u.io_tab = tab;
                    cx.notify();
                });
                window.refresh();
            }
            io_modal.update(cx, |u, cx| {
                u.io_tab = IoTab::Import;
                u.io_format = "curl".into();
                u.payload_input.update(cx, |t, cx| {
                    t.set_value(
                        "curl -X POST https://httpbin.org/post -H 'X-Test: 1' -d '{\"ok\":true}'",
                        window,
                        cx,
                    );
                });
                u.run_io_import(cx);
            });
        });
        // Flush observer notification -> AppShell::render() to consume cURL draft handoff & close modal.
        visual.run_until_parked();
        visual.update(|window, cx| {
            assert_eq!(shell.read(cx).state.active_module, NavModule::Requests);
            assert!(shell.read(cx).state.active_utility.is_none());

            shell.update(cx, |s, cx| {
                s.open_utility(UtilityDialog::Diagnostics, window, cx);
            });
            let diag = shell.read(cx).diagnostics_view.clone();
            for mode in [ConsoleMode::Diagnostics, ConsoleMode::GpuBench] {
                diag.update(cx, |u, cx| {
                    u.console_mode = mode;
                    cx.notify();
                });
                window.refresh();
            }

            // 4. Push auto-dismissing toast & verify sidebar collapse/expand rendering.
            let toasts_before = shell.read(cx).state.toasts.len();
            assert_eq!(toasts_before, 1, "cURL draft handoff should push one toast");
            shell.update(cx, |s, cx| {
                s.push_toast_auto(ToastKind::Success, "Saved", None, cx);
                s.toggle_sidebar(cx);
            });
            assert_eq!(shell.read(cx).state.toasts.len(), toasts_before + 1);
            window.refresh();
            shell.update(cx, |s, cx| s.toggle_sidebar(cx));
            window.refresh();

            // 5. Regression guard: both normal and danger popover menu items must construct
            //    without triggering GPUI's `hover style already set` panic.
            let _normal = crate::ui::menu_item(
                "reg-menu-normal",
                crate::ui::UiIcon::Pencil,
                "Rename",
                false,
                cx,
                |_, _, _| {},
            );
            let _danger = crate::ui::menu_item(
                "reg-menu-danger",
                crate::ui::UiIcon::Trash,
                "Delete",
                true,
                cx,
                |_, _, _| {},
            );

            // 6. Exercise global outbound proxy mode switching from the unified top titlebar.
            shell.update(cx, |s, cx| {
                s.set_global_proxy_url("direct".to_owned(), cx);
            });
            window.refresh();
            shell.update(cx, |s, cx| {
                s.set_global_proxy_url(String::new(), cx);
            });
            window.refresh();
        });

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn catalog_two_tabs_and_provider_pricing_lifecycle(cx: &mut TestAppContext) {
        use spanbay_desktop::core::app::{llm_model_save_impl, llm_provider_save_impl};
        use spanbay_desktop::core::llm::store::{ModelSource, Provider, ProviderModel};
        use spanbay_desktop::core::llm::wire::{AuthStyle, Protocol, ProviderEndpoint};

        use crate::views::providers::{ProviderTab, ProvidersView};
        use crate::views::settings::catalog::{
            CatalogSettingsView, CatalogTab, UnlistedPriceFilter,
        };

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());

        let cloud = llm_provider_save_impl(
            rt.state(),
            Provider {
                name: "MyRelay".to_owned(),
                category: "custom".to_owned(),
                base_url: "https://relay.example.com".to_owned(),
                auth: AuthStyle::Bearer,
                endpoints: vec![ProviderEndpoint {
                    protocol: Protocol::OpenaiChat,
                    path_prefix: "/v1".to_owned(),
                    auth: None,
                    url_mode: Default::default(),
                    enabled: true,
                }],
                ..Provider::default()
            },
        )
        .expect("seed cloud provider");
        let local = llm_provider_save_impl(
            rt.state(),
            Provider {
                name: "Local Ollama".to_owned(),
                category: "local".to_owned(),
                base_url: "http://127.0.0.1:11434".to_owned(),
                auth: AuthStyle::None,
                endpoints: vec![ProviderEndpoint {
                    protocol: Protocol::OpenaiChat,
                    path_prefix: "/v1".to_owned(),
                    auth: None,
                    url_mode: Default::default(),
                    enabled: true,
                }],
                ..Provider::default()
            },
        )
        .expect("seed local provider");

        for (pid, mid) in [
            (&cloud.id, "gpt-4o"),
            (&cloud.id, "custom/unlisted-alpha"),
            (&cloud.id, "custom/unlisted-beta"),
            (&local.id, "local-free-model"),
        ] {
            llm_model_save_impl(
                rt.state(),
                ProviderModel {
                    provider_id: pid.clone(),
                    model_id: mid.to_owned(),
                    source: ModelSource::Manual,
                    enabled: true,
                    ..ProviderModel::default()
                },
            )
            .expect("seed model");
        }

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        fn wait_for(
            visual: &mut gpui_kit::VisualTestContext,
            pred: impl Fn(&gpui_kit::App) -> bool,
        ) -> bool {
            for _ in 0..200 {
                visual.run_until_parked();
                if visual.update(|window, cx| {
                    window.refresh();
                    pred(cx)
                }) {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            false
        }

        // 1. Settings › Models & Pricing: two-tab Catalog / Unlisted filtering & paging.
        let (cat_view, cat_visual) = cx.add_window_view(CatalogSettingsView::new);
        assert!(
            wait_for(cat_visual, |cx| {
                let v = cat_view.read(cx);
                v.page.is_some() && v.unlisted.len() == 2
            }),
            "catalog page and 2 paid unlisted models (excluding local free) must load"
        );

        cat_visual.update(|window, cx| {
            cat_view.update(cx, |v, cx| {
                v.set_tab(CatalogTab::Unlisted, cx);
                v.unlisted_provider = Some(cloud.id.clone());
                v.unlisted_price_filter = UnlistedPriceFilter::Unpriced;
                v.unlisted_page_size = 50;
                cx.notify();
            });
            window.refresh();
            assert_eq!(cat_view.read(cx).active_tab, CatalogTab::Unlisted);

            cat_view.update(cx, |v, cx| {
                v.unlisted_search_input
                    .update(cx, |i, cx| i.set_value("alpha", window, cx));
                v.unlisted_price_filter = UnlistedPriceFilter::Priced;
                cx.notify();
            });
            window.refresh();
            cat_view.update(cx, |v, cx| v.clear_unlisted_filters(window, cx));
            window.refresh();
            assert!(!cat_view.read(cx).unlisted_has_active_filters(cx));
        });

        // Cross-view live event sync: saving a model pricing override emits CatalogChanged
        // and automatically refreshes an already-mounted CatalogSettingsView.
        {
            use spanbay_desktop::core::app::llm_model_pricing_save_impl;
            use spanbay_desktop::core::model::model_catalog::{ModelPricing, PricingMode};

            let mut direct = ModelPricing::dynamic(&cloud.id, "custom/unlisted-alpha");
            direct.mode = PricingMode::Custom;
            direct.input_per_mtok = Some(1.25);
            direct.output_per_mtok = Some(3.75);
            llm_model_pricing_save_impl(rt.state(), direct).expect("save direct unlisted price");
        }
        assert!(
            wait_for(cat_visual, |cx| {
                cat_view
                    .read(cx)
                    .unlisted
                    .iter()
                    .any(|u| u.model_id == "custom/unlisted-alpha" && u.effective.is_some())
            }),
            "CatalogSettingsView must live-refresh on LlmEvent::CatalogChanged"
        );

        // 2. Providers › Config & Models: catalog pricing basis + relay multiplier persistence.
        let (prov_view, prov_visual) = cx.add_window_view(ProvidersView::new);
        assert!(
            wait_for(prov_visual, |cx| {
                let v = prov_view.read(cx);
                v.providers.len() == 2
                    && !v.catalog_families.is_empty()
                    && v.pricing_of(&cloud.id).is_some()
            }),
            "providers view must load providers, catalog families, and provider pricing"
        );

        let sample_family =
            prov_visual.update(|_window, cx| prov_view.read(cx).catalog_families[0].id.clone());

        prov_visual.update(|window, cx| {
            prov_view.update(cx, |v, cx| {
                v.select_provider(cloud.id.clone(), window, cx);
                v.provider_tab = ProviderTab::Models;
                cx.notify();
            });
            window.refresh();
            prov_view.update(cx, |v, cx| {
                v.provider_tab = ProviderTab::Config;
                cx.notify();
            });
            window.refresh();

            let cfg = prov_view
                .read(cx)
                .config_form
                .clone()
                .expect("config form mounted");
            cfg.update(cx, |f, cx| {
                f.save_catalog_basis(Some(sample_family.clone()), cx);
            });
        });

        assert!(
            wait_for(prov_visual, |cx| {
                prov_view
                    .read(cx)
                    .pricing_of(&cloud.id)
                    .and_then(|pv| pv.catalog_family.as_deref())
                    == Some(sample_family.as_str())
            }),
            "provider catalog pricing basis must persist from Provider › Config"
        );

        prov_visual.update(|window, cx| {
            let cfg = prov_view
                .read(cx)
                .config_form
                .clone()
                .expect("config form mounted");
            cfg.update(cx, |f, cx| {
                f.relay_mult_input
                    .update(cx, |i, cx| i.set_value("0.5", window, cx));
                f.relay_currency = Some("CNY".to_owned());
                f.save_relay_pricing_only(cx);
            });
            window.refresh();
        });

        assert!(
            wait_for(prov_visual, |cx| {
                prov_view
                    .read(cx)
                    .providers
                    .iter()
                    .find(|p| p.provider.id == cloud.id)
                    .and_then(|p| p.provider.quirks.relay_pricing.as_ref())
                    .is_some_and(|rp| {
                        (rp.multiplier - 0.5).abs() < 1e-6 && rp.currency.as_deref() == Some("CNY")
                    })
            }),
            "relay pricing multiplier and currency must persist from Provider › Config"
        );

        // Equivalent float formatting ("0.50" vs saved 0.5) must not report dirty,
        // and currency-only override with empty multiplier must report dirty cleanly.
        prov_visual.update(|window, cx| {
            let cfg = prov_view
                .read(cx)
                .config_form
                .clone()
                .expect("config form mounted");
            cfg.update(cx, |f, cx| {
                f.relay_mult_input
                    .update(cx, |i, cx| i.set_value("0.50", window, cx));
                assert!(
                    !f.relay_dirty(cx),
                    "0.50 equals saved 0.5 and must not leave Apply/Reset stuck visible"
                );
                f.relay_mult_input
                    .update(cx, |i, cx| i.set_value("", window, cx));
                assert!(
                    f.relay_dirty(cx),
                    "empty multiplier with CNY currency (= 1.0 CNY) differs from 0.5 CNY"
                );
            });
        });

        // Cross-view live event sync: saving a per-model override emits CatalogChanged
        // and updates ProvidersView.provider_pricing (which notifies ProviderConfigForm).
        {
            use spanbay_desktop::core::app::llm_model_pricing_save_impl;
            use spanbay_desktop::core::model::model_catalog::{ModelPricing, PricingMode};

            let mut ovr = ModelPricing::dynamic(&cloud.id, "custom/unlisted-beta");
            ovr.mode = PricingMode::Custom;
            ovr.input_per_mtok = Some(2.0);
            ovr.output_per_mtok = Some(6.0);
            llm_model_pricing_save_impl(rt.state(), ovr).expect("save second unlisted override");
        }
        assert!(
            wait_for(prov_visual, |cx| {
                prov_view
                    .read(cx)
                    .pricing_of(&cloud.id)
                    .is_some_and(|pv| pv.overrides >= 2)
            }),
            "ProvidersView and ProviderConfigForm must live-refresh on LlmEvent::CatalogChanged"
        );

        // Local free provider renders the Local · Free pricing card cleanly.
        prov_visual.update(|window, cx| {
            prov_view.update(cx, |v, cx| {
                v.select_provider(local.id.clone(), window, cx);
                v.provider_tab = ProviderTab::Config;
                cx.notify();
            });
            window.refresh();
        });

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn code_editor_fills_response_pane_and_usage_gateway_scope_filters(cx: &mut TestAppContext) {
        use crate::ui::{EditorState, code_editor, v_col};
        use crate::views::usage::{UsageView, state::RangePreset};
        use gpui_kit::{
            AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
            Window, div, px,
        };
        use spanbay_desktop::core::llm::usage::UsageSource;

        struct ResponsePaneHarness {
            editor: Entity<EditorState>,
        }

        impl Render for ResponsePaneHarness {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                v_col()
                    .w(px(800.0))
                    .h(px(400.0))
                    .child(div().h(px(40.0)).flex_shrink_0())
                    .child(
                        v_col()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .p(px(8.0))
                            .child(code_editor(&self.editor).readonly(true)),
                    )
            }
        }

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Ocean, false, false, &settings.appearance, cx);
        });

        // 1. Verify `code_editor` fills its bounded flex-1 container (regression for 2-line collapse).
        let (harness, visual) = cx.add_window_view(|window, cx| {
            let editor = cx.new(|cx| {
                let mut ed = EditorState::new(window, cx)
                    .soft_wrap(true)
                    .default_value("{\n  \"args\": {},\n  \"headers\": {\n    \"Host\": \"httpbin.org\"\n  },\n  \"origin\": \"127.0.0.1\",\n  \"url\": \"https://httpbin.org/get\"\n}");
                ed.set_highlighter("json", cx);
                ed
            });
            ResponsePaneHarness { editor }
        });
        let editor_ent = harness.read_with(visual, |h, _| h.editor.clone());
        let painted_h = visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            editor_ent.read(cx).input_bounds().size.height
        });
        assert!(
            painted_h > px(250.0),
            "code_editor inside 360px response container must fill height (got {painted_h:?})"
        );

        // 2. Verify UsageView::select_gateway_scope pre-filters strictly to Gateway + Today.
        let (usage_view, usage_visual) = cx.add_window_view(UsageView::new);
        usage_visual.run_until_parked();
        usage_visual.update(|window, cx| {
            usage_view.update(cx, |u, cx| u.select_gateway_scope(false, cx));
            window.refresh();
            let u = usage_view.read(cx);
            assert_eq!(u.filter_source, Some(UsageSource::Gateway));
            assert_eq!(u.range, RangePreset::Today);
            assert!(u.scope_provider_id.is_none());
        });

        // 3. Verify unified `PayloadViewer` (auto-detect, Pretty/Raw, Wrap) & `payload_block`.
        {
            use crate::ui::{
                PayloadBlockOptions, PayloadViewer, highlighter_for_format, payload_block,
            };
            use spanbay_desktop::core::response_viewer::ResponseBodyFormat;

            let (viewer, pv_visual) =
                cx.add_window_view(|window, cx| PayloadViewer::new("test-pv", true, window, cx));
            pv_visual.update(|window, cx| {
                // JSON delivered with generic text/plain header auto-promotes to Pretty JSON.
                viewer.update(cx, |v, cx| {
                    v.set_text(
                        r#"{"data":[{"id":"ord_1"}],"ok":true}"#,
                        Some("text/plain"),
                        window,
                        cx,
                    );
                });
                window.refresh();
                let v = viewer.read(cx);
                assert_eq!(v.format(), ResponseBodyFormat::Json);
                assert!(v.is_pretty());
                assert!(v.displayed_text(cx).contains("\n  \"data\""));

                // Toggle Raw restores exact compact string; toggle Wrap flips soft-wrap state.
                viewer.update(cx, |v, cx| {
                    v.set_format(ResponseBodyFormat::Json, false, window, cx);
                    v.toggle_wrap(window, cx);
                });
                let v = viewer.read(cx);
                assert!(!v.is_pretty());
                assert!(!v.is_soft_wrap());
                assert_eq!(
                    v.displayed_text(cx),
                    r#"{"data":[{"id":"ord_1"}],"ok":true}"#
                );

                // Plain text stays Text; XML detects Xml and maps to compiled HTML highlighter.
                viewer.update(cx, |v, cx| {
                    v.set_text("pong", Some("text/plain"), window, cx)
                });
                assert_eq!(viewer.read(cx).format(), ResponseBodyFormat::Text);
                viewer.update(cx, |v, cx| {
                    v.set_text("<?xml version=\"1.0\"?><root><a/></root>", None, window, cx)
                });
                assert_eq!(viewer.read(cx).format(), ResponseBodyFormat::Xml);
                assert_eq!(highlighter_for_format(ResponseBodyFormat::Xml), "html");

                // Compact `payload_block` handles JSON, Diff, and plain text cleanly.
                let _ = payload_block("pb-json", r#"{"a":1}"#, PayloadBlockOptions::default(), cx);
                let _ = payload_block(
                    "pb-diff",
                    "--- a/foo.rs\n+++ b/foo.rs\n@@ -1 +1 @@\n-old\n+new\n",
                    PayloadBlockOptions::default(),
                    cx,
                );
            });
        }

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn run_monitor_adaptive_three_views_and_execution_graph_inspector(cx: &mut TestAppContext) {
        use crate::views::chat::{ChatSidebarMode, ChatView};
        use crate::views::runs::{
            RunsFilter, RunsHandoff, RunsScope, RunsView, WorkflowMonitorModal, WorkflowTab,
        };
        use spanbay_desktop::core::agent::dto::{
            AgentBoard, AgentBoardRow, AgentRunTopology, BoardChanges, BoardLimits,
            BoardSubagentCounts, ExecutionWave, SessionRunHistoryItem, TokensToday,
            TopologySubagent, TopologyToolStep,
        };
        use spanbay_desktop::core::agent::model::{
            AgentRunKind, AgentRunStatus, AgentRunSummary, FileChange, FileChangeKind,
            FileChangeStatus, SubagentRole,
        };
        use spanbay_desktop::core::llm::hub::HubUsage;
        use spanbay_desktop::state::is_graph_available;

        let make_run = |id: &str, kind: AgentRunKind, status: AgentRunStatus| AgentRunSummary {
            run_id: id.to_owned(),
            session_id: "as_test".to_owned(),
            kind,
            parent_run_id: None,
            parent_call_id: None,
            status,
            model_label: "deepseek/reasoner".to_owned(),
            target: None,
            iterations: 3,
            usage: HubUsage {
                input_tokens: Some(1200),
                output_tokens: Some(450),
                ..HubUsage::default()
            },
            cost_micro_usd: 1500,
            started_at_ms: 1_000,
            finished_at_ms: Some(9_000),
            last_event_ms: Some(9_000),
            last_step: None,
            stop_reason: None,
            error_kind: None,
            error: None,
            last_seq: 0,
            role: Some(SubagentRole::Fast),
            status_detail: None,
            cost_currency: Some("USD".to_owned()),
            failure: None,
            cap_inapplicable: None,
        };

        let direct_topo = AgentRunTopology {
            title: "Explain SQL error".to_owned(),
            folder: None,
            run: make_run("run_direct", AgentRunKind::Main, AgentRunStatus::Completed),
            awake_ms: 1_200,
            snapshot_ms: 2_200,
            waiting_for_slot: false,
            wait: None,
            pending_approval: None,
            pending_changes: BoardChanges::default(),
            plan_done: 0,
            plan_total: 0,
            cost_currency: Some("USD".to_owned()),
            last_answer: Some("Missing GROUP BY column.".to_owned()),
            queued_turns: 0,
            queue_paused: false,
            waves: Vec::new(),
            subagent_counts: BoardSubagentCounts::default(),
            task_graph: Vec::new(),
            user_prompt: Some("Why does this SQL query fail?".to_owned()),
            reasoning_preview: None,
            full_answer: Some("Missing `GROUP BY` column.".to_owned()),
        };

        let parallel_topo = AgentRunTopology {
            title: "Refactor gateway auth".to_owned(),
            folder: Some("/tmp/SpanBay".to_owned()),
            run: make_run("run_dag", AgentRunKind::Main, AgentRunStatus::Completed),
            awake_ms: 8_000,
            snapshot_ms: 9_000,
            waiting_for_slot: false,
            wait: None,
            pending_approval: None,
            pending_changes: BoardChanges {
                files: 2,
                added: 42,
                removed: 8,
            },
            plan_done: 0,
            plan_total: 0,
            cost_currency: Some("USD".to_owned()),
            last_answer: Some("Refactored auth and tests pass.".to_owned()),
            queued_turns: 0,
            queue_paused: false,
            waves: vec![
                ExecutionWave::ParallelSubagents {
                    wave_index: 1,
                    started_ms: Some(2_000),
                    ended_ms: Some(5_000),
                    subagents: vec![
                        TopologySubagent {
                            run: make_run(
                                "sub_1",
                                AgentRunKind::Subagent,
                                AgentRunStatus::Completed,
                            ),
                            parent_call_id: "c_sub_1".to_owned(),
                            child_session_id: "cs_1".to_owned(),
                            description: "Audit router.rs".to_owned(),
                            subagent_kind: "explore".to_owned(),
                            resumed: false,
                            awake_ms: 2_800,
                            pending_approval: None,
                            last_answer: Some("Checked 429 retry paths.".to_owned()),
                            brief: Some("Inspect retry_after handling in router.rs".to_owned()),
                            full_report: Some("## Audit\nChecked 429 retry paths.".to_owned()),
                            tool_calls: vec![TopologyToolStep {
                                call_id: "tc_1".to_owned(),
                                name: "grep".to_owned(),
                                class: "read".to_owned(),
                                summary: "retry_after".to_owned(),
                                status: "ok".to_owned(),
                                started_ms: Some(2_100),
                                ended_ms: Some(2_300),
                                duration_ms: Some(200),
                                exit_code: None,
                                parallel_safe: true,
                                args_preview: Some("{\"pattern\":\"retry_after\"}".to_owned()),
                                output_preview: Some("router.rs:42: retry_after".to_owned()),
                                ..TopologyToolStep::default()
                            }],
                        },
                        TopologySubagent {
                            run: make_run(
                                "sub_2",
                                AgentRunKind::Subagent,
                                AgentRunStatus::Completed,
                            ),
                            parent_call_id: "c_sub_2".to_owned(),
                            child_session_id: "cs_2".to_owned(),
                            description: "Verify unit tests".to_owned(),
                            subagent_kind: "explore".to_owned(),
                            resumed: false,
                            awake_ms: 2_500,
                            pending_approval: None,
                            last_answer: Some("Tests baseline clean.".to_owned()),
                            brief: None,
                            full_report: None,
                            tool_calls: Vec::new(),
                        },
                    ],
                },
                ExecutionWave::ParallelTools {
                    wave_index: 2,
                    started_ms: Some(5_200),
                    ended_ms: Some(6_100),
                    tools: vec![
                        TopologyToolStep {
                            call_id: "tc_2".to_owned(),
                            name: "edit_file".to_owned(),
                            class: "write".to_owned(),
                            summary: "auth.rs".to_owned(),
                            status: "ok".to_owned(),
                            started_ms: Some(5_200),
                            ended_ms: Some(5_600),
                            duration_ms: Some(400),
                            exit_code: None,
                            parallel_safe: true,
                            risk: Some("medium".to_owned()),
                            decision: Some("allow".to_owned()),
                            sandbox: Some("seatbelt".to_owned()),
                            file_change_id: Some("fc_1".to_owned()),
                            file_path: Some("src/auth.rs".to_owned()),
                            file_added: 12,
                            file_removed: 3,
                            ..TopologyToolStep::default()
                        },
                        TopologyToolStep {
                            call_id: "tc_3".to_owned(),
                            name: "read_file".to_owned(),
                            class: "read".to_owned(),
                            summary: "auth_tests.rs".to_owned(),
                            status: "ok".to_owned(),
                            started_ms: Some(5_200),
                            ended_ms: Some(6_100),
                            duration_ms: Some(900),
                            exit_code: None,
                            parallel_safe: true,
                            ..TopologyToolStep::default()
                        },
                    ],
                },
            ],
            subagent_counts: BoardSubagentCounts {
                total: 2,
                completed: 2,
                ..BoardSubagentCounts::default()
            },
            task_graph: Vec::new(),
            user_prompt: Some("Refactor gateway auth middleware".to_owned()),
            reasoning_preview: Some("Plan parallel audit then edit.".to_owned()),
            full_answer: Some("## Summary\nRefactored auth and tests pass.".to_owned()),
        };

        let sample_board = AgentBoard {
            running: vec![AgentBoardRow {
                run: parallel_topo.run.clone(),
                title: "Refactor gateway auth".to_owned(),
                folder: Some("/tmp/SpanBay".to_owned()),
                awake_ms: 8_000,
                waiting_for_slot: false,
                wait: None,
                pending_approval: None,
                inline_allow: false,
                plan_done: 1,
                plan_total: 3,
                cost_currency: Some("USD".to_owned()),
                subagents: 2,
                subagent_counts: parallel_topo.subagent_counts,
                pending_changes: parallel_topo.pending_changes,
                last_answer: parallel_topo.last_answer.clone(),
                approval_timed_out: false,
                seen: true,
                review: false,
            }],
            needs_you: Vec::new(),
            done: Vec::new(),
            limits: BoardLimits {
                max_concurrent_runs: 3,
                max_subagents: 3,
                max_steps: 20,
                approval_timeout_s: 600,
                queue_messages: 5,
                auto_compact_pct: Some(85),
            },
            cost_today: Vec::new(),
            tokens_today: TokensToday {
                input: 4_000,
                output: 1_200,
            },
            running_processes: 0,
            active_subagents: 1,
        };

        let sample_history = vec![
            SessionRunHistoryItem {
                run: parallel_topo.run.clone(),
                title: parallel_topo.title.clone(),
                awake_ms: 8_000,
                cost_currency: Some("USD".to_owned()),
                prompt_preview: Some("Refactor gateway auth middleware".to_owned()),
                tool_count: 3,
                subagent_count: 2,
                has_parallel: true,
                has_task_graph: false,
                pending_changes: parallel_topo.pending_changes,
            },
            SessionRunHistoryItem {
                run: direct_topo.run.clone(),
                title: direct_topo.title.clone(),
                awake_ms: 1_200,
                cost_currency: Some("USD".to_owned()),
                prompt_preview: Some("Why does this SQL query fail?".to_owned()),
                tool_count: 0,
                subagent_count: 0,
                has_parallel: false,
                has_task_graph: false,
                pending_changes: BoardChanges::default(),
            },
        ];

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Ocean, false, false, &settings.appearance, cx);
        });

        // 0. Standalone full-width RunsView (NavModule::Runs): filters, scopes, Stop All confirm, handoff.
        let (runs_view, runs_visual) = cx.add_window_view(RunsView::new);
        runs_visual.run_until_parked();
        runs_visual.update(|window, cx| {
            runs_view.update(cx, |v, cx| {
                v.board = Some(sample_board.clone());
                v.all_history = sample_history.clone();
                cx.notify();
            });
            for filter in [
                RunsFilter::All,
                RunsFilter::Waiting,
                RunsFilter::Running,
                RunsFilter::Failed,
                RunsFilter::Done,
            ] {
                runs_view.update(cx, |v, cx| v.set_filter(filter, cx));
                window.refresh();
            }
            runs_view.update(cx, |v, cx| {
                v.set_filter(RunsFilter::All, cx);
                v.set_scope(RunsScope::AllHistory, cx);
            });
            window.refresh();
            runs_view.update(cx, |v, cx| {
                v.open_workflow_modal("as_test".into(), Some("run_dag".into()), None, cx);
            });
            assert!(matches!(
                runs_view.read(cx).take_pending_handoff(),
                Some(RunsHandoff::OpenWorkflowModal { .. })
            ));
        });

        // 1. Shared 90% viewport WorkflowMonitorModal: direct answer -> auto Steps, Graph unavailable.
        let (modal, visual) = cx.add_window_view(WorkflowMonitorModal::new);
        visual.run_until_parked();

        visual.update(|window, cx| {
            modal.update(cx, |m, cx| {
                m.open = true;
                m.session_id = "as_test".to_owned();
                m.run_id = Some("run_direct".to_owned());
                m.topology = Some(direct_topo.clone());
                m.session_history = sample_history.clone();
                assert!(!is_graph_available(&direct_topo));
                assert_eq!(m.effective_tab(&direct_topo), (WorkflowTab::Steps, true));
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear(cx);
        });

        // 2. Parallel fan-out run -> auto Graph, expand Parallel Group, inspect all key archetypes,
        //    switch to Timeline & Changes tabs, verify layered Esc dismissal.
        visual.update(|window, cx| {
            modal.update(cx, |m, cx| {
                m.run_id = Some("run_dag".to_owned());
                m.topology = Some(parallel_topo.clone());
                m.changes = vec![FileChange {
                    id: "fc_1".to_owned(),
                    session_id: "as_test".to_owned(),
                    run_id: "run_dag".to_owned(),
                    call_id: "tc_2".to_owned(),
                    folder: Some("/tmp/SpanBay".to_owned()),
                    path: "src/auth.rs".to_owned(),
                    kind: FileChangeKind::Edit,
                    before_sha256: None,
                    after_sha256: None,
                    added: 12,
                    removed: 3,
                    status: FileChangeStatus::Pending,
                    created_at_ms: 5_600,
                }];
                m.selected_change_id = Some("fc_1".to_owned());
                m.selected_change_diff = Some((
                    "fc_1".to_owned(),
                    "@@ -1,2 +1,3 @@\n fn auth() {}\n+fn verify() {}\n".to_owned(),
                    false,
                ));
                assert!(is_graph_available(&parallel_topo));
                assert_eq!(m.effective_tab(&parallel_topo), (WorkflowTab::Graph, true));
                m.toggle_group("grp-tools-run_dag-2".to_owned(), cx);
                m.select_node(Some("sub:sub_1".to_owned()), cx);
                m.set_zoom(0.75, cx);
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear(cx);

            // Inspect Tool Step, Parallel Group, Main Start, and Main Summary archetypes cleanly.
            for nid in [
                "tool:tc_2",
                "grp-tools-run_dag-2",
                "main-start-run_dag",
                "main-end-run_dag",
            ] {
                modal.update(cx, |m, cx| m.select_node(Some(nid.to_owned()), cx));
                window.refresh();
                window.draw(cx).clear(cx);
            }

            modal.update(cx, |m, cx| {
                m.select_node(Some("sub:sub_1".to_owned()), cx);
                m.set_tab("run_dag".to_owned(), WorkflowTab::Timeline, cx);
                assert_eq!(
                    m.effective_tab(&parallel_topo),
                    (WorkflowTab::Timeline, false)
                );
                assert_eq!(m.selected_node_id.as_deref(), Some("sub:sub_1"));
            });
            window.refresh();
            window.draw(cx).clear(cx);

            // Switch to full-width 2-pane Changes Diff tab & verify layered Esc dismissal.
            modal.update(cx, |m, cx| {
                m.set_tab("run_dag".to_owned(), WorkflowTab::Changes, cx);
                m.open_text_dialog("grep".to_owned(), "full tool output".to_owned(), cx);
            });
            window.refresh();
            window.draw(cx).clear(cx);
            assert!(modal.update(cx, |m, cx| m.dismiss_inner_or_close(cx)));
            assert!(modal.read(cx).full_output_dialog.is_none());
            assert!(modal.read(cx).is_open());
            assert!(modal.update(cx, |m, cx| m.dismiss_inner_or_close(cx)));
            assert!(!modal.read(cx).is_open());
        });

        // 3. Phase 2: Structured Task Graph renders node recovery UI inside WorkflowMonitorModal
        //    and Plan approval gate with embedded Task Graph preview inside ChatView.
        let planned_tasks = vec![
            spanbay_desktop::core::agent::model::AgentTodoItem {
                id: "n1".to_owned(),
                content: "Audit router".to_owned(),
                status: spanbay_desktop::core::agent::model::AgentTodoStatus::Completed,
                kind: Some(spanbay_desktop::core::agent::model::TaskNodeKind::Explore),
                role: Some(SubagentRole::Fast),
                group: Some("Audit".to_owned()),
                summary: Some("Router inspected".to_owned()),
                ..Default::default()
            },
            spanbay_desktop::core::agent::model::AgentTodoItem {
                id: "n2".to_owned(),
                content: "Audit store".to_owned(),
                status: spanbay_desktop::core::agent::model::AgentTodoStatus::Completed,
                kind: Some(spanbay_desktop::core::agent::model::TaskNodeKind::Explore),
                role: Some(SubagentRole::Fast),
                group: Some("Audit".to_owned()),
                ..Default::default()
            },
            spanbay_desktop::core::agent::model::AgentTodoItem {
                id: "n3".to_owned(),
                content: "Implement fix".to_owned(),
                status: spanbay_desktop::core::agent::model::AgentTodoStatus::Failed,
                depends_on: vec!["n1".to_owned(), "n2".to_owned()],
                kind: Some(spanbay_desktop::core::agent::model::TaskNodeKind::Implement),
                attempt: 1,
                error: Some("borrow check failed".to_owned()),
                ..Default::default()
            },
            spanbay_desktop::core::agent::model::AgentTodoItem {
                id: "n4".to_owned(),
                content: "Run tests".to_owned(),
                status: spanbay_desktop::core::agent::model::AgentTodoStatus::Blocked,
                depends_on: vec!["n3".to_owned()],
                kind: Some(spanbay_desktop::core::agent::model::TaskNodeKind::Verify),
                ..Default::default()
            },
        ];

        let mut task_graph_topo = direct_topo.clone();
        task_graph_topo.run.run_id = "run_tg".to_owned();
        task_graph_topo.pending_changes = BoardChanges {
            files: 1,
            added: 12,
            removed: 2,
        };
        task_graph_topo.task_graph = planned_tasks.clone();

        visual.update(|window, cx| {
            modal.update(cx, |m, cx| {
                m.open = true;
                m.run_id = Some("run_tg".to_owned());
                m.topology = Some(task_graph_topo.clone());
                assert!(is_graph_available(&task_graph_topo));
                assert_eq!(
                    m.effective_tab(&task_graph_topo),
                    (WorkflowTab::Graph, true)
                );
                m.select_node(Some("task:n3".to_owned()), cx);
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear(cx);

            // Exercise unified Process Graph layout switching (TB, Serpentine wrap, Swimlanes,
            // hybrid manual pinning & reset).
            modal.update(cx, |m, cx| {
                m.set_layout_direction(spanbay_desktop::state::LayoutDirection::TopToBottom, cx);
            });
            window.refresh();
            window.draw(cx).clear(cx);

            modal.update(cx, |m, cx| {
                m.set_layout_direction(
                    spanbay_desktop::state::LayoutDirection::Serpentine { max_per_row: 3 },
                    cx,
                );
            });
            window.refresh();
            window.draw(cx).clear(cx);

            modal.update(cx, |m, cx| {
                m.toggle_swimlanes(cx);
                assert!(m.layout_policy.swimlanes);
                m.pinned_positions
                    .insert("task:n3".to_owned(), (420.0, 160.0));
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear(cx);

            modal.update(cx, |m, cx| {
                m.toggle_alignment(cx);
                m.reset_pinned_layout(cx);
                assert!(m.pinned_positions.is_empty());
            });
            window.refresh();
            window.draw(cx).clear(cx);
        });

        let (chat, chat_visual) = cx.add_window_view(ChatView::new);
        chat_visual.run_until_parked();
        chat_visual.update(|window, cx| {
            // Render Plan approval gate with embedded Task Graph preview & switch to Markdown.
            chat.update(cx, |v, cx| {
                v.sidebar_mode = ChatSidebarMode::Time;
                v.thread.open_gates.push((
                    "run_plan".to_owned(),
                    spanbay_desktop::core::agent::model::HumanGateDto {
                        id: "gate_plan_1".to_owned(),
                        run_id: "run_plan".to_owned(),
                        call_id: "tc_plan_1".to_owned(),
                        kind: spanbay_desktop::core::agent::model::HumanGateKind::Plan,
                        created_at_ms: 5_000,
                        body: spanbay_desktop::core::agent::model::HumanGateBody::Plan {
                            markdown: "1. Audit\n2. Fix\n3. Verify".to_owned(),
                            title: Some("Auth Task Graph".to_owned()),
                            steps: 4,
                            tasks: planned_tasks.clone(),
                        },
                    },
                ));
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear(cx);

            chat.update(cx, |v, cx| {
                v.set_plan_gate_markdown("gate_plan_1".to_owned(), true, cx);
            });
            window.refresh();
            window.draw(cx).clear(cx);
        });

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn appearance_studio_accent_and_syntax_highlight_themes(cx: &mut TestAppContext) {
        use std::sync::Arc;

        use spanbay_desktop::core::app::settings_get_impl;
        use spanbay_desktop::runtime::DesktopRuntime;
        use spanbay_desktop::state::SettingsSection;
        use spanbay_desktop::theme::{
            AccentChoice, AccentPreset, PaletteId, SyntaxThemeId, ThemePreferences,
        };

        use crate::bridge::CoreBridge;
        use crate::ui::{apply_theme, palette, syntax_palette};
        use crate::views::settings::{SettingsView, appearance::SyntaxPreviewSample};

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());

        let custom_prefs = ThemePreferences {
            palette: PaletteId::Amber,
            accent: AccentChoice::Custom(0xE11D48),
            syntax_theme: SyntaxThemeId::RosePine,
        };

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_theme(custom_prefs, false, false, &settings.appearance, cx);

            let base_amber =
                spanbay_desktop::theme::resolve_palette(PaletteId::Amber, false, false);
            let active = palette(cx);
            assert_ne!(
                active.accent, base_amber.accent,
                "custom theme accent must override the base palette accent"
            );
            assert_eq!(syntax_palette(cx).id, SyntaxThemeId::RosePine);

            // Switch through all 6 syntax themes in both light and dark modes.
            for st in SyntaxThemeId::ALL {
                for dark in [false, true] {
                    let prefs = ThemePreferences {
                        palette: PaletteId::Slate,
                        accent: AccentChoice::Preset(AccentPreset::Emerald),
                        syntax_theme: st,
                    };
                    apply_theme(prefs, dark, false, &settings.appearance, cx);
                    assert_eq!(syntax_palette(cx).id, st);
                    assert_eq!(syntax_palette(cx).is_dark, dark);
                }
            }
            apply_theme(custom_prefs, false, false, &settings.appearance, cx);
        });

        let (view, visual) = cx.add_window_view(|window, cx| {
            SettingsView::new(SettingsSection::Appearance, custom_prefs, window, cx)
        });
        visual.run_until_parked();

        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);

            // Exercise palette, accent preset, custom hex, syntax theme, and preview sample toggles.
            view.update(cx, |v, cx| {
                v.choose_accent(AccentChoice::Preset(AccentPreset::Violet), window, cx);
                assert_eq!(
                    v.take_pending_theme_prefs().map(|p| p.accent),
                    Some(AccentChoice::Preset(AccentPreset::Violet))
                );
                v.choose_syntax_theme(SyntaxThemeId::TokyoNight, cx);
                assert_eq!(
                    v.take_pending_theme_prefs().map(|p| p.syntax_theme),
                    Some(SyntaxThemeId::TokyoNight)
                );
                v.syntax_preview = SyntaxPreviewSample::Xml;
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear(cx);

            view.update(cx, |v, cx| {
                v.syntax_preview = SyntaxPreviewSample::Diff;
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear(cx);
        });

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn workspace_switcher_create_cancel_and_counts_lifecycle(cx: &mut TestAppContext) {
        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let _ = workspace_create_impl(rt.state(), "ws-init".into(), "Initial".into());
        let settings = settings_get_impl(rt.state());
        let (host, rx) = GpuiHostWindow::pair();

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        let mut shell_slot = None;
        let (_root, visual) = cx.add_window_view(|window, cx| {
            let s = cx.new(|cx| AppShell::new(rx, host, window, cx));
            shell_slot = Some(s.clone());
            Root::new(s, window, cx)
        });
        let shell = shell_slot.expect("mounted AppShell");
        visual.run_until_parked();

        visual.update(|window, cx| {
            // 1. Open New Workspace modal -> renders modal dialog cleanly, blank submit rejected, cancel closes.
            shell.update(cx, |s, cx| s.open_new_workspace_modal(window, cx));
            assert!(shell.read(cx).new_workspace_input.is_some());
            window.refresh();
            window.draw(cx).clear(cx);
            assert!(
                !shell.update(cx, |s, cx| s.submit_create_workspace(cx)),
                "blank workspace name must not submit"
            );
            assert!(
                shell.read(cx).new_workspace_input.is_some(),
                "modal stays open when submit is rejected"
            );

            shell.update(cx, |s, cx| s.cancel_create_workspace(cx));
            assert!(shell.read(cx).new_workspace_input.is_none());

            // 2. Re-open modal, type a valid name, and submit -> creates, switches, populates counts, closes modal.
            shell.update(cx, |s, cx| s.open_new_workspace_modal(window, cx));
            let input = shell
                .read(cx)
                .new_workspace_input
                .clone()
                .expect("new workspace modal input");
            input.update(cx, |st, cx| st.set_value("Payments API", window, cx));
            assert!(shell.update(cx, |s, cx| s.submit_create_workspace(cx)));
            assert!(shell.read(cx).new_workspace_input.is_none());
            window.refresh();
        });

        // Wait for blocking workspace_create_impl task to land on foreground shell.
        for _ in 0..80 {
            visual.run_until_parked();
            let created = visual.update(|_window, cx| {
                shell
                    .read(cx)
                    .state
                    .workspaces
                    .iter()
                    .any(|w| w.name == "Payments API")
            });
            if created {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        visual.update(|window, cx| {
            let s = shell.read(cx);
            let created = s
                .state
                .workspaces
                .iter()
                .find(|w| w.name == "Payments API")
                .expect("created workspace must appear in shell state");
            assert_eq!(
                s.state.active_workspace_id.as_deref(),
                Some(created.id.as_str()),
                "newly created workspace must become active"
            );
            assert!(
                s.workspace_counts.contains_key(created.id.as_str()),
                "workspace counts must refresh immediately after creation"
            );
            window.refresh();
            window.draw(cx).clear(cx);
        });

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn add_provider_sniff_modal_dialog_lifecycle(cx: &mut TestAppContext) {
        use crate::views::providers::ProvidersView;

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        let (view, visual) = cx.add_window_view(ProvidersView::new);
        visual.run_until_parked();

        visual.update(|window, cx| {
            // 1. Open Add Provider modal dialog with URL only (no provider name).
            view.update(cx, |v, cx| {
                v.open_sniff_sheet(
                    None,
                    Some("https://relay.example.com".into()),
                    None,
                    window,
                    cx,
                );
            });
            let modal = view
                .read(cx)
                .sniff_modal
                .clone()
                .expect("Add Provider must open as a centered modal dialog on ProvidersView");
            assert!(
                modal.read(cx).name_input.read(cx).value().trim().is_empty(),
                "provider name starts empty when opened without a preset"
            );

            // Submitting sniff or unverified creation without a provider name must be rejected.
            modal.update(cx, |m, cx| {
                m.start_sniff(window, cx);
            });
            assert_eq!(
                modal.read(cx).step,
                1,
                "must stay on Step 1 when name is empty"
            );
            assert!(
                modal.read(cx).flash.is_some(),
                "missing required name must surface warning flash"
            );

            modal.update(cx, |m, cx| {
                m.flash = None;
                m.create_unverified(cx);
            });
            assert!(
                modal.read(cx).flash.is_some(),
                "unverified creation must also require a non-empty provider name"
            );

            // Entering a custom provider name clears the warning flash.
            let name_input = modal.read(cx).name_input.clone();
            name_input.update(cx, |st, cx| st.set_value("Team Relay", window, cx));
            assert_eq!(name_input.read(cx).value().trim(), "Team Relay");
            window.refresh();
            window.draw(cx).clear(cx);

            // Esc / dismiss closes the modal cleanly.
            let dismissed = view.update(cx, |v, cx| v.dismiss_modal_if_open(cx));
            assert!(dismissed);
            assert!(view.read(cx).sniff_modal.is_none());
            window.refresh();
            window.draw(cx).clear(cx);
        });

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn chat_workspace_and_folder_binding_lifecycle(cx: &mut TestAppContext) {
        use crate::views::chat::ChatView;
        use spanbay_desktop::core::app::{
            SaveEnvironmentInput, environment_save_impl, workspace_create_impl,
        };

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());

        let ws_a =
            workspace_create_impl(rt.state(), "ws_alpha".into(), "test".into()).expect("ws_a");
        let env_a = environment_save_impl(
            rt.state(),
            SaveEnvironmentInput {
                id: "env_dev".into(),
                workspace_id: ws_a.id.as_str().to_owned(),
                name: "Dev".into(),
                variables: Vec::new(),
                confirm_before_send: Some(false),
                confirm_safe_methods: Some(false),
                declared_hosts: Some(Vec::new()),
                base_revision: None,
            },
        )
        .expect("env_a");
        let ws_b = workspace_create_impl(rt.state(), "ws_beta".into(), "Ops".into()).expect("ws_b");
        let env_b = environment_save_impl(
            rt.state(),
            SaveEnvironmentInput {
                id: "env_prod".into(),
                workspace_id: ws_b.id.as_str().to_owned(),
                name: "Prod".into(),
                variables: Vec::new(),
                confirm_before_send: Some(false),
                confirm_safe_methods: Some(false),
                declared_hosts: Some(Vec::new()),
                base_revision: None,
            },
        )
        .expect("env_b");

        let folder = std::env::temp_dir().join(format!(
            "spanbay-chat-bind-{}",
            spanbay_desktop::core::app::current_time_ms()
        ));
        std::fs::create_dir_all(&folder).expect("create temp project dir");
        let canonical_folder = std::fs::canonicalize(&folder)
            .expect("canonical temp dir")
            .to_string_lossy()
            .into_owned();

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        let (view, visual) = cx.add_window_view(ChatView::new);
        visual.run_until_parked();

        // 1. Seed active workspace + environment from Shell, then bind a folder BEFORE any session exists.
        visual.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.set_active_binding(
                    Some(ws_a.id.as_str().to_owned()),
                    Some(env_a.id.clone()),
                    cx,
                );
                v.bind_folder_path(folder.to_string_lossy().into_owned(), cx);
            });
        });

        for _ in 0..80 {
            visual.run_until_parked();
            let ready = visual.read(|cx| {
                view.read(cx).session.as_ref().is_some_and(|s| {
                    s.workspace_dir.as_deref() == Some(canonical_folder.as_str())
                        && s.spanbay_workspace_id.as_deref() == Some("ws_alpha")
                })
            });
            if ready {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let first_session_id = visual.update(|window, cx| {
            let v = view.read(cx);
            let s = v
                .session
                .as_ref()
                .expect("binding folder on empty chat must create a session");
            assert_eq!(s.workspace_dir.as_deref(), Some(canonical_folder.as_str()));
            assert_eq!(s.spanbay_workspace_id.as_deref(), Some("ws_alpha"));
            assert_eq!(s.spanbay_environment_id.as_deref(), Some("env_dev"));
            assert!(
                v.environments.iter().any(|e| e.id == env_a.id),
                "bound workspace environments must load automatically"
            );
            let sid = s.id.clone();
            window.refresh();
            window.draw(cx).clear(cx);
            sid
        });

        // 2. Rebind the idle chat to ws_beta (clearing env_id) and confirm environments refresh.
        visual.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.bind_workspace_env(ws_b.id.as_str().to_owned(), None, cx);
            });
        });
        for _ in 0..80 {
            visual.run_until_parked();
            let rebound = visual.read(|cx| {
                let v = view.read(cx);
                v.session
                    .as_ref()
                    .is_some_and(|s| s.spanbay_workspace_id.as_deref() == Some("ws_beta"))
                    && v.environments.iter().any(|e| e.id == env_b.id)
            });
            if rebound {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        visual.update(|window, cx| {
            let v = view.read(cx);
            let s = v.session.as_ref().expect("session present after rebind");
            assert_eq!(s.id, first_session_id);
            assert_eq!(s.spanbay_workspace_id.as_deref(), Some("ws_beta"));
            assert_eq!(s.spanbay_environment_id, None);
            assert!(v.environments.iter().any(|e| e.id == env_b.id));
            window.refresh();
            window.draw(cx).clear(cx);
        });

        // 3. Switching Shell's active workspace keeps the existing bound chat untouched, while a
        //    brand-new chat inherits the newly active workspace.
        visual.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.set_active_workspace(Some(ws_a.id.as_str().to_owned()), cx);
            });
            assert_eq!(
                view.read(cx)
                    .session
                    .as_ref()
                    .and_then(|s| s.spanbay_workspace_id.as_deref()),
                Some("ws_beta"),
                "switching shell workspace must not redirect an already-bound chat"
            );
            view.update(cx, |v, cx| v.create_session(cx));
        });
        for _ in 0..80 {
            visual.run_until_parked();
            let switched = visual.read(|cx| {
                view.read(cx).session.as_ref().is_some_and(|s| {
                    s.id != first_session_id
                        && s.spanbay_workspace_id.as_deref() == Some("ws_alpha")
                })
            });
            if switched {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        visual.update(|_, cx| {
            let s = view.read(cx).session.as_ref().expect("new session");
            assert_ne!(s.id, first_session_id);
            assert_eq!(s.spanbay_workspace_id.as_deref(), Some("ws_alpha"));
        });

        let _ = std::fs::remove_dir_all(&folder);
        rt.shutdown_blocking();
    }

    /// Verifies Plan A tool trace presentation in `ChatView`:
    /// - One row per command (`read_file` wrapped in `<external_data>`, plus `shell`),
    /// - Expanding a `read_file` step renders cleanly without leaking `<external_data>` tags,
    /// - Opening full preview on a non-spilled tool call opens `ChatModal::ToolOutput` directly
    ///   with cleaned file contents instead of failing with `NotFound`,
    /// - Attempting to open an out-of-workspace traversal path reports a validation error.
    #[gpui_kit::test]
    fn chat_tool_trace_enveloped_read_file_and_inline_preview_modal(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use crate::views::chat::ChatView;
        use spanbay_desktop::core::agent::model::{AgentMessage, AgentRole, AgentSessionInput};
        use spanbay_desktop::core::agent::untrusted::envelope_tags;
        use spanbay_desktop::core::app::agent_session_create_impl;
        use spanbay_desktop::core::model::hub::HubPart;
        use spanbay_desktop::state::format_tool_trace;

        // `ChatView::new` starts `reload_sessions` on the real `spanbay-core` Tokio runtime, whose
        // completion wakes a GPUI foreground future from a Tokio thread: opt into the parking
        // mode every other real-runtime test here uses, or the scheduler flags that wake.
        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = rt.state().settings();
        let folder = std::env::temp_dir().join(format!(
            "spanbay-trace-ui-test-{}",
            spanbay_desktop::core::app::current_time_ms()
        ));
        std::fs::create_dir_all(&folder).expect("create temp workspace folder");
        std::fs::write(folder.join("Cargo.toml"), "[package]\nname = \"demo\"\n")
            .expect("write Cargo.toml");

        let (open_tag, close_tag) = envelope_tags("read_file");
        let mut numbered = String::new();
        for i in 1..=22 {
            numbered.push_str(&format!("{i:>6}\tfield_{i} = \"val_{i}\"\n"));
        }
        let enveloped_output = format!("{open_tag}\n{numbered}{close_tag}");

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        let (view, visual) = cx.add_window_view(ChatView::new);
        visual.run_until_parked();

        let created = agent_session_create_impl(
            rt.state(),
            AgentSessionInput {
                title: Some("Trace test".to_owned()),
                workspace_dir: Some(folder.to_string_lossy().into_owned()),
                ..AgentSessionInput::default()
            },
        )
        .expect("create session");
        let sid = created.id.clone();

        visual.update(|_, cx| {
            view.update(cx, |chat, cx| {
                chat.thread.session_id = sid.clone();
                chat.session = Some(created.clone());
                chat.thread.messages = vec![
                    AgentMessage {
                        id: "m-assistant-1".to_owned(),
                        session_id: sid.clone(),
                        run_id: Some("r-trace-1".to_owned()),
                        seq: 1,
                        role: AgentRole::Assistant,
                        parts: vec![
                            HubPart::ToolCall {
                                id: "call_read_1".to_owned(),
                                name: "read_file".to_owned(),
                                arguments: r#"{"path":"Cargo.toml"}"#.to_owned(),
                                signature: None,
                                origin: None,
                            },
                            HubPart::ToolCall {
                                id: "call_shell_1".to_owned(),
                                name: "shell".to_owned(),
                                arguments: r#"{"command":"cargo check"}"#.to_owned(),
                                signature: None,
                                origin: None,
                            },
                        ],
                        attachments: Vec::new(),
                        model: Some("test-model".to_owned()),
                        usage: None,
                        parent_id: None,
                        branch_index: 0,
                        partial: false,
                        cost_micro_usd: 0,
                        cost_currency: None,
                        meta: Default::default(),
                        branch_siblings: Vec::new(),
                        error: None,
                        created_at_ms: 1,
                    },
                    AgentMessage {
                        id: "m-tool-1".to_owned(),
                        session_id: sid.clone(),
                        run_id: Some("r-trace-1".to_owned()),
                        seq: 2,
                        role: AgentRole::Tool,
                        parts: vec![
                            HubPart::ToolResult {
                                call_id: "call_read_1".to_owned(),
                                name: Some("read_file".to_owned()),
                                content: vec![HubPart::text(enveloped_output.clone())],
                                is_error: false,
                            },
                            HubPart::ToolResult {
                                call_id: "call_shell_1".to_owned(),
                                name: Some("shell".to_owned()),
                                content: vec![HubPart::text("Finished dev profile\n")],
                                is_error: false,
                            },
                        ],
                        attachments: Vec::new(),
                        model: None,
                        usage: None,
                        parent_id: None,
                        branch_index: 0,
                        partial: false,
                        cost_micro_usd: 0,
                        cost_currency: None,
                        meta: Default::default(),
                        branch_siblings: Vec::new(),
                        error: None,
                        created_at_ms: 2,
                    },
                ];
                chat.turn_inputs_version += 1;
                cx.notify();
            });
        });

        // Default state (collapsed) and expanded state both paint cleanly.
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            view.update(cx, |chat, cx| {
                chat.toggle_trace("call_read_1".to_owned(), cx);
                chat.toggle_trace("call_shell_1".to_owned(), cx);
            });
            window.refresh();
            window.draw(cx).clear(cx);
        });

        // Non-spilled full preview opens cleanly with stripped envelope & plain code.
        let pres = format_tool_trace(
            "read_file",
            r#"{"path":"Cargo.toml"}"#,
            Some(&enveloped_output),
            false,
            false,
            None,
        );
        assert!(pres.can_preview_full);
        assert!(pres.external_isolated);
        visual.update(|window, cx| {
            view.update(cx, |chat, cx| {
                chat.open_tool_output(
                    "call_read_1".to_owned(),
                    "read_file".to_owned(),
                    "Read · Cargo.toml".to_owned(),
                    pres.cleaned_full_output.clone(),
                    false,
                    cx,
                );
            });
            assert!(view.read(cx).review_open);
            assert!(view.read(cx).sidebar_collapsed);
            match view.read(cx).review_tool_output.as_ref() {
                Some(out) => {
                    assert_eq!(out.call_id, "call_read_1");
                    assert!(!out.has_more);
                    assert!(!out.content.contains("<external_data"));
                    assert!(out.content.contains("field_1 = \"val_1\""));
                    assert!(out.content.contains("field_22 = \"val_22\""));
                }
                other => panic!("expected review_tool_output, got {other:?}"),
            }
            assert!(view.read(cx).status_msg.is_none());
            window.refresh();
            window.draw(cx).clear(cx);

            // External open rejects traversal outside workspace without panicking.
            view.update(cx, |chat, cx| {
                chat.open_workspace_file_external("../escape.txt".to_owned(), cx);
            });
        });
        for _ in 0..50 {
            visual.run_until_parked();
            if visual.read(|cx| view.read(cx).status_msg.is_some()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        visual.update(|_window, cx| {
            assert!(matches!(
                view.read(cx).status_msg.as_ref(),
                Some((crate::ui::Tone::Danger, _))
            ));
        });
        let _ = std::fs::remove_dir_all(&folder);
        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn chat_review_panel_hunk_revert_running_composer_and_density_cycling(cx: &mut TestAppContext) {
        use crate::views::chat::{ChatView, TranscriptDensity};
        use spanbay_desktop::core::agent::model::{
            AgentRunStatus, AgentSessionInput, FileChange, FileChangeKind, FileChangeStatus,
            QueuedContent,
        };
        use spanbay_desktop::core::app::{agent_blob_put_impl, agent_session_create_impl};
        use spanbay_desktop::core::model::hub::HubPart;
        use spanbay_desktop::state::provisional_run_summary;

        cx.background_executor.allow_parking();
        let folder =
            std::env::temp_dir().join(format!("spanbay-rv-ui-test-{}", std::process::id(),));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).expect("temp workspace dir");
        let canonical_ws = folder.canonicalize().unwrap_or_else(|_| folder.clone());
        let before = "line1\nold_top\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\nold_bot\nline12\n";
        let after = "line1\nnew_top\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\nnew_bot\nline12\n";
        std::fs::write(canonical_ws.join("router.rs"), after).expect("write router.rs");
        let data_dir = folder.join("data");
        std::fs::create_dir_all(&data_dir).expect("create temp data dir");
        let rt = Arc::new(
            DesktopRuntime::open_at(&data_dir.join("spanbay.sqlite3")).expect("on-disk runtime"),
        );
        let settings = settings_get_impl(rt.state());
        let created = agent_session_create_impl(
            rt.state(),
            AgentSessionInput {
                title: Some("Review & Shell Test".into()),
                workspace_dir: Some(canonical_ws.to_string_lossy().into_owned()),
                ..AgentSessionInput::default()
            },
        )
        .expect("create session");
        let before_sha =
            agent_blob_put_impl(rt.state(), before.as_bytes()).expect("store before blob");
        let after_sha =
            agent_blob_put_impl(rt.state(), after.as_bytes()).expect("compute after sha256");
        let fc = FileChange {
            id: "fc-rv-1".into(),
            session_id: created.id.clone(),
            run_id: "run-rv-1".into(),
            call_id: "call-rv-1".into(),
            path: "router.rs".into(),
            folder: Some(canonical_ws.to_string_lossy().into_owned()),
            kind: FileChangeKind::Edit,
            before_sha256: Some(before_sha),
            after_sha256: Some(after_sha),
            added: 2,
            removed: 2,
            status: FileChangeStatus::Pending,
            created_at_ms: 1000,
        };
        rt.state()
            .store()
            .insert_file_change(&fc)
            .expect("insert file change");
        let queued = rt
            .state()
            .store()
            .add_queued_turn(
                &created.id,
                &QueuedContent {
                    parts: vec![HubPart::text("initial queued prompt")],
                    skills: Vec::new(),
                    contexts: Vec::new(),
                },
            )
            .expect("add queued turn")
            .expect("queue not full");

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        let (view, visual) = cx.add_window_view(ChatView::new);
        visual.run_until_parked();

        let listed =
            spanbay_desktop::core::app::agent_session_list_impl(rt.state(), None, None, None)
                .expect("list sessions");
        let summary = listed
            .sessions
            .iter()
            .find(|s| s.id == created.id)
            .expect("created session in list");
        assert_eq!((summary.pending_added, summary.pending_removed), (2, 2));
        // 1. Density cycling: Normal -> Verbose -> Compact -> Normal.
        visual.update(|window, cx| {
            assert_eq!(view.read(cx).density, TranscriptDensity::Normal);
            view.update(cx, |v, cx| v.cycle_density(cx));
            assert_eq!(view.read(cx).density, TranscriptDensity::Verbose);
            view.update(cx, |v, cx| v.cycle_density(cx));
            assert_eq!(view.read(cx).density, TranscriptDensity::Compact);
            view.update(cx, |v, cx| v.cycle_density(cx));
            assert_eq!(view.read(cx).density, TranscriptDensity::Normal);
            window.refresh();
            window.draw(cx).clear(cx);
        });

        // 2. Review panel open auto-collapses session list and restores on close; width persists.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.thread.session_id = created.id.clone();
                v.session = Some(created.clone());
                v.persisted_changes = vec![fc.clone()];
                v.turn_inputs_version += 1;
                v.open_review(Some("router.rs".into()), cx);
            });
            assert!(view.read(cx).review_open);
            assert!(view.read(cx).sidebar_collapsed);
            assert_eq!(
                view.read(cx).review_selected_path.as_deref(),
                Some("router.rs")
            );
            view.update(cx, |v, cx| v.set_review_panel_width(500.0, cx));
            assert_eq!(view.read(cx).review_panel_width, 500.0);
            window.refresh();
            window.draw(cx).clear(cx);
        });
        for _ in 0..30 {
            visual.run_until_parked();
            if visual.read(|cx| view.read(cx).review_diff_cache.is_some()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // 3. Per-hunk revert action reverts hunk 0 while preserving hunk 1.
        visual.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.revert_change_hunk("fc-rv-1".into(), 0, cx);
            });
        });
        for _ in 0..40 {
            visual.run_until_parked();
            if visual.read(|cx| {
                view.read(cx)
                    .persisted_changes
                    .first()
                    .is_some_and(|c| c.added == 1 && c.removed == 1)
            }) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let disk_after_hunk =
            std::fs::read_to_string(canonical_ws.join("router.rs")).expect("read router.rs");
        assert!(disk_after_hunk.contains("old_top\n") && disk_after_hunk.contains("new_bot\n"));

        // 3b. Two changes on one file in the same turn: Net diff hides per-hunk revert while
        //     PerEdit enables it for the latest change; deleted files never show per-hunk revert.
        let fc_second = FileChange {
            id: "fc-rv-2".into(),
            created_at_ms: 1001,
            ..fc.clone()
        };
        let fc_deleted = FileChange {
            id: "fc-rv-del".into(),
            path: "removed.rs".into(),
            after_sha256: None,
            added: 0,
            removed: 5,
            created_at_ms: 1002,
            ..fc.clone()
        };
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.persisted_changes = vec![fc_deleted.clone(), fc_second.clone(), fc.clone()];
                v.turn_inputs_version += 1;
                v.set_review_diff_mode(crate::views::chat::ReviewDiffMode::Net, cx);
            });
            assert!(!view.read(cx).can_revert_hunk_in_current_view("router.rs"));
            assert!(!view.read(cx).can_revert_hunk_in_current_view("removed.rs"));
            view.update(cx, |v, cx| {
                v.set_review_diff_mode(crate::views::chat::ReviewDiffMode::PerEdit, cx);
            });
            assert!(view.read(cx).can_revert_hunk_in_current_view("router.rs"));
            assert!(!view.read(cx).can_revert_hunk_in_current_view("removed.rs"));
            window.refresh();
            window.draw(cx).clear(cx);
        });
        visual.update(|window, cx| {
            view.update(cx, |v, cx| v.close_review(cx));
            assert!(!view.read(cx).review_open);
            assert!(!view.read(cx).sidebar_collapsed);
            window.refresh();
            window.draw(cx).clear(cx);
        });

        // 4. Running composer shows Stop and queue/steer + editable queue item.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.thread.active_run = Some(provisional_run_summary(
                    "run-rv-1".into(),
                    created.id.clone(),
                    AgentRunStatus::Running,
                    None,
                    None,
                    2,
                ));
                v.thread.queued_turns = vec![queued.clone()];
                v.begin_edit_queued_turn(queued.id.clone(), window, cx);
            });
            assert!(view.read(cx).is_run_active());
            assert_eq!(
                view.read(cx).editing_queue_id.as_deref(),
                Some(queued.id.as_str())
            );
            window.refresh();
            window.draw(cx).clear(cx);
            view.update(cx, |v, cx| {
                v.composer
                    .update(cx, |c, cx| c.set_value("updated queued prompt", window, cx));
                v.save_edited_queued_turn(window, cx);
            });
        });
        for _ in 0..30 {
            visual.run_until_parked();
            if visual.read(|cx| {
                view.read(cx).thread.queued_turns.first().is_some_and(|q| {
                    q.parts.iter().any(|p| {
                        matches!(p, HubPart::Text { text, .. } if text == "updated queued prompt")
                    })
                })
            }) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(visual.read(|cx| view.read(cx).editing_queue_id.is_none()));

        let _ = std::fs::remove_dir_all(&folder);
        rt.shutdown_blocking();
    }

    /// W1 headless verification:
    /// 1. Finished older turns auto-collapse while the latest turn stays expanded, and
    ///    `toggle_turn_fold` toggles collapsed/expanded state cleanly.
    /// 2. Consecutive read-only calls (`read_file` + `grep`) merge into one `ExploreGroup`,
    ///    while `todo_write` is silenced from the stream.
    /// 3. Non-zero exit code / failed tool calls auto-expand (`is_tool_call_expanded`) and
    ///    prevent turn auto-collapse even in `Compact` mode.
    /// 4. Deny-with-feedback on the inline approval card (`Tab` -> type feedback -> `submit_deny`)
    ///    sends `ApprovalDecision::Deny` with the user's feedback reason to the model.
    #[gpui_kit::test]
    fn chat_turn_fold_explore_merge_failure_auto_expand_and_deny_with_feedback(
        cx: &mut TestAppContext,
    ) {
        use crate::views::chat::thread::is_turn_collapsed;
        use crate::views::chat::trace::is_tool_call_expanded;
        use crate::views::chat::{ChatView, TranscriptDensity as Density};
        use spanbay_desktop::core::agent::approvals::RuleContext;
        use spanbay_desktop::core::agent::attention::PendingApprovalSummary;
        use spanbay_desktop::core::agent::model::{
            AgentMessage, AgentRole, AgentSessionInput, ApprovalDecision, ApprovalScope,
            LiveApprovalRequest, RiskLevel, SandboxKind, ToolCallStatus, ToolClass,
        };
        use spanbay_desktop::core::app::agent_session_create_impl;
        use spanbay_desktop::core::model::hub::HubPart;
        use spanbay_desktop::state::{
            LiveApproval, LiveToolMeta, ThreadTurnEntry, TurnActivityItem,
        };

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        let (view, visual) = cx.add_window_view(ChatView::new);
        for _ in 0..20 {
            visual.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let session =
            agent_session_create_impl(rt.state(), AgentSessionInput::default()).expect("session");
        let sid = session.id.clone();

        let mk_msg = |id: &str, seq: i64, role: AgentRole, parts: Vec<HubPart>| AgentMessage {
            id: id.to_owned(),
            session_id: sid.clone(),
            run_id: Some("r-w1".to_owned()),
            seq,
            role,
            parts,
            attachments: Vec::new(),
            model: Some("deepseek/deepseek-v4-pro".to_owned()),
            usage: None,
            parent_id: None,
            branch_index: 0,
            partial: false,
            cost_micro_usd: 0,
            cost_currency: Some("USD".to_owned()),
            meta: Default::default(),
            branch_siblings: Vec::new(),
            error: None,
            created_at_ms: seq * 1_000,
        };

        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.thread.session_id = sid.clone();
                v.session = Some(session.clone());
                // Turn 1 (older finished turn): 2 consecutive read-only calls + 1 todo_write + answer.
                // Turn 2 (latest turn): 1 successful shell + 1 failed shell (exit 101) + answer.
                v.thread.messages = vec![
                    mk_msg("u-1", 1, AgentRole::User, vec![HubPart::text("Explore repo")]),
                    mk_msg(
                        "a-1",
                        2,
                        AgentRole::Assistant,
                        vec![
                            HubPart::ToolCall {
                                id: "c-todo-1".to_owned(),
                                name: "todo_write".to_owned(),
                                arguments: r#"{"todos":[{"id":"1","content":"Read","status":"completed"}]}"#.to_owned(),
                                signature: None,
                                origin: None,
                            },
                            HubPart::ToolCall {
                                id: "c-read-1".to_owned(),
                                name: "read_file".to_owned(),
                                arguments: r#"{"path":"src/router.rs"}"#.to_owned(),
                                signature: None,
                                origin: None,
                            },
                            HubPart::ToolCall {
                                id: "c-read-1-dup".to_owned(),
                                name: "read_file".to_owned(),
                                arguments: r#"{"path":"src/router.rs"}"#.to_owned(),
                                signature: None,
                                origin: None,
                            },
                            HubPart::ToolCall {
                                id: "c-read-2".to_owned(),
                                name: "read_file".to_owned(),
                                arguments: r#"{"path":"src/limits.rs"}"#.to_owned(),
                                signature: None,
                                origin: None,
                            },
                            HubPart::ToolCall {
                                id: "c-grep-1".to_owned(),
                                name: "grep".to_owned(),
                                arguments: r#"{"pattern":"429"}"#.to_owned(),
                                signature: None,
                                origin: None,
                            },
                            HubPart::text("Explored router.rs."),
                        ],
                    ),
                    mk_msg(
                        "t-1",
                        3,
                        AgentRole::Tool,
                        vec![
                            HubPart::ToolResult {
                                call_id: "c-todo-1".to_owned(),
                                name: Some("todo_write".to_owned()),
                                content: vec![HubPart::text("ok")],
                                is_error: false,
                            },
                            HubPart::ToolResult {
                                call_id: "c-read-1".to_owned(),
                                name: Some("read_file".to_owned()),
                                content: vec![HubPart::text("1\tfn router() {}")],
                                is_error: false,
                            },
                            HubPart::ToolResult {
                                call_id: "c-read-1-dup".to_owned(),
                                name: Some("read_file".to_owned()),
                                content: vec![HubPart::text("1\tfn router() {}")],
                                is_error: false,
                            },
                            HubPart::ToolResult {
                                call_id: "c-read-2".to_owned(),
                                name: Some("read_file".to_owned()),
                                content: vec![HubPart::text("1\tconst MAX: u32 = 15;")],
                                is_error: false,
                            },
                            HubPart::ToolResult {
                                call_id: "c-grep-1".to_owned(),
                                name: Some("grep".to_owned()),
                                content: vec![HubPart::text("src/router.rs:1: 429")],
                                is_error: false,
                            },
                        ],
                    ),
                    mk_msg("u-2", 4, AgentRole::User, vec![HubPart::text("Run tests")]),
                    mk_msg(
                        "a-2",
                        5,
                        AgentRole::Assistant,
                        vec![
                            HubPart::ToolCall {
                                id: "c-sh-ok".to_owned(),
                                name: "shell".to_owned(),
                                arguments: r#"{"command":"cargo test router"}"#.to_owned(),
                                signature: None,
                                origin: None,
                            },
                            HubPart::ToolCall {
                                id: "c-sh-fail".to_owned(),
                                name: "shell".to_owned(),
                                arguments: r#"{"command":"cargo build --offline"}"#.to_owned(),
                                signature: None,
                                origin: None,
                            },
                            HubPart::text("Offline build failed."),
                        ],
                    ),
                    mk_msg(
                        "t-2",
                        6,
                        AgentRole::Tool,
                        vec![
                            HubPart::ToolResult {
                                call_id: "c-sh-ok".to_owned(),
                                name: Some("shell".to_owned()),
                                content: vec![HubPart::text("test result: ok.")],
                                is_error: false,
                            },
                            HubPart::ToolResult {
                                call_id: "c-sh-fail".to_owned(),
                                name: Some("shell".to_owned()),
                                content: vec![HubPart::text("error: offline")],
                                is_error: true,
                            },
                        ],
                    ),
                ];
                v.thread.live_tool_meta.insert(
                    "c-sh-ok".to_owned(),
                    LiveToolMeta {
                        status: ToolCallStatus::Ok,
                        duration_ms: 800,
                        exit_code: Some(0),
                        link: None,
                    },
                );
                v.thread.live_tool_meta.insert(
                    "c-sh-fail".to_owned(),
                    LiveToolMeta {
                        status: ToolCallStatus::Error,
                        duration_ms: 3400,
                        exit_code: Some(101),
                        link: None,
                    },
                );
                v.turn_inputs_version += 1;
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear(cx);

            // Verify Turn 1 (older turn) auto-collapsed, ExploreGroup merged, and todo_write hidden.
            let entries = view.read(cx).cached_turn_entries();
            assert_eq!(entries.len(), 4);
            let ThreadTurnEntry::AssistantTurn(turn1) = &entries[1] else {
                panic!("expected AssistantTurn 1");
            };
            assert!(is_turn_collapsed(view.read(cx), turn1));
            assert_eq!(turn1.totals.hidden_todo_count, 1);
            assert_eq!(turn1.spine.len(), 1, "read_file + grep merged into 1 ExploreGroup and todo_write hidden");
            match &turn1.spine[0] {
                TurnActivityItem::ExploreGroup(eg) => {
                    assert_eq!(eg.files_read, 2, "duplicate read of src/router.rs deduplicated");
                    assert_eq!(eg.searches_run, 1);
                    assert_eq!(eg.items.len(), 2, "consecutive reads merged into 1 sub-row");
                    assert_eq!(eg.items[0].pres.target, "router.rs, limits.rs");
                }
                other => panic!("expected ExploreGroup, got {other:?}"),
            }

            // Toggle Turn 1 expanded -> collapsed.
            let t1_id = turn1.turn_id.clone();
            view.update(cx, |v, cx| v.toggle_turn_fold(t1_id.clone(), cx));
            assert!(!is_turn_collapsed(view.read(cx), turn1));
            window.refresh();
            window.draw(cx).clear(cx);
            view.update(cx, |v, cx| v.toggle_turn_fold(t1_id, cx));
            assert!(is_turn_collapsed(view.read(cx), turn1));

            // Verify Turn 2 (failure auto-expand): exit 0 shell is collapsed, exit 101 shell is auto-expanded,
            // and even in Compact density mode a turn with a failure does not auto-collapse.
            view.update(cx, |v, cx| v.set_density(Density::Compact, cx));
            let entries_compact = view.read(cx).cached_turn_entries();
            let ThreadTurnEntry::AssistantTurn(turn2) = &entries_compact[3] else {
                panic!("expected AssistantTurn 2");
            };
            assert!(!is_turn_collapsed(view.read(cx), turn2));
            match (&turn2.spine[0], &turn2.spine[1]) {
                (TurnActivityItem::ToolCall(sh_ok), TurnActivityItem::ToolCall(sh_fail)) => {
                    assert!(!is_tool_call_expanded(view.read(cx), sh_ok));
                    assert!(is_tool_call_expanded(view.read(cx), sh_fail));
                }
                other => panic!("expected two shell ToolCalls, got {other:?}"),
            }
        });

        // 4. Deny with feedback: Tab on focused approval card opens deny_reason_input, typing
        //    feedback and submitting records ApprovalDecision::Deny with the feedback message.
        let _ticket = rt
            .state()
            .agent
            .approvals
            .register(
                PendingApprovalSummary {
                    run_id: "r-deny-fb".to_owned(),
                    session_id: sid.clone(),
                    call_id: "call-deny-fb".to_owned(),
                    tool: "shell".to_owned(),
                    reason: "mode".to_owned(),
                    preview: None,
                    requested_ms: 1_000,
                    risk: "medium".to_owned(),
                    summary: "Run cargo test".to_owned(),
                    full_text: None,
                    card: None,
                },
                RuleContext {
                    session_id: sid.clone(),
                    workspace_dir: None,
                    tool: "shell".to_owned(),
                    pattern: Some("cargo test *".to_owned()),
                    offered: vec![ApprovalScope::Once, ApprovalScope::Session],
                },
            )
            .expect("register approval ticket");

        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.thread.pending_approvals = vec![LiveApproval::new(
                    "r-deny-fb".to_owned(),
                    "call-deny-fb".to_owned(),
                    "shell".to_owned(),
                    LiveApprovalRequest {
                        arguments: r#"{"command":"cargo test"}"#.to_owned(),
                        class: ToolClass::Exec,
                        read_only: false,
                        destructive: false,
                        sandbox: SandboxKind::Seatbelt,
                        reason: "mode".to_owned(),
                        preview: None,
                        network: false,
                        risk: RiskLevel::Medium,
                        summary: "Run cargo test".to_owned(),
                        full_text: None,
                        card: None,
                        requested_at_ms: 1_000,
                    },
                )];
                window.focus(&v.approval_focus, cx);
                cx.notify();
            });
            window.refresh();
        });
        visual.simulate_keystrokes("tab");
        visual.run_until_parked();
        visual.update(|window, cx| {
            assert_eq!(
                view.read(cx).active_deny_reason_call(),
                Some("call-deny-fb"),
                "Tab on approval card must open the deny feedback input"
            );
            view.update(cx, |v, cx| {
                v.set_deny_reason_text("Only run router unit tests first", window, cx);
                v.submit_deny(cx);
            });
        });
        for _ in 0..40 {
            visual.run_until_parked();
            if visual.read(|cx| view.read(cx).last_approval_notice.is_some()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        visual.read(|cx| {
            let notice = view
                .read(cx)
                .last_approval_notice
                .as_ref()
                .expect("deny-with-feedback notice recorded");
            assert_eq!(notice.decision, ApprovalDecision::Deny);
            assert_eq!(
                notice.reason.as_deref(),
                Some("Only run router unit tests first")
            );
        });

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn execution_graph_node_click_and_drag_discrimination(cx: &mut TestAppContext) {
        use crate::views::runs::graph::build_modal_graph_layout;
        use crate::views::runs::{WorkflowMonitorModal, WorkflowTab};
        use gpui_kit::{Modifiers, MouseButton, point, px};
        use spanbay_desktop::core::agent::dto::{
            AgentRunTopology, BoardChanges, BoardSubagentCounts, ExecutionWave, TopologySubagent,
        };
        use spanbay_desktop::core::agent::model::{
            AgentRunKind, AgentRunStatus, AgentRunSummary, SubagentRole,
        };
        use spanbay_desktop::core::llm::hub::HubUsage;

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        let make_run = |id: &str, kind: AgentRunKind, status: AgentRunStatus| AgentRunSummary {
            run_id: id.to_owned(),
            session_id: "as_drag".to_owned(),
            kind,
            parent_run_id: None,
            parent_call_id: None,
            status,
            model_label: "deepseek/reasoner".to_owned(),
            target: None,
            iterations: 2,
            usage: HubUsage::default(),
            cost_micro_usd: 0,
            started_at_ms: 1_000,
            finished_at_ms: Some(5_000),
            last_event_ms: Some(5_000),
            last_step: None,
            stop_reason: None,
            error_kind: None,
            error: None,
            last_seq: 0,
            role: Some(SubagentRole::Fast),
            status_detail: None,
            cost_currency: Some("USD".to_owned()),
            failure: None,
            cap_inapplicable: None,
        };

        let topo = AgentRunTopology {
            title: "Drag test run".to_owned(),
            folder: None,
            run: make_run("run_drag", AgentRunKind::Main, AgentRunStatus::Completed),
            awake_ms: 4_000,
            snapshot_ms: 5_000,
            waiting_for_slot: false,
            wait: None,
            pending_approval: None,
            pending_changes: BoardChanges::default(),
            plan_done: 0,
            plan_total: 0,
            cost_currency: Some("USD".to_owned()),
            last_answer: Some("Done.".to_owned()),
            queued_turns: 0,
            queue_paused: false,
            waves: vec![ExecutionWave::ParallelSubagents {
                wave_index: 1,
                started_ms: Some(1_500),
                ended_ms: Some(4_500),
                subagents: vec![
                    TopologySubagent {
                        run: make_run("sub_a", AgentRunKind::Subagent, AgentRunStatus::Completed),
                        parent_call_id: "c_a".to_owned(),
                        child_session_id: "cs_a".to_owned(),
                        description: "Explore A".to_owned(),
                        subagent_kind: "explore".to_owned(),
                        resumed: false,
                        awake_ms: 2_000,
                        pending_approval: None,
                        last_answer: Some("A done".to_owned()),
                        brief: None,
                        full_report: None,
                        tool_calls: Vec::new(),
                    },
                    TopologySubagent {
                        run: make_run("sub_b", AgentRunKind::Subagent, AgentRunStatus::Completed),
                        parent_call_id: "c_b".to_owned(),
                        child_session_id: "cs_b".to_owned(),
                        description: "Explore B".to_owned(),
                        subagent_kind: "explore".to_owned(),
                        resumed: false,
                        awake_ms: 2_000,
                        pending_approval: None,
                        last_answer: Some("B done".to_owned()),
                        brief: None,
                        full_report: None,
                        tool_calls: Vec::new(),
                    },
                ],
            }],
            subagent_counts: BoardSubagentCounts {
                total: 2,
                completed: 2,
                ..BoardSubagentCounts::default()
            },
            task_graph: Vec::new(),
            user_prompt: Some("Test drag".to_owned()),
            reasoning_preview: None,
            full_answer: Some("Done.".to_owned()),
        };

        let (modal, visual) = cx.add_window_view(WorkflowMonitorModal::new);
        visual.run_until_parked();

        let initial_sub_a_pos = visual.update(|window, cx| {
            modal.update(cx, |m, cx| {
                m.open = true;
                m.session_id = "as_drag".to_owned();
                m.run_id = Some("run_drag".to_owned());
                m.topology = Some(topo.clone());
                m.selected_node_id = None;
                m.user_cleared_selection = true;
                assert_eq!(m.effective_tab(&topo), (WorkflowTab::Graph, true));
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear(cx);

            let layout = build_modal_graph_layout(modal.read(cx), &topo, cx);
            let node_a = layout
                .nodes
                .iter()
                .find(|n| n.node.id == "sub:sub_a")
                .expect("sub:sub_a node in graph");
            (node_a.x, node_a.y)
        });

        // 1. Plain click (mouse down + up without moving) on `sub:sub_a`:
        //    selects node & opens inspector, then moving mouse with no button pressed
        //    must NOT move or pin the node.
        let sub_a_center = visual
            .debug_bounds("wf-gnode-sub:sub_a")
            .expect("wf-gnode-sub:sub_a bounds")
            .center();
        visual.simulate_mouse_down(sub_a_center, MouseButton::Left, Modifiers::none());
        visual.simulate_mouse_up(sub_a_center, MouseButton::Left, Modifiers::none());

        visual.update(|window, cx| {
            let m = modal.read(cx);
            assert_eq!(m.selected_node_id.as_deref(), Some("sub:sub_a"));
            assert!(
                m.drag_state.is_none(),
                "drag_state must be cleared on mouse_up"
            );
            assert!(m.pinned_positions.is_empty());
            window.refresh();
            window.draw(cx).clear(cx);
        });

        visual.simulate_mouse_move(
            sub_a_center + point(px(72.0), px(48.0)),
            None,
            Modifiers::none(),
        );
        visual.update(|window, cx| {
            let m = modal.read(cx);
            assert!(
                m.pinned_positions.is_empty(),
                "unpressed mouse move after click must not reposition the node"
            );
            assert!(m.drag_state.is_none());
            let layout = build_modal_graph_layout(m, &topo, cx);
            let node_a = layout
                .nodes
                .iter()
                .find(|n| n.node.id == "sub:sub_a")
                .expect("sub:sub_a node in graph");
            assert_eq!((node_a.x, node_a.y), initial_sub_a_pos);
            window.refresh();
            window.draw(cx).clear(cx);
        });

        // 2. Active drag (mouse down + move > 4px threshold + mouse up) on `sub:sub_b`:
        //    moves `sub:sub_b` and keeps selection on `sub:sub_a` unchanged.
        let initial_sub_b_pos = visual.update(|_window, cx| {
            let layout = build_modal_graph_layout(modal.read(cx), &topo, cx);
            let node_b = layout
                .nodes
                .iter()
                .find(|n| n.node.id == "sub:sub_b")
                .expect("sub:sub_b node in graph");
            (node_b.x, node_b.y)
        });
        let sub_b_center = visual
            .debug_bounds("wf-gnode-sub:sub_b")
            .expect("wf-gnode-sub:sub_b bounds")
            .center();
        let drag_target = sub_b_center + point(px(56.0), px(36.0));

        visual.simulate_mouse_down(sub_b_center, MouseButton::Left, Modifiers::none());
        visual.simulate_mouse_move(drag_target, MouseButton::Left, Modifiers::none());
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        visual.simulate_mouse_up(drag_target, MouseButton::Left, Modifiers::none());

        let pinned_sub_b = visual.update(|window, cx| {
            let m = modal.read(cx);
            assert!(
                m.drag_state.is_none(),
                "drag_state must be cleared after drag mouse_up"
            );
            assert_eq!(
                m.selected_node_id.as_deref(),
                Some("sub:sub_a"),
                "dragging sub:sub_b must not change node selection"
            );
            let pinned = *m
                .pinned_positions
                .get("sub:sub_b")
                .expect("sub:sub_b must be pinned at dragged position");
            assert_ne!(pinned, initial_sub_b_pos);
            window.refresh();
            window.draw(cx).clear(cx);
            pinned
        });

        // Moving the mouse with no button after drag release must not move `sub:sub_b` further.
        visual.simulate_mouse_move(
            drag_target + point(px(64.0), px(40.0)),
            None,
            Modifiers::none(),
        );
        visual.read(|cx| {
            let m = modal.read(cx);
            assert_eq!(
                m.pinned_positions.get("sub:sub_b").copied(),
                Some(pinned_sub_b)
            );
            assert_eq!(m.selected_node_id.as_deref(), Some("sub:sub_a"));
        });

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn flow_graph_step_click_and_drag_discrimination(cx: &mut TestAppContext) {
        use crate::views::flows::FlowsView;
        use gpui_kit::{Modifiers, MouseButton, point, px};
        use spanbay_desktop::core::app::factory_service::{flow_new, flow_step_new};
        use spanbay_desktop::state::FlowViewMode;

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());
        workspace_create_impl(rt.state(), "ws-flow-drag".into(), "Flow Drag WS".into())
            .expect("create workspace");
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
        });

        let (view, visual) = cx.add_window_view(|window, cx| FlowsView::new(None, window, cx));
        visual.run_until_parked();

        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                let mut flow = flow_new(rt.state(), "ws-flow-drag").expect("create flow");
                let mut s1 = flow_step_new(&flow);
                s1.id = "s1".to_owned();
                s1.name = "Auth".to_owned();
                s1.position = 0;
                let mut s2 = flow_step_new(&flow);
                s2.id = "s2".to_owned();
                s2.name = "Verify".to_owned();
                s2.position = 1;
                flow.steps = vec![s1, s2];
                v.draft = Some(flow);
                v.view_mode = FlowViewMode::Graph;
                v.selected_step_id = None;
                v.inspector_open = false;
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear(cx);
        });

        // 1. Plain click on `s1`: selects step, clears drag_state; subsequent unpressed move
        //    does not pin or reposition `s1`.
        let s1_center = visual
            .debug_bounds("flow-gnode-s1")
            .expect("flow-gnode-s1 bounds")
            .center();
        visual.simulate_mouse_down(s1_center, MouseButton::Left, Modifiers::none());
        visual.simulate_mouse_up(s1_center, MouseButton::Left, Modifiers::none());

        visual.update(|window, cx| {
            let v = view.read(cx);
            assert_eq!(v.selected_step_id.as_deref(), Some("s1"));
            assert!(v.drag_state.is_none());
            assert!(
                v.draft
                    .as_ref()
                    .is_some_and(|f| f.options.pinned_positions.is_empty())
            );
            window.refresh();
            window.draw(cx).clear(cx);
        });

        visual.simulate_mouse_move(
            s1_center + point(px(64.0), px(40.0)),
            None,
            Modifiers::none(),
        );
        visual.update(|window, cx| {
            let v = view.read(cx);
            assert!(
                v.draft
                    .as_ref()
                    .is_some_and(|f| f.options.pinned_positions.is_empty())
            );
            assert!(v.drag_state.is_none());
            window.refresh();
            window.draw(cx).clear(cx);
        });

        // 2. Drag `s2` beyond threshold: pins `s2` without changing selection from `s1`.
        let s2_center = visual
            .debug_bounds("flow-gnode-s2")
            .expect("flow-gnode-s2 bounds")
            .center();
        let drag_target = s2_center + point(px(52.0), px(36.0));
        visual.simulate_mouse_down(s2_center, MouseButton::Left, Modifiers::none());
        visual.simulate_mouse_move(drag_target, MouseButton::Left, Modifiers::none());
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        visual.simulate_mouse_up(drag_target, MouseButton::Left, Modifiers::none());

        visual.read(|cx| {
            let v = view.read(cx);
            assert!(v.drag_state.is_none());
            assert_eq!(v.selected_step_id.as_deref(), Some("s1"));
            assert!(
                v.draft
                    .as_ref()
                    .is_some_and(|f| f.options.pinned_positions.contains_key("s2"))
            );
        });

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn window_close_guard_checks_unsaved_edits_and_running_services_before_quitting(
        cx: &mut TestAppContext,
    ) {
        use std::sync::atomic::Ordering;

        use spanbay_desktop::core::app::factory_service::mock_server_new;
        use spanbay_desktop::core::app::{mock_server_save_impl, mock_server_start_impl};
        use spanbay_desktop::core::model::settings::Language;

        use crate::ui::apply_language;

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let _ = workspace_create_impl(
            rt.state(),
            "ws-quit-guard".into(),
            "Quit Guard Workspace".into(),
        );
        let settings = settings_get_impl(rt.state());
        let (host, rx) = GpuiHostWindow::pair();

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Slate, false, false, &settings.appearance, cx);
            apply_language(Language::ZhCn, cx);
        });

        let mut shell_slot = None;
        let (_root, visual) = cx.add_window_view(|window, cx| {
            let s = cx.new(|cx| AppShell::new(rx, host, window, cx));
            shell_slot = Some(s.clone());
            Root::new(s, window, cx)
        });
        let shell = shell_slot.expect("mounted AppShell");
        visual.run_until_parked();

        // 1. Idle & clean: close check passes immediately and arms allow_close.
        visual.update(|window, cx| {
            let allowed = shell.update(cx, |s, cx| s.handle_window_close_request(window, cx));
            assert!(allowed, "idle close should proceed without confirmation");
            assert!(!shell.read(cx).is_quit_confirm_open());
            assert!(shell.read(cx).allow_close_flag().load(Ordering::SeqCst));
            // Reset allow_close for subsequent blocker scenarios in this same window.
            shell
                .read(cx)
                .allow_close_flag()
                .store(false, Ordering::SeqCst);
        });

        // 2. Unsaved environment edits: close check blocks with destructive modal, Esc/cancel
        //    dismisses without quitting, and rendering succeeds in both zh-CN and en.
        visual.update(|window, cx| {
            shell.update(cx, |s, cx| s.navigate(NavModule::Environments, cx));
            window.refresh();
            window.draw(cx).clear(cx);
        });
        visual.run_until_parked();

        visual.update(|window, cx| {
            shell.update(cx, |s, cx| {
                let env = s
                    .modules
                    .environments
                    .as_ref()
                    .expect("mounted EnvironmentsView");
                env.update(cx, |e, _| {
                    e.env_dirty = true;
                    e.env_draft_name = "Production".to_owned();
                });
            });
            let allowed = shell.update(cx, |s, cx| s.handle_window_close_request(window, cx));
            assert!(!allowed, "unsaved environment must block silent close");
            assert!(shell.read(cx).is_quit_confirm_open());
            assert!(
                shell
                    .read(cx)
                    .quit_confirm
                    .as_ref()
                    .expect("quit_confirm state")
                    .is_destructive()
            );
            assert!(!shell.read(cx).allow_close_flag().load(Ordering::SeqCst));
            window.refresh();
            window.draw(cx).clear(cx);

            apply_language(Language::En, cx);
            window.refresh();
            window.draw(cx).clear(cx);

            // Canceling dismisses modal and leaves allow_close unarmed.
            assert!(shell.update(cx, |s, cx| s.dismiss_quit_confirm(cx)));
            assert!(!shell.read(cx).is_quit_confirm_open());
            assert!(!shell.read(cx).allow_close_flag().load(Ordering::SeqCst));

            // Clear dirty flag so the next case tests resident services only.
            shell.update(cx, |s, cx| {
                let env = s
                    .modules
                    .environments
                    .as_ref()
                    .expect("mounted EnvironmentsView");
                env.update(cx, |e, _| {
                    e.env_dirty = false;
                });
            });
        });

        // 3. Resident service only (running Mock server): close check blocks with non-destructive
        //    service confirmation; confirming arms allow_close and clears the modal.
        let srv = mock_server_new(rt.state(), "ws-quit-guard").expect("new mock server");
        let srv_id = srv.id.clone();
        mock_server_save_impl(rt.state(), srv).expect("save mock server");
        mock_server_start_impl(rt.state(), "ws-quit-guard".into(), srv_id)
            .expect("start mock server");

        visual.update(|window, cx| {
            let allowed = shell.update(cx, |s, cx| s.handle_window_close_request(window, cx));
            assert!(!allowed, "running mock server must prompt before quitting");
            let state = shell
                .read(cx)
                .quit_confirm
                .as_ref()
                .expect("quit_confirm for running mock");
            assert!(!state.is_destructive());
            assert_eq!(state.running_work.mock_servers, 1);
            window.refresh();
            window.draw(cx).clear(cx);

            shell.update(cx, |s, cx| s.confirm_quit(cx));
            assert!(!shell.read(cx).is_quit_confirm_open());
            assert!(shell.read(cx).allow_close_flag().load(Ordering::SeqCst));
        });

        rt.shutdown_blocking();
    }
}
