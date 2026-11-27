//! The frozen IPC view of one model catalog row (`CatalogEntryDto`).

use serde::{Deserialize, Serialize};

/// Metadata of one model as the catalog lookup commands return it; the full row is
/// [`crate::model_catalog::ModelRecord`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogEntry {
    /// The model id.
    pub model_id: String,
    /// The catalog family (models.dev provider id) of the row.
    pub provider: String,
    /// `chat` or `embedding`.
    pub mode: String,
    /// Context window in tokens.
    pub max_input_tokens: Option<u64>,
    /// Maximum generated tokens.
    pub max_output_tokens: Option<u64>,
    /// Accepts images.
    pub vision: bool,
    /// Supports function calling.
    pub tools: bool,
    /// Supports reasoning.
    pub reasoning: bool,
    /// Supports prompt caching.
    pub prompt_caching: bool,
    /// USD per million input tokens.
    pub input_cost_per_mtok: Option<f64>,
    /// USD per million output tokens.
    pub output_cost_per_mtok: Option<f64>,
    /// USD per million cache-read input tokens.
    pub cache_read_cost_per_mtok: Option<f64>,
    /// The catalog's known input modalities (`text`, `image`, `audio`, `video`, `pdf`); left out
    /// while unknown.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub input_modalities: Vec<String>,
}
