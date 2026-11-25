//! Widget initialization and input subscriptions for `RequestsView::new`.

use std::cell::Cell;
use std::collections::HashSet;

use gpui_kit::{AppContext as _, Context, Window};
use pingdeck_desktop::core::app::settings_get_impl;
use pingdeck_desktop::core::app::snippet_service::SnippetLanguage;

use crate::bridge::runtime;
use crate::ui::{EditorState, InputEvent, InputState, TextareaState};

use super::RequestsView;

impl RequestsView {
    pub fn new(workspace_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = settings_get_impl(runtime(cx).state());
        let mk_in = |ph: &'static str, w: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(w, cx).placeholder(ph))
        };
        let mk_ed =
            |lang: &'static str, init: &'static str, w: &mut Window, cx: &mut Context<Self>| {
                cx.new(|cx| {
                    let mut ed = EditorState::new(w, cx).default_value(init);
                    ed.set_highlighter(lang, cx);
                    ed
                })
            };

        let search_input = mk_in("筛选请求 / Filter requests…", window, cx);
        let name_input = mk_in("请求名称 / Request name", window, cx);
        let url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://api.example.com/v1/resource  ({{base_url}})")
                .default_value("https://httpbin.org/get")
        });
        let kv_key_input = mk_in("键 / Key", window, cx);
        let kv_val_input = mk_in("值 / Value", window, cx);
        let kv_desc_input = mk_in("说明 / Description", window, cx);
        let kv_file_input = mk_in("文件路径（可选）/ File path", window, cx);
        let auth_f1 = mk_in("Token / Username / Key / Access Key", window, cx);
        let auth_f2 = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Password / Value / Secret / Token URL")
                .masked(false)
        });
        let auth_f3 = mk_in("Client ID / Region", window, cx);
        let auth_f4 = mk_in("Client Secret / Service", window, cx);
        let auth_f5 = mk_in("Scope / Session Token", window, cx);
        let auth_f6 = mk_in("Authorization URL (PKCE)", window, cx);

        let body_editor = mk_ed("json", "", window, cx);
        let gql_vars_editor = mk_ed("json", "{}", window, cx);
        let binary_path_input = mk_in("/path/to/payload.bin", window, cx);
        let pre_script_editor = mk_ed(
            "javascript",
            "// Pre-request script (Boa sandbox)\n",
            window,
            cx,
        );
        let post_script_editor = mk_ed(
            "javascript",
            "// Post-response test script (pm.test / pm.expect)\n",
            window,
            cx,
        );
        let docs_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Markdown 接口文档说明…")
                .rows(6)
        });
        let net_timeout_input = mk_in("30000", window, cx);
        let net_total_timeout_input = mk_in("不限 / Off", window, cx);
        let net_retry_input = mk_in("0", window, cx);
        let net_max_redir_input = mk_in("10", window, cx);
        let net_proxy_input = mk_in("http://127.0.0.1:7890", window, cx);
        let assert_target_input = mk_in("$.data[0].id / Content-Type", window, cx);
        let assert_val_input = mk_in("期望值 / Expected", window, cx);
        let oauth_var_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("oauth_token")
                .default_value("oauth_token")
        });

        let ai_custom_url_input = mk_in("https://api.openai.com/v1/chat/completions", window, cx);
        let ai_custom_model_input = mk_in("gpt-4o-mini", window, cx);
        let ai_msg_new_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("输入消息内容，点击「+ 添加消息」追加到对话列表…")
                .rows(2)
        });
        let ai_temp_input = mk_in("0.2", window, cx);
        let ai_max_tok_input = mk_in("1024", window, cx);
        let ai_top_p_input = mk_in("1.0", window, cx);
        let ai_tools_editor = mk_ed("json", "", window, cx);
        let ai_raw_editor = mk_ed("json", "", window, cx);

        let response_editor = mk_ed(
            "json",
            "// 发送请求后在此查看实时响应 / Send a request to inspect the response",
            window,
            cx,
        );
        let resp_filter_input = mk_in("JSONPath（如 $.data[0].id）或筛选关键字…", window, cx);

        let runner_iter_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("1")
                .default_value("1")
        });
        let runner_delay_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("0")
                .default_value("0")
        });
        let container_name_input = mk_in("名称 / Name", window, cx);
        let container_desc_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("集合或文件夹说明（Markdown）…")
                .rows(4)
        });
        let container_var_key = mk_in("变量名 / Key", window, cx);
        let container_var_val = mk_in("变量值 / Value", window, cx);
        let container_pre_editor = mk_ed("javascript", "", window, cx);
        let container_post_editor = mk_ed("javascript", "", window, cx);

        let snippet_editor = mk_ed("bash", "", window, cx);
        let modal_name_input = mk_in("输入名称…", window, cx);
        let modal_text_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("粘贴 cURL 命令或 Postman / OpenAPI / HAR JSON…")
                .rows(8)
        });

        let mut subs = Vec::new();
        subs.push(cx.subscribe(&search_input, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        }));
        subs.push(cx.subscribe(&url_input, |this, _, ev: &InputEvent, cx| {
            if !this.hydrating && matches!(ev, InputEvent::Change) {
                this.on_url_input_changed(cx);
            }
        }));
        subs.push(
            cx.subscribe_in(&url_input, window, |this, _, ev, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.send_active_request(None, None, window, cx);
                }
            }),
        );
        subs.push(cx.subscribe(&name_input, |this, _, ev: &InputEvent, cx| {
            if !this.hydrating && matches!(ev, InputEvent::Change) {
                let name = this.name_input.read(cx).value().to_string();
                if let Some(tab) = this.active_req_mut() {
                    tab.draft.name = name;
                    tab.recompute_dirty();
                    cx.notify();
                }
            }
        }));
        subs.push(
            cx.subscribe_in(&resp_filter_input, window, |this, _, ev, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.apply_jsonpath_filter(window, cx);
                }
            }),
        );
        subs.push(cx.subscribe(&body_editor, |this, _, ev: &InputEvent, cx| {
            if !this.hydrating && matches!(ev, InputEvent::Change) {
                this.mark_active_dirty(cx);
            }
        }));
        subs.push(
            cx.subscribe(&pre_script_editor, |this, _, ev: &InputEvent, cx| {
                if !this.hydrating && matches!(ev, InputEvent::Change) {
                    this.mark_active_dirty(cx);
                }
            }),
        );
        subs.push(
            cx.subscribe(&post_script_editor, |this, _, ev: &InputEvent, cx| {
                if !this.hydrating && matches!(ev, InputEvent::Change) {
                    this.mark_active_dirty(cx);
                }
            }),
        );
        subs.push(
            cx.subscribe_in(&ai_raw_editor, window, |this, _, ev, _, cx| {
                if !this.hydrating && matches!(ev, InputEvent::Change) {
                    this.mark_active_dirty(cx);
                }
            }),
        );
        subs.push(
            cx.subscribe_in(&modal_name_input, window, |this, _, ev, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.submit_active_modal_on_enter(window, cx);
                }
            }),
        );

        let mut view = Self {
            workspace_id: workspace_id.clone(),
            settings,
            collections: Vec::new(),
            folders: Vec::new(),
            requests: Vec::new(),
            environments: Vec::new(),
            active_env_id: None,
            resolved_vars: Vec::new(),
            providers: Vec::new(),
            models: Vec::new(),
            collapsed_nodes: HashSet::new(),
            agent_touched_only: false,
            last_undo: None,
            tabs: Vec::new(),
            active_tab_id: None,
            search_input,
            name_input,
            url_input,
            kv_key_input,
            kv_val_input,
            kv_desc_input,
            kv_file_input,
            auth_f1,
            auth_f2,
            auth_f3,
            auth_f4,
            auth_f5,
            auth_f6,
            body_editor,
            gql_vars_editor,
            binary_path_input,
            pre_script_editor,
            post_script_editor,
            docs_input,
            net_timeout_input,
            net_total_timeout_input,
            net_retry_input,
            net_max_redir_input,
            net_proxy_input,
            assert_target_input,
            assert_val_input,
            assert_target_kind: 0,
            assert_op_idx: 0,
            oauth_var_input,
            ai_custom_url_input,
            ai_custom_model_input,
            ai_msg_new_input,
            ai_temp_input,
            ai_max_tok_input,
            ai_top_p_input,
            ai_tools_editor,
            ai_raw_editor,
            response_editor,
            resp_filter_input,
            runner_iter_input,
            runner_delay_input,
            container_name_input,
            container_desc_input,
            container_var_key,
            container_var_val,
            container_var_secret: false,
            container_pre_editor,
            container_post_editor,
            active_modal: None,
            snippet_lang: SnippetLanguage::Curl,
            snippet_editor,
            modal_name_input,
            modal_text_input,
            console_open: false,
            console_entries: Vec::new(),
            status_msg: None,
            pending_handoff: Cell::new(None),
            pending_hydrate: false,
            hydrating: false,
            _subscriptions: subs,
            _tasks: Vec::new(),
        };
        if let Some(ws) = workspace_id {
            view.reload_workspace(ws, true, cx);
        }
        view.reload_ai_catalog(cx);
        view
    }
}
