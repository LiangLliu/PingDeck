//! State container, sub-tab enums, modals, handoffs, and input hydration for `MockView`.

use std::cell::Cell;
use std::collections::HashMap;

use gpui_kit::{Context, Entity, Subscription, Task, Window};
use pingdeck_desktop::core::app::{EnvironmentDto, MockLiveDataDto, MockServerStatusDto};
use pingdeck_desktop::core::model::items::{
    MockCallback, MockDataBucket, MockResponse, MockRoute, MockRuleOperator, MockRuleTarget,
    MockServer, MockTransactionLog,
};
use pingdeck_desktop::core::model::settings::Language;

use crate::ui::{EditorState, InputState, Tone};

/// Detail sub-tabs inside a selected mock server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockSubTab {
    Routes,
    Data,
    Headers,
    Callbacks,
    Traffic,
    Proxy,
    Settings,
}

impl MockSubTab {
    pub const ALL: [Self; 7] = [
        Self::Routes,
        Self::Data,
        Self::Headers,
        Self::Callbacks,
        Self::Traffic,
        Self::Proxy,
        Self::Settings,
    ];

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Routes => 0,
            Self::Data => 1,
            Self::Headers => 2,
            Self::Callbacks => 3,
            Self::Traffic => 4,
            Self::Proxy => 5,
            Self::Settings => 6,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Data,
            2 => Self::Headers,
            3 => Self::Callbacks,
            4 => Self::Traffic,
            5 => Self::Proxy,
            6 => Self::Settings,
            _ => Self::Routes,
        }
    }

    #[must_use]
    pub const fn label(self, lang: Language) -> &'static str {
        let m = crate::ui::Tr(lang).mock();
        match self {
            Self::Routes => m.tab_routes(),
            Self::Data => m.tab_data(),
            Self::Headers => m.tab_headers(),
            Self::Callbacks => m.tab_callbacks(),
            Self::Traffic => m.tab_traffic(),
            Self::Proxy => m.tab_proxy(),
            Self::Settings => m.tab_settings(),
        }
    }
}

/// Supported mock import formats inside the Import modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockImportFormat {
    PingDeckJson,
    Mockoon,
    OpenApi,
}

/// Traffic outcome filter inside the Traffic sub-tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrafficFilter {
    All,
    Route,
    Proxy,
    NoMatch,
    Error,
}

/// Modal dialogs owned by `MockView`.
#[derive(Debug, Clone)]
pub enum MockModal {
    CreateServer,
    DeleteServer { id: String, name: String },
    Import { format: MockImportFormat },
    ExportOptions { include_credentials: bool },
}

/// Cross-module handoff signals emitted by `MockView` and polled by `AppShell`.
#[derive(Debug, Clone)]
pub enum MockHandoff {
    OpenTestRequest {
        name: String,
        method: String,
        url: String,
    },
}

/// Full stateful Mock server workbench view (`NavModule::Mock`).
pub struct MockView {
    pub workspace_id: Option<String>,
    pub lang: Language,
    pub servers: Vec<MockServer>,
    pub statuses: HashMap<String, MockServerStatusDto>,
    pub environments: Vec<EnvironmentDto>,
    pub selected_id: Option<String>,
    pub draft: Option<MockServer>,
    pub dirty: bool,
    pub active_tab: MockSubTab,

    // Sub-selections inside the active draft server
    pub selected_route_id: Option<String>,
    pub selected_response_id: Option<String>,
    pub selected_bucket_id: Option<String>,
    pub selected_callback_id: Option<String>,

    // Live runtime state & traffic
    pub logs: Vec<MockTransactionLog>,
    pub selected_log_id: Option<String>,
    pub traffic_filter: TrafficFilter,
    pub traffic_page: usize,
    pub log_sub_id: Option<u64>,
    pub log_stream_task: Option<Task<()>>,
    pub live_data: Option<MockLiveDataDto>,

    // Inline builder selectors
    pub new_rule_target: MockRuleTarget,
    pub new_rule_op: MockRuleOperator,
    pub new_rule_invert: bool,

