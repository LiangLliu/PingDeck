//! Operation reports: applied-config ledger entries, bulk-enable/undo reports and probe-apply
//! records.

use serde::{Deserialize, Serialize};

use crate::llm::client_config::{AppliedFile, ClientApp};
use crate::llm::probe::apply::{ApplyReport, PreviousState};

/// One ledger row: a file written for `client_id` by the writer of `app`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// `ap_` and 12 hex characters.
    pub id: String,
    /// The gateway client whose token the file holds.
    pub client_id: String,
    pub app: ClientApp,
    /// The file as [`crate::llm::client_config::apply`] reported it (`backup_path` is `None` and
    /// `existed_before` false when the write created the file).
    pub file: AppliedFile,
    pub applied_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BulkEnableReport {
    /// Operation id: pass it to `undo_bulk`.
    pub id: String,
    pub provider_id: String,
    /// The flag of every model that actually changed, before and after. Empty when nothing
    /// changed; then nothing was logged and there is nothing to undo.
    pub previous: Vec<PreviousState>,
    /// When it was applied, Unix ms.
    pub applied_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoReport {
    /// Models whose flag was restored.
    pub restored: Vec<String>,
    /// Models left as they are because they were edited (or deleted) since.
    pub kept: Vec<String>,
}

/// One apply of probe results, as the undo needs it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeApplyRecord {
    pub apply_id: String,
    /// The run whose results were applied.
    pub run_id: String,
    /// The provider whose models changed.
    pub provider_id: String,
    /// When it was applied, Unix ms.
    pub applied_at_ms: i64,
    /// What it did; `report.previous` holds the flag of every changed model before and after.
    pub report: ApplyReport,
}
