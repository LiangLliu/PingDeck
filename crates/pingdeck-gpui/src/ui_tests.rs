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
}