    // Shared form inputs hydrated on selection changes
    pub search_input: Entity<InputState>,
    pub route_search_input: Entity<InputState>,
    pub new_srv_name_input: Entity<InputState>,
    pub new_srv_port_input: Entity<InputState>,
    pub srv_name_input: Entity<InputState>,
    pub srv_port_input: Entity<InputState>,
    pub srv_prefix_input: Entity<InputState>,
    pub srv_latency_input: Entity<InputState>,
    pub route_path_input: Entity<InputState>,
    pub route_desc_input: Entity<InputState>,
    pub route_crud_id_input: Entity<InputState>,
    pub resp_label_input: Entity<InputState>,
    pub resp_status_input: Entity<InputState>,
    pub resp_latency_input: Entity<InputState>,
    pub resp_body_editor: Entity<EditorState>,
    pub resp_file_input: Entity<InputState>,
    pub stream_interval_input: Entity<InputState>,
    pub stream_ev_name_input: Entity<InputState>,
    pub stream_ev_data_input: Entity<InputState>,
    pub kv_key_input: Entity<InputState>,
    pub kv_val_input: Entity<InputState>,
    pub proxy_res_kv_key: Entity<InputState>,
    pub proxy_res_kv_val: Entity<InputState>,
    pub rule_mod_input: Entity<InputState>,
    pub rule_val_input: Entity<InputState>,
    pub bucket_name_input: Entity<InputState>,
    pub bucket_doc_input: Entity<InputState>,
    pub bucket_json_editor: Entity<EditorState>,
    pub cb_name_input: Entity<InputState>,
    pub cb_url_input: Entity<InputState>,
    pub cb_delay_input: Entity<InputState>,
    pub cb_body_editor: Entity<EditorState>,
    pub proxy_url_input: Entity<InputState>,
    pub traffic_search_input: Entity<InputState>,
    pub import_editor: Entity<EditorState>,

