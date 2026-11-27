#![forbid(unsafe_code)]

//! The vocabulary every SpanBay crate shares, and the pure rules over it.
//!
//! No I/O, no `tokio`, no `reqwest`, no `rusqlite`: everything here is data plus the
//! functions that transform it. That is what lets the store, the executor, the LLM layer
//! and the app all name the same types without depending on each other.
//!
//! Dependency direction: this crate depends only on [`spanbay_base`] and
//! [`spanbay_response`]. Everything above it may depend on it.
//!
//! | module | what it is |
//! |---|---|
//! | [`models`], [`items`], [`variables`], [`validation`], [`hierarchy`], [`ai_request`], [`history_filter`] | the SpanBay domain: requests, collections, environments, variables and their rules |
//! | [`credentials`] | where a request carries a credential: the one position rule set of every outlet |
//! | [`agent_target`], [`runner_item`], [`assertion`], [`script_result`] | what a run reports |
//! | [`events`] | the push-event sink and its event payloads |
//! | [`settings`] | the settings sections, their hub, their registry and their limits |
//! | [`wire`], [`hub`], [`capabilities`], [`catalog_entry`], [`model_catalog`], [`phase`] | the LLM layer's wire types, the model catalog and its pricing, and the model-chat representation |
//! | [`stored_setting`] | the one row shape the settings hub reads |

pub mod agent;
pub mod agent_target;
pub mod ai_request;
pub mod assertion;
pub mod capabilities;
pub mod catalog_entry;
pub mod credentials;
pub mod error_model;
pub mod events;
pub mod hierarchy;
pub mod history_filter;
pub mod hub;
pub mod items;
pub mod model_catalog;
pub mod models;
pub mod phase;
pub mod runner_item;
pub mod script_result;
pub mod settings;
pub mod stored_setting;
pub mod tokens;
pub mod validation;
pub mod variables;
pub mod wire;

pub use assertion::{AiResultView, AssertionResult};
pub use events::{EventSink, Sequenced};
pub use hierarchy::{
    CollectionSummary, FolderMoveResult, FolderSummary, RequestSummary, URL_SUMMARY_MAX_CHARS,
    WorkspaceCounts, WorkspaceTree, effective_auth, folder_chain,
};
pub use history_filter::{HistoryCursor, HistoryFilter, HistoryRunSummary};
pub use items::{Flow, FlowRunResult, FlowRunSummary, MockServer, MockTransactionLog};
pub use models::{
    ApiKeyLocation, Assertion, AuthConfig, Collection, CollectionId, CookieJarEntry,
    CookieJarEntryId, CookieJarExport, CookieJarExportSaveResult, CookieJarImport,
    CookieJarImportOpenFileResult, CookieJarImportResult, Environment, EnvironmentId,
    EnvironmentVariable, ExecutionHistoryEntry, ExecutionHistoryResultKind, Folder, FolderId,
    FormPart, HistoryCheckSummary, HistoryEntryId, HistoryOrigin, HistoryOriginRef, HistorySource,
    HttpMethod, KeyValue, NetworkField, NetworkOptions, OAuth2ClientAuth, OAuth2GrantType,
    ProtocolKind, RawLanguage, Request, RequestBody, RequestBodyFilePickResult,
    RequestBodyFileValidationResult, RequestId, TlsCertificate, Workspace, WorkspaceId,
};
pub use models::{
    HISTORY_BODY_PREVIEW_LIMIT_BYTES, RequestBodyFileValidationStatus, VariableResolutionSource,
};
pub use runner_item::{CollectionRunStatus, CollectionRunSummary, RunnerItem};
pub use script_result::{ConsoleEntry, ConsoleLevel, ScriptError, ScriptTestResult};
pub use settings::{
    Settings, SettingsEvent, SettingsHub, SettingsSection, ToolApproval,
    limits::{CAPTURE_MAX_BYTES, CAPTURE_MAX_DAYS, CaptureMode, LOG_MAX_DAYS, LOG_MAX_ROWS},
    policy::{SettingsSecret, local_only_fields},
};
pub use stored_setting::StoredSetting;
pub use variables::{
    VariableResolutionError, VariableScopes, dynamic_variable_names, resolve_template,
    resolve_template_lenient,
};
