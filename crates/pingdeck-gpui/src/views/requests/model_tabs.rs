//! Per-tab state containers (`RequestTabState`, `RunnerTabState`,
//! `ContainerSettingsTabState`, `OpenTab`) and modal specs for `RequestsView`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use pingdeck_desktop::core::app::request_model_service::EffectiveAuth;
use pingdeck_desktop::core::app::{
    AiCompareEstimateDto, AiCompareResultDto, AiCompareRow, EnvironmentVariableDto, RunnerItem,
    RunnerResult, SaveRequestInput, TokenInfoDto,
};
use pingdeck_desktop::core::executor::ExecutionResult;
use pingdeck_desktop::core::llm::ai_run::AiResultDto;
use pingdeck_desktop::core::llm::capture_record::CapturedFrame;
use pingdeck_desktop::core::model::ai_request::RequestMode;
use pingdeck_desktop::core::model::script_result::ConsoleLevel;
use pingdeck_desktop::core::model::{AuthConfig, Request, RequestBodyFileValidationResult};
use pingdeck_desktop::core::response_viewer::ResponseBodyFormat;
use pingdeck_desktop::state::SettingsSection;

use super::model::{
    AiEdTab, AiRespTab, HttpEdTab, HttpRespTab, NewItemKind, ScriptSubTab,
    parse_ai_builder_from_request,
};

/// Structured message card for the AI API request builder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiMessageDraft {
    pub role: String,
    pub content: String,
}

/// Structured sampling & logging parameters for the AI API request builder.
#[derive(Debug, Clone, PartialEq)]
pub struct AiParamsDraft {
    pub temperature: String,
    pub max_tokens: String,
    pub top_p: String,
    pub stream: bool,
    pub reasoning_effort: String,
    pub keep_exchange: bool,
}

impl Default for AiParamsDraft {
    fn default() -> Self {
        Self {
            temperature: "0.2".into(),
            max_tokens: "1024".into(),
            top_p: String::new(),
            stream: true,
            reasoning_effort: String::new(),
            keep_exchange: true,
        }
    }
}

/// Structured failure card shown inside a request tab when sending fails or stops before response.
#[derive(Debug, Clone)]
pub struct RequestFailureInfo {
    pub kind_label: String,
    pub message: String,
    pub attempt_count: Option<u32>,
    pub connect_phase: Option<bool>,
    pub unresolved_keys: Vec<String>,
    pub history_unsaved: bool,
    pub is_cancelled: bool,
}

/// Complete isolated state of one open request tab (HTTP / GraphQL / AI API).
#[derive(Debug, Clone)]
pub struct RequestTabState {
    pub tab_id: String,
    pub saved_request_id: Option<String>,
    pub draft: Request,
    pub saved_snapshot: Option<Request>,
    pub revision: Option<String>,
    pub dirty: bool,
    pub http_ed_tab: HttpEdTab,
    pub http_resp_tab: HttpRespTab,
    pub ai_ed_tab: AiEdTab,
    pub ai_resp_tab: AiRespTab,
    pub script_sub: ScriptSubTab,
    pub docs_preview: bool,
    pub expanded_example_idx: Option<usize>,
    pub ai_messages: Vec<AiMessageDraft>,
    pub ai_params: AiParamsDraft,
    pub ai_tools_json: String,
    pub effective_auth: Option<EffectiveAuth>,
    pub binary_validation: Option<RequestBodyFileValidationResult>,
    pub oauth_token_info: Option<TokenInfoDto>,
    pub oauth_busy: bool,
    pub sending: bool,
    pub cancel_token: Option<String>,
    pub last_http: Option<ExecutionResult>,
    pub last_http_at_ms: Option<i64>,
    pub http_pretty: bool,
    pub http_soft_wrap: bool,
    pub http_format: ResponseBodyFormat,
    pub http_formatted_text: Option<String>,
    pub http_format_mode: Option<String>,
    pub http_jsonpath_query: String,
    pub http_jsonpath_result: Option<(bool, String, String)>,
    pub wire_popover_open: bool,
    pub last_ai: Option<AiResultDto>,
    pub ai_live_frames: Vec<CapturedFrame>,
    pub ai_selected_frame: Option<usize>,
    pub ai_reasoning_open: bool,
    pub ai_compare_result: Option<AiCompareResultDto>,
    pub ai_compare_live_rows: Vec<AiCompareRow>,
    pub ai_compare_running: bool,
    pub failure: Option<RequestFailureInfo>,
}

