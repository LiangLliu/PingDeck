//! Pure data types and rules of persisted LLM records: keys, models, gateway clients, the request
//! log (with the read-time `reason` of a row), capture stats, applied configs, probe apply logs,
//! and the status and probe facts derived from keys and models. SQLite persistence lives in
//! `spanbay-store`.

mod clients;
mod facts;
mod keys;
mod log;
mod log_reason;
mod models;
mod ops;

pub use clients::{
    CheckedClientInput, ClientHashEntry, ClientInput, ClientLimits, ClientPolicyInput,
    ClientWithToken, GatewayClient, MAX_ALLOW_ENTRIES, MAX_CLIENTS, MAX_LIMIT_CONCURRENT,
    MAX_LIMIT_RPM, MAX_NAME_CHARS, MIGRATED_CLIENT_ID, MintedToken, TOUCH_INTERVAL_MS, TokenLookup,
    check_client_input, check_client_limits, match_token, mint_token, token_tag,
};
pub use facts::{balance_signal, decisive_test};
pub use keys::{
    KeyRunState, KeyRuntime, KeyTest, KeyVerdict, ModelsListing, ModelsListingState, ProviderKey,
};
pub use log::{
    CaptureStats, ClearReport, DEFAULT_LOG_PAGE, LOG_ROWS_PER_SOURCE, LOG_SOURCES, LogDetail,
    LogFilter, LogPage, LogPageResult, MAX_LOG_PAGE, RequestLogEntry, StatusClass, book_entry,
};
pub use log_reason::derive_log_reason;
pub use models::{
    Alias, AliasTarget, FactColumns, FactSource, ModelRef, ModelSource, ProviderModel,
};
pub use ops::{BulkEnableReport, LedgerEntry, ProbeApplyRecord, UndoReport};
