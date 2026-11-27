//! Postman Collection v2.1 import and export.

mod export;
mod fidelity;
mod import;
mod parse;

#[cfg(test)]
mod fidelity_tests;
#[cfg(test)]
mod tests;

pub use export::export_collection;
pub use import::import_collection;

use std::collections::HashMap;
use thiserror::Error;

use crate::workspace::models::{Collection, EnvironmentVariable, Folder, Request};
use crate::workspace::validation::WorkspaceError;

/// Schema URL written to exported Postman collections' `info.schema`.
pub const POSTMAN_SCHEMA_URL: &str =
    "https://schema.getpostman.com/json/collection/v2.1.0/collection.json";

/// Failure modes of Postman collection import and export.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PostmanError {
    #[error("invalid Postman collection JSON: {message}")]
    InvalidJson { message: String },
    #[error("Postman collection info.name is missing or blank")]
    MissingCollectionName,
    #[error("failed to serialize Postman collection: {message}")]
    Serialization { message: String },
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
}

/// Entities and warnings produced by importing a Postman collection.
#[derive(Debug, Clone, PartialEq)]
pub struct PostmanImport {
    pub collection: Collection,
    pub folders: Vec<Folder>,
    pub requests: Vec<Request>,
    pub variables: Vec<EnvironmentVariable>,
    pub warnings: Vec<String>,
}

/// What an export needs that it cannot look up itself (it never opens the database).
#[derive(Debug, Clone, Default)]
pub struct PostmanExportContext {
    /// Resolved URL (provider base URL plus endpoint path) of each provider-target AI request,
    /// by request id. A provider request without an entry exports its label URL
    /// and a warning.
    pub ai_urls: HashMap<String, String>,
    /// Write literal credentials (auth fields and credential headers that are not
    /// `{{variable}}` references). Off by default: they are exported empty, with a warning.
    pub include_literal_credentials: bool,
}

/// An exported collection and what the user should know about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostmanExport {
    /// Postman v2.1 JSON.
    pub json: String,
    /// Things the export left out or could not resolve, one line each.
    pub warnings: Vec<String>,
}