impl RequestTabState {
    #[must_use]
    pub fn from_request(tab_id: String, req: Request, saved: bool) -> Self {
        let (ai_messages, ai_params, ai_tools_json) = parse_ai_builder_from_request(&req);
        let revision = req.revision.clone();
        let saved_request_id = saved.then(|| req.id.as_str().to_owned());
        let saved_snapshot = saved.then(|| req.clone());
        Self {
            tab_id,
            saved_request_id,
            draft: req,
            saved_snapshot,
            revision,
            dirty: !saved,
            http_ed_tab: HttpEdTab::Params,
            http_resp_tab: HttpRespTab::Body,
            ai_ed_tab: AiEdTab::Messages,
            ai_resp_tab: AiRespTab::Merged,
            script_sub: ScriptSubTab::Post,
            docs_preview: false,
            expanded_example_idx: None,
            ai_messages,
            ai_params,
            ai_tools_json,
            effective_auth: None,
            binary_validation: None,
            oauth_token_info: None,
            oauth_busy: false,
            sending: false,
            cancel_token: None,
            last_http: None,
            last_http_at_ms: None,
            http_pretty: true,
            http_soft_wrap: true,
            http_format: ResponseBodyFormat::Json,
            http_formatted_text: None,
            http_format_mode: None,
            http_jsonpath_query: String::new(),
            http_jsonpath_result: None,
            wire_popover_open: false,
            last_ai: None,
            ai_live_frames: Vec::new(),
            ai_selected_frame: None,
            ai_reasoning_open: false,
            ai_compare_result: None,
            ai_compare_live_rows: Vec::new(),
            ai_compare_running: false,
            failure: None,
        }
    }

    /// Recomputes `dirty` against `saved_snapshot`.
    pub fn recompute_dirty(&mut self) {
        self.dirty = match &self.saved_snapshot {
            None => true,
            Some(snap) => {
                snap.name != self.draft.name
                    || snap.method != self.draft.method
                    || snap.url != self.draft.url
                    || snap.protocol != self.draft.protocol
                    || snap.mode != self.draft.mode
                    || snap.ai != self.draft.ai
                    || snap.query_params != self.draft.query_params
                    || snap.path_variables != self.draft.path_variables
                    || snap.headers != self.draft.headers
                    || snap.auth != self.draft.auth
                    || snap.body != self.draft.body
                    || snap.network_options != self.draft.network_options
                    || snap.assertions != self.draft.assertions
                    || snap.description != self.draft.description
                    || snap.pre_request_script != self.draft.pre_request_script
                    || snap.test_script != self.draft.test_script
                    || snap.examples != self.draft.examples
            }
        };
    }

    #[must_use]
    pub fn is_ai(&self) -> bool {
        self.draft.mode == RequestMode::AiApi
    }

    #[must_use]
    pub fn to_save_input(&self) -> SaveRequestInput {
        let req = &self.draft;
        SaveRequestInput {
            id: req.id.as_str().to_owned(),
            workspace_id: req.workspace_id.as_str().to_owned(),
            collection_id: req.collection_id.as_ref().map(|c| c.as_str().to_owned()),
            folder_id: req.folder_id.as_ref().map(|f| f.as_str().to_owned()),
            name: if req.name.trim().is_empty() {
                if req.url.trim().is_empty() {
                    "Untitled Request".into()
                } else {
                    req.url.trim().to_owned()
                }
            } else {
                req.name.clone()
            },
            protocol: req.protocol,
            method: req.method,
            url: req.url.clone(),
            query_params: req.query_params.clone(),
            path_variables: req.path_variables.clone(),
            headers: req.headers.clone(),
            auth: req.auth.clone(),
            body: req.body.clone(),
            network_options: req.network_options.clone(),
            assertions: req.assertions.clone(),
            description: req.description.clone(),
            pre_request_script: req.pre_request_script.clone(),
            test_script: req.test_script.clone(),
            position: req.position,
            mode: req.mode,
            ai: req.ai.clone(),
            examples: Some(req.examples.clone()),
        }
    }
}

/// Collection / Folder Runner tab hosted inside the Requests module (`docs/product.md §2`).
#[derive(Debug, Clone)]
pub struct RunnerTabState {
    pub tab_id: String,
    pub collection_id: String,
    pub collection_name: String,
    pub folder_id: Option<String>,
    pub folder_name: Option<String>,
    pub environment_id: Option<String>,
    pub iterations: u32,
    pub delay_ms: u64,
    pub stop_on_error: bool,
    pub selected_request_ids: BTreeSet<String>,
    pub running: bool,
    pub cancel_token: Option<String>,
    pub live_items: Vec<RunnerItem>,
    pub result: Option<RunnerResult>,
    pub results_page: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContainerSection {
    #[default]
    General,
    Auth,
    Variables,
    Scripts,
}

impl ContainerSection {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::General => 0,
            Self::Auth => 1,
            Self::Variables => 2,
            Self::Scripts => 3,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Auth,
            2 => Self::Variables,
            3 => Self::Scripts,
            _ => Self::General,
        }
    }
}

