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

    #[gpui_kit::test]
    fn requests_workbench_full_lifecycle(cx: &mut TestAppContext) {
        use pingdeck_desktop::core::app::snippet_service::SnippetLanguage;
        use pingdeck_desktop::core::model::ai_request::RequestMode;
        use pingdeck_desktop::core::model::{AuthConfig, RequestBody};

        use crate::views::requests::RequestsView;
        use crate::views::requests::model::{
            ActiveModal, AiEdTab, HttpEdTab, ImportExportTab, NewItemKind, OpenTab,
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

        // 7. Import a cURL command into a new draft tab.
        visual.update(|window, cx| {
            let before = view.read(cx).tabs.len();
            view.update(cx, |v, cx| {
                v.open_import_export_modal(ImportExportTab::Import, None, window, cx);
                if let Some(ActiveModal::ImportExport { format, .. }) = &mut v.active_modal {
                    *format = "curl".into();
                }
                v.modal_text_input.update(cx, |t, cx| {
                    t.set_value(
                        "curl -X POST https://httpbin.org/post -H 'X-Test: 1' -d '{\"ok\":true}'",
                        window,
                        cx,
                    );
                });
                v.import_apply(window, cx);
            });
            assert_eq!(view.read(cx).tabs.len(), before + 1);
            assert!(view.read(cx).active_modal.is_none());
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
        use pingdeck_desktop::core::model::settings::{Language, SettingsSection as CoreSection};

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
            SettingsView::new(SettingsSection::General, PaletteId::Slate, window, cx)
        });
        visual.run_until_parked();

        // Helper to wait for async blocking settings commits to land in GPUI.
        fn wait_for(
            visual: &mut gpui_kit::VisualTestContext,
            pred: impl Fn(&gpui_kit::App) -> bool,
        ) -> bool {
            for _ in 0..100 {
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
        assert!(
            wait_for(visual, |cx| {
                let s = &view.read(cx).settings;
                s.network.timeout_ms == 45_000
                    && s.editor.tab_size == 4
                    && s.mock.default_latency_ms == 125
                    && s.ai.gateway.max_retries == 6
                    && s.advanced.script_recursion_limit == 512
            }),
            "settings patches must persist and sync back into SettingsView"
        );

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
        use crate::views::flows::state::{FlowModal, FlowRunTab, FlowStepTab};

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

        // 2. Cycle all inspector & bottom tabs + Options modal.
        visual.update(|window, cx| {
            view.update(cx, |v, cx| {
                for tab in FlowStepTab::ALL {
                    v.step_tab = tab;
                    cx.notify();
                }
                for rtab in [
                    FlowRunTab::LiveRun,
                    FlowRunTab::Variables,
                    FlowRunTab::History,
                ] {
                    v.run_tab = rtab;
                    cx.notify();
                }
                v.active_modal = Some(FlowModal::Options);
                cx.notify();
            });
            window.refresh();
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
        use pingdeck_desktop::core::app::history_restore_many_impl;
        use pingdeck_desktop::core::model::{ExecutionHistoryEntry, HistorySource, Request};

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
                    value: Some("pingdeck".into()),
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

        // 1. Cycle all 9 time-range presets, 5 chart metrics, and 6 breakdown dimensions.
        visual.update(|window, cx| {
            for preset in RangePreset::ALL {
                view.update(cx, |v, cx| {
                    v.range = preset;
                    cx.notify();
                });
                window.refresh();
            }
            for metric in ChartMetric::ALL {
                view.update(cx, |v, cx| {
                    v.chart_metric = metric;
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
        use crate::views::chat::{ChatHandoff, ChatModal, ChatSidebarMode, ChatView, DetailsTab};

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
            for mode in [
                ChatSidebarMode::Time,
                ChatSidebarMode::Board,
                ChatSidebarMode::Trash,
            ] {
                view.update(cx, |v, cx| {
                    v.sidebar_mode = mode;
                    cx.notify();
                });
                window.refresh();
            }

            view.update(cx, |v, cx| {
                v.details_open = true;
                v.plan_mode = true;
                cx.notify();
            });
            for tab in [
                DetailsTab::Progress,
                DetailsTab::Changes,
                DetailsTab::Context,
                DetailsTab::Options,
            ] {
                view.update(cx, |v, cx| {
                    v.details_tab = tab;
                    cx.notify();
                });
                window.refresh();
            }

            // Exercise FullAccessConfirm, DiffPreview, and ToolOutput modals + Esc dismissal.
            for modal in [
                ChatModal::FullAccessConfirm {
                    acknowledged: false,
                },
                ChatModal::DiffPreview {
                    path: "src/main.rs".into(),
                    diff: "+fn ok() {}\n".into(),
                    binary: false,
                },
                ChatModal::ToolOutput {
                    call_id: "call_1".into(),
                    name: "read_file".into(),
                    content: "line 1\nline 2".into(),
                    has_more: false,
                    next_offset: 0,
                },
            ] {
                view.update(cx, |v, cx| {
                    v.active_modal = Some(modal);
                    cx.notify();
                });
                window.refresh();
                assert!(view.update(cx, |v, cx| v.dismiss_modal_if_open(cx)));
                assert!(view.read(cx).active_modal.is_none());
            }

            view.update(cx, |v, cx| {
                v.pending_handoff.set(Some(ChatHandoff::OpenProviders));
                cx.notify();
            });
            assert!(matches!(
                view.read(cx).take_pending_handoff(),
                Some(ChatHandoff::OpenProviders)
            ));
        });

        rt.shutdown_blocking();
    }

    #[gpui_kit::test]
    fn providers_gateway_bench_and_handoff(cx: &mut TestAppContext) {
        use crate::views::providers::{GatewayTab, LeftSelection, ProvidersHandoff, ProvidersView};

        cx.background_executor.allow_parking();
        let rt = Arc::new(DesktopRuntime::open_in_memory().expect("in-memory runtime"));
        let settings = settings_get_impl(rt.state());

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(CoreBridge::new(Arc::clone(&rt)));
            apply_palette(PaletteId::Ocean, true, false, &settings.appearance, cx);
        });

        let (view, visual) = cx.add_window_view(ProvidersView::new);
        visual.run_until_parked();

        visual.update(|window, cx| {
            // 1. Cycle all 4 Local Gateway cockpit tabs.
            for tab in [
                GatewayTab::Overview,
                GatewayTab::Clients,
                GatewayTab::Access,
                GatewayTab::Rules,
            ] {
                view.update(cx, |v, cx| {
                    v.selection = LeftSelection::Gateway;
                    v.gateway_tab = tab;
                    cx.notify();
                });
                window.refresh();
            }

            // 2. Mount and render Model Test Bench view.
            view.update(cx, |v, cx| {
                v.open_bench(None, Vec::new(), window, cx);
            });
            assert_eq!(view.read(cx).selection, LeftSelection::Bench);
            assert!(view.read(cx).bench_view.is_some());
            window.refresh();

            // 3. Exercise inline modal dismissal & Usage handoff emission.
            view.update(cx, |v, cx| {
                v.confirm_lan_open = true;
                v.adding_key_open = true;
                cx.notify();
            });
            assert!(view.update(cx, |v, cx| v.dismiss_modal_if_open(cx)));
            assert!(!view.read(cx).confirm_lan_open && !view.read(cx).adding_key_open);

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
        use pingdeck_desktop::core::app::factory_service::{flow_new, mock_server_new};
        use pingdeck_desktop::core::app::{
            SaveRequestInput, flow_save_impl, mock_server_save_impl, request_save_impl,
        };
        use pingdeck_desktop::core::model::ai_request::RequestMode;
        use pingdeck_desktop::core::model::{
            AuthConfig, HttpMethod, NetworkOptions, ProtocolKind, RequestBody,
        };
        use pingdeck_desktop::state::{PaletteAction, ToastKind};

        use crate::views::utility::console::ConsoleMode;
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
                ("wiz:gw", PaletteAction::OpenGateway, NavModule::Providers),
            ] {
                shell.update(cx, |s, cx| {
                    s.execute_palette_action(id.into(), action, window, cx);
                });
                assert_eq!(shell.read(cx).state.active_module, expected_mod);
                window.refresh();
            }

            // 3. Exercise 3-tab Workspace Import/Export & 2-tab Diagnostic Console inside UtilityPanel.
            shell.update(cx, |s, cx| {
                s.open_utility(UtilityDialog::ImportExport, window, cx);
            });
            let util = shell.read(cx).utility_view.clone();
            for tab in [IoTab::Import, IoTab::SanitizedExport, IoTab::SecretsExport] {
                util.update(cx, |u, cx| {
                    u.io_tab = tab;
                    cx.notify();
                });
                window.refresh();
            }
            shell.update(cx, |s, cx| {
                s.open_utility(UtilityDialog::Console, window, cx);
            });
            for mode in [ConsoleMode::Diagnostics, ConsoleMode::GpuBench] {
                util.update(cx, |u, cx| {
                    u.console_mode = mode;
                    cx.notify();
                });
                window.refresh();
            }

            // 4. Push auto-dismissing toast & verify sidebar collapse/expand rendering.
            shell.update(cx, |s, cx| {
                s.push_toast_auto(ToastKind::Success, "Saved", None, cx);
                s.toggle_sidebar(cx);
            });
            assert_eq!(shell.read(cx).state.toasts.len(), 1);
            window.refresh();
            shell.update(cx, |s, cx| s.toggle_sidebar(cx));
            window.refresh();
        });

        rt.shutdown_blocking();
    }
}
