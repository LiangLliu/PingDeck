//! One import path for the workspace domain inside the use-case crates: the vocabulary from
//! `spanbay_model`, the assertion engine from `spanbay_executor` and the store from
//! `spanbay_store`.

pub use spanbay_model::{
    ai_request, hierarchy, history_filter, items, models, validation, variables,
};

pub use spanbay_executor::assertions;

pub use spanbay_store::storage;

pub use spanbay_model::hierarchy::FolderMoveResult;
pub use spanbay_model::history_filter::{HistoryCursor, HistoryFilter, HistoryRunSummary};
pub use spanbay_model::items::{
    Flow, FlowRunResult, FlowRunSummary, MockServer, MockTransactionLog,
};
pub use spanbay_model::models::{
    Collection, CookieJarEntry, CookieJarExport, CookieJarExportSaveResult,
    CookieJarImportOpenFileResult, CookieJarImportResult, Environment, EnvironmentVariable,
    ExecutionHistoryEntry, ExecutionHistoryResultKind, Folder, HistoryOrigin, HistorySource,
    Request, RequestBodyFilePickResult, RequestBodyFileValidationResult, TlsCertificate, Workspace,
    WorkspaceId,
};
pub use storage::MAX_TAB_SESSION_BYTES;
pub use storage::{HistoryRows, PlacementFault, StorageError};

// Crate-internal conveniences; the modules above remain the path for everything else.
pub use spanbay_model::hierarchy::{effective_auth, folder_chain};
pub use spanbay_model::items::{
    FlowExtraction, FlowExtractionSource, FlowRunInput, FlowStep, MockResponse, MockResponseMode,
    MockRoute, MockRouteType, MockRule, MockRuleOperator, MockRuleTarget, MockRulesOperator,
};
pub use spanbay_model::models::{
    ApiKeyLocation, Assertion, AuthConfig, CollectionId, CookieJarEntryId, CookieJarImport,
    EnvironmentId, FolderId, FormPart, HISTORY_BODY_PREVIEW_LIMIT_BYTES, HistoryEntryId,
    HttpMethod, KeyValue, NetworkOptions, OAuth2GrantType, ProtocolKind, RequestBody,
    RequestBodyFileValidationStatus, RequestId, VariableResolutionSource,
};
pub use spanbay_model::variables::{
    VariableResolutionError, VariableScopes, dynamic_variable_names, resolve_template,
    resolve_template_lenient,
};
pub use storage::WorkspaceStore;