/// Editable Collection or Folder settings tab (auth, variables, pre/post scripts).
#[derive(Debug, Clone)]
pub struct ContainerSettingsTabState {
    pub tab_id: String,
    pub is_collection: bool,
    pub id: String,
    pub collection_id: String,
    pub parent_folder_id: Option<String>,
    pub name: String,
    pub description: String,
    pub auth: AuthConfig,
    pub variables: Vec<EnvironmentVariableDto>,
    pub pending_var_values: Vec<Option<String>>,
    pub pre_request_script: String,
    pub test_script: String,
    pub position: i64,
    pub revision: Option<String>,
    pub section: ContainerSection,
    pub script_sub: ScriptSubTab,
    pub dirty: bool,
    pub saving: bool,
}

/// Any open document tab in the Requests module tab bar.
#[derive(Debug, Clone)]
pub enum OpenTab {
    Request(Box<RequestTabState>),
    Runner(Box<RunnerTabState>),
    Container(Box<ContainerSettingsTabState>),
}

impl OpenTab {
    #[must_use]
    pub fn tab_id(&self) -> &str {
        match self {
            Self::Request(t) => &t.tab_id,
            Self::Runner(t) => &t.tab_id,
            Self::Container(t) => &t.tab_id,
        }
    }

    #[must_use]
    pub fn title(&self, lang: pingdeck_desktop::core::model::settings::Language) -> String {
        let r = crate::ui::Tr(lang).requests();
        match self {
            Self::Request(t) => {
                if t.draft.name.trim().is_empty() {
                    r.model_tabs_untitled().into()
                } else {
                    t.draft.name.clone()
                }
            }
            Self::Runner(t) => {
                let target = t.folder_name.as_deref().unwrap_or(&t.collection_name);
                r.model_tabs_run(target)
            }
            Self::Container(t) => r.model_tabs_settings(&t.name),
        }
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        match self {
            Self::Request(t) => t.dirty,
            Self::Runner(_) => false,
            Self::Container(t) => t.dirty,
        }
    }
}

/// Chronological console entry shown in the collapsible `⌘J` drawer.
#[derive(Debug, Clone)]
pub struct ConsoleRecord {
    pub ts_ms: i64,
    pub tag: &'static str,
    pub level: ConsoleLevel,
    pub summary: String,
    pub detail: Option<String>,
}

/// Cross-module navigation or handoff emitted by `RequestsView` and consumed by `AppShell`.
#[derive(Debug, Clone)]
pub enum RequestsHandoff {
    OpenSettings(SettingsSection),
    OpenEnvironments,
    OpenHistory,
    OpenUsageCall(String),
    OpenImportExport {
        export_collection_id: Option<String>,
    },
    AskAgent {
        title: String,
        context_markdown: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeItemKind {
    Collection,
    Folder,
    Request,
}

/// Active modal dialog owned by `RequestsView`.
#[derive(Debug, Clone)]
pub enum ActiveModal {
    NewItem {
        kind: NewItemKind,
        target_collection_id: Option<String>,
        target_folder_id: Option<String>,
    },
    CodeSnippets {
        values_mode: bool,
    },
    SaveAs {
        target_collection_id: Option<String>,
        target_folder_id: Option<String>,
    },
    Rename {
        kind: TreeItemKind,
        id: String,
    },
    MoveRequest {
        request_id: String,
        target_collection_id: Option<String>,
        target_folder_id: Option<String>,
    },
    ExtractVariable {
        value: String,
        is_secret: bool,
        use_globals: bool,
    },
    ConfirmCloseDirty {
        tab_id: String,
        name: String,
    },
    ConfirmGuard {
        tab_id: String,
        message: String,
        is_ai: bool,
        is_runner: bool,
        save_to_file: Option<PathBuf>,
    },
    StaleWriteConfirm {
        tab_id: String,
        name: String,
    },
    CompareModels {
        tab_id: String,
        selected: Vec<(String, String)>,
        concurrency: u8,
        estimate: Option<AiCompareEstimateDto>,
        error: Option<String>,
    },
}