    // Modal, feedback, sync flags, handoff
    pub try_it_busy: bool,
    pub try_it_result: Option<(u16, u64, String, String)>,
    pub active_modal: Option<MockModal>,
    pub status_msg: Option<(Tone, String)>,
    pub pending_sync: bool,
    pub syncing: bool,
    pub pending_handoff: Cell<Option<MockHandoff>>,
    pub(super) _subscriptions: Vec<Subscription>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl MockView {
    #[must_use]
    pub fn status_of(&self, server_id: &str) -> Option<&MockServerStatusDto> {
        self.statuses.get(server_id)
    }

    #[must_use]
    pub fn is_running(&self, server_id: &str) -> bool {
        self.statuses.get(server_id).is_some_and(|s| s.running)
    }

    #[must_use]
    pub fn selected_route(&self) -> Option<&MockRoute> {
        let srv = self.draft.as_ref()?;
        let rid = self.selected_route_id.as_deref()?;
        srv.routes.iter().find(|r| r.id == rid)
    }

    pub fn selected_route_mut(&mut self) -> Option<&mut MockRoute> {
        let rid = self.selected_route_id.clone()?;
        self.draft.as_mut()?.routes.iter_mut().find(|r| r.id == rid)
    }

    #[must_use]
    pub fn selected_response(&self) -> Option<&MockResponse> {
        let route = self.selected_route()?;
        let id = self.selected_response_id.as_deref()?;
        route.responses.iter().find(|r| r.id == id)
    }

    pub fn selected_response_mut(&mut self) -> Option<&mut MockResponse> {
        let id = self.selected_response_id.clone()?;
        self.selected_route_mut()?
            .responses
            .iter_mut()
            .find(|r| r.id == id)
    }

    #[must_use]
    pub fn selected_bucket(&self) -> Option<&MockDataBucket> {
        let srv = self.draft.as_ref()?;
        let id = self.selected_bucket_id.as_deref()?;
        srv.data_buckets.iter().find(|b| b.id == id)
    }

    pub fn selected_bucket_mut(&mut self) -> Option<&mut MockDataBucket> {
        let id = self.selected_bucket_id.clone()?;
        self.draft
            .as_mut()?
            .data_buckets
            .iter_mut()
            .find(|b| b.id == id)
    }

    #[must_use]
    pub fn selected_callback(&self) -> Option<&MockCallback> {
        let srv = self.draft.as_ref()?;
        let id = self.selected_callback_id.as_deref()?;
        srv.callbacks.iter().find(|c| c.id == id)
    }

    pub fn selected_callback_mut(&mut self) -> Option<&mut MockCallback> {
        let id = self.selected_callback_id.clone()?;
        self.draft
            .as_mut()?
            .callbacks
            .iter_mut()
            .find(|c| c.id == id)
    }

    /// Ensures sub-selection IDs point to valid items in `draft`.
    pub fn normalize_sub_selections(&mut self) {
        let Some(srv) = &self.draft else {
            self.selected_route_id = None;
            self.selected_response_id = None;
            self.selected_bucket_id = None;
            self.selected_callback_id = None;
            return;
        };
        if self
            .selected_route_id
            .as_deref()
            .is_none_or(|id| !srv.routes.iter().any(|r| r.id == id))
        {
            self.selected_route_id = srv.routes.first().map(|r| r.id.clone());
        }
        let route = self
            .selected_route_id
            .as_deref()
            .and_then(|id| srv.routes.iter().find(|r| r.id == id));
        if let Some(r) = route {
            if self
                .selected_response_id
                .as_deref()
                .is_none_or(|id| !r.responses.iter().any(|x| x.id == id))
            {
                self.selected_response_id = r
                    .responses
                    .iter()
                    .find(|x| x.is_default)
                    .or_else(|| r.responses.first())
                    .map(|x| x.id.clone());
            }
        } else {
            self.selected_response_id = None;
        }
        if self
            .selected_bucket_id
            .as_deref()
            .is_none_or(|id| !srv.data_buckets.iter().any(|b| b.id == id))
        {
            self.selected_bucket_id = srv.data_buckets.first().map(|b| b.id.clone());
        }
        if self
            .selected_callback_id
            .as_deref()
            .is_none_or(|id| !srv.callbacks.iter().any(|c| c.id == id))
        {
            self.selected_callback_id = srv.callbacks.first().map(|c| c.id.clone());
        }
    }

    /// Snapshots all active text/code widgets back into `self.draft` before switching selection or saving.
    pub fn snapshot_inputs(&mut self, cx: &Context<Self>) {
        if self.syncing {
            return;
        }
        let srv_name = self.srv_name_input.read(cx).value().to_string();
        let srv_port = self.srv_port_input.read(cx).value().to_string();
        let srv_prefix = self.srv_prefix_input.read(cx).value().to_string();
        let srv_latency = self.srv_latency_input.read(cx).value().to_string();
        let proxy_url = self.proxy_url_input.read(cx).value().to_string();
        let route_path = self.route_path_input.read(cx).value().to_string();
        let route_desc = self.route_desc_input.read(cx).value().to_string();
        let route_crud_id = self.route_crud_id_input.read(cx).value().to_string();
        let resp_label = self.resp_label_input.read(cx).value().to_string();
        let resp_status = self.resp_status_input.read(cx).value().to_string();
        let resp_latency = self.resp_latency_input.read(cx).value().to_string();
        let resp_body = self.resp_body_editor.read(cx).value().to_string();
        let resp_file = self.resp_file_input.read(cx).value().to_string();
        let stream_interval = self.stream_interval_input.read(cx).value().to_string();
        let bucket_name = self.bucket_name_input.read(cx).value().to_string();
        let bucket_doc = self.bucket_doc_input.read(cx).value().to_string();
        let bucket_json = self.bucket_json_editor.read(cx).value().to_string();
        let cb_name = self.cb_name_input.read(cx).value().to_string();
        let cb_url = self.cb_url_input.read(cx).value().to_string();
        let cb_delay = self.cb_delay_input.read(cx).value().to_string();
        let cb_body = self.cb_body_editor.read(cx).value().to_string();

        let sel_route = self.selected_route_id.clone();
        let sel_resp = self.selected_response_id.clone();
        let sel_bucket = self.selected_bucket_id.clone();
        let sel_cb = self.selected_callback_id.clone();

        let Some(srv) = self.draft.as_mut() else {
            return;
        };
        if !srv_name.trim().is_empty() {
            srv.name = srv_name;
        }
        if let Ok(port) = srv_port.trim().parse::<u16>()
            && port > 0
        {
            srv.port = port;
        }
        srv.endpoint_prefix = srv_prefix;
        if let Ok(lat) = srv_latency.trim().parse::<u64>() {
            srv.latency_ms = lat;
        }
        srv.proxy_base_url = proxy_url;

        if let Some(rid) = sel_route
            && let Some(route) = srv.routes.iter_mut().find(|r| r.id == rid)
        {
            route.path = route_path;
            route.description = route_desc;
            if !route_crud_id.trim().is_empty() {
                route.crud_id_key = route_crud_id;
            }
            if let Some(respid) = sel_resp
                && let Some(resp) = route.responses.iter_mut().find(|x| x.id == respid)
            {
                resp.label = resp_label;
                if let Ok(code) = resp_status.trim().parse::<u16>()
                    && (100..=599).contains(&code)
                {
                    resp.status_code = code;
                }
                if let Ok(lat) = resp_latency.trim().parse::<u64>() {
                    resp.latency_ms = lat;
                }
                if resp.body_source
                    == Some(pingdeck_desktop::core::model::items::MockBodySource::File)
                {
                    resp.body = resp_file;
                } else if resp.stream.is_none() {
                    resp.body = resp_body;
                }
                if let Some(stream) = resp.stream.as_mut()
                    && let Ok(ms) = stream_interval.trim().parse::<u64>()
                {
                    stream.interval_ms = ms;
                }
            }
        }

        if let Some(bid) = sel_bucket
            && let Some(bucket) = srv.data_buckets.iter_mut().find(|b| b.id == bid)
        {
            bucket.name = bucket_name;
            bucket.documentation = bucket_doc;
            bucket.value_json = bucket_json;
        }

        if let Some(cid) = sel_cb
            && let Some(cb) = srv.callbacks.iter_mut().find(|c| c.id == cid)
        {
            cb.name = cb_name;
            cb.url = cb_url;
            if let Ok(d) = cb_delay.trim().parse::<u64>() {
                cb.delay_ms = d;
            }
            cb.body = cb_body;
        }
    }

    /// Hydrates all shared input/editor widgets from the currently selected draft entities.
    pub fn sync_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_sync = false;
        self.normalize_sub_selections();
        let Some(srv) = self.draft.clone() else {
            return;
        };
        let route = self
            .selected_route_id
            .as_deref()
            .and_then(|id| srv.routes.iter().find(|r| r.id == id))
            .cloned();
        let resp = route.as_ref().and_then(|r| {
            self.selected_response_id
                .as_deref()
                .and_then(|id| r.responses.iter().find(|x| x.id == id))
                .cloned()
        });
        let bucket = self
            .selected_bucket_id
            .as_deref()
            .and_then(|id| srv.data_buckets.iter().find(|b| b.id == id))
            .cloned();
        let cb = self
            .selected_callback_id
            .as_deref()
            .and_then(|id| srv.callbacks.iter().find(|c| c.id == id))
            .cloned();

        self.syncing = true;
        self.srv_name_input
            .update(cx, |s, cx| s.set_value(srv.name, window, cx));
        self.srv_port_input
            .update(cx, |s, cx| s.set_value(srv.port.to_string(), window, cx));
        self.srv_prefix_input
            .update(cx, |s, cx| s.set_value(srv.endpoint_prefix, window, cx));
        self.srv_latency_input.update(cx, |s, cx| {
            s.set_value(srv.latency_ms.to_string(), window, cx)
        });
        self.proxy_url_input
            .update(cx, |s, cx| s.set_value(srv.proxy_base_url, window, cx));

        if let Some(r) = route {
            self.route_path_input
                .update(cx, |s, cx| s.set_value(r.path, window, cx));
            self.route_desc_input
                .update(cx, |s, cx| s.set_value(r.description, window, cx));
            self.route_crud_id_input
                .update(cx, |s, cx| s.set_value(r.crud_id_key, window, cx));
        }
        if let Some(rp) = resp {
            self.resp_label_input
                .update(cx, |s, cx| s.set_value(rp.label, window, cx));
            self.resp_status_input.update(cx, |s, cx| {
                s.set_value(rp.status_code.to_string(), window, cx)
            });
            self.resp_latency_input.update(cx, |s, cx| {
                s.set_value(rp.latency_ms.to_string(), window, cx)
            });
            let is_file =
                rp.body_source == Some(pingdeck_desktop::core::model::items::MockBodySource::File);
            self.resp_file_input.update(cx, |s, cx| {
                s.set_value(
                    if is_file {
                        rp.body.clone()
                    } else {
                        String::new()
                    },
                    window,
                    cx,
                );
            });
            self.resp_body_editor.update(cx, |s, cx| {
                s.set_value(if is_file { String::new() } else { rp.body }, window, cx);
            });
            let interval = rp
                .stream
                .as_ref()
                .map_or(200, |st| st.interval_ms)
                .to_string();
            self.stream_interval_input
                .update(cx, |s, cx| s.set_value(interval, window, cx));
        }
        if let Some(b) = bucket {
            self.bucket_name_input
                .update(cx, |s, cx| s.set_value(b.name, window, cx));
            self.bucket_doc_input
                .update(cx, |s, cx| s.set_value(b.documentation, window, cx));
            self.bucket_json_editor
                .update(cx, |s, cx| s.set_value(b.value_json, window, cx));
        }
        if let Some(c) = cb {
            self.cb_name_input
                .update(cx, |s, cx| s.set_value(c.name, window, cx));
            self.cb_url_input
                .update(cx, |s, cx| s.set_value(c.url, window, cx));
            self.cb_delay_input
                .update(cx, |s, cx| s.set_value(c.delay_ms.to_string(), window, cx));
            self.cb_body_editor
                .update(cx, |s, cx| s.set_value(c.body, window, cx));
        }
        self.syncing = false;
    }
}
