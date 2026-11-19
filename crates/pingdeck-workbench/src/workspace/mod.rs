//! One import path for the workspace domain inside the use-case crates: the vocabulary from
//! `pingdeck_model`, the assertion engine from `pingdeck_executor` and the store from
//! `pingdeck_store`.

pub use pingdeck_model::{
    ai_request, hierarchy, history_filter, items, models, validation, variables,
};

pub use pingdeck_executor::assertions;

pub use pingdeck_store::storage;

pub use pingdeck_model::hierarchy::FolderMoveResult;
pub use pingdeck_model::history_filter::{HistoryCursor, HistoryFilter, HistoryRunSummary};
pub use pingdeck_model::items::{
    Flow, FlowRunResult, FlowRunSummary, MockServer, MockTransactionLog,
};
pub use pingdeck_model::models::{
    Collection, CookieJarEntry, CookieJarExport, CookieJarExportSaveResult,
    CookieJarImportOpenFileResult, CookieJarImportResult, Environment, EnvironmentVariable,
    ExecutionHistoryEntry, ExecutionHistoryResultKind, Folder, HistoryOrigin, HistorySource,
    Request, RequestBodyFilePickResult, RequestBodyFileValidationResult, TlsCertificate, Workspace,
    WorkspaceId,
};
pub use storage::MAX_TAB_SESSION_BYTES;
pub use storage::{HistoryRows, PlacementFault, StorageError};

// Crate-internal conveniences; the modules above remain the path for everything else.
pub use pingdeck_model::hierarchy::{effective_auth, folder_chain};
pub use pingdeck_model::items::{
    FlowExtraction, FlowExtractionSource, FlowRunInput, FlowStep, MockResponse, MockResponseMode,
    MockRoute, MockRouteType, MockRule, MockRuleOperator, MockRuleTarget, MockRulesOperator,
};
pub use pingdeck_model::models::{
    ApiKeyLocation, Assertion, AuthConfig, CollectionId, CookieJarEntryId, CookieJarImport,
    EnvironmentId, FolderId, FormPart, HISTORY_BODY_PREVIEW_LIMIT_BYTES, HistoryEntryId,
    HttpMethod, KeyValue, NetworkOptions, OAuth2GrantType, ProtocolKind, RequestBody,
    RequestBodyFileValidationStatus, RequestId, VariableResolutionSource,
};
pub use pingdeck_model::variables::{
    VariableResolutionError, VariableScopes, dynamic_variable_names, resolve_template,
    resolve_template_lenient,
};
pub use storage::WorkspaceStore;
