//! Importers for non-Postman exchange formats: cURL commands, HAR archives, OpenAPI 2/3
//! documents and the collection exports of Insomnia (v4), Bruno and Hoppscotch.
//!
//! Every importer returns an [`ImportedCollection`] with deterministic IDs so a
//! re-import of the same document produces the same entities. Inputs that cannot
//! be represented are reported through `warnings`; only unreadable documents are
//! errors.

mod bruno;
mod curl;
mod har;
mod helpers;
mod hoppscotch;
mod insomnia;
mod openapi;
mod tree;

#[cfg(test)]
mod migration_tests;
#[cfg(test)]
mod tests;

pub use bruno::import_bruno;
pub use curl::import_curl;
pub use har::import_har;
pub use hoppscotch::import_hoppscotch;
pub use insomnia::import_insomnia;
pub use openapi::{OpenApiMockRoutes, import_openapi, openapi_mock_routes};

use thiserror::Error;

use crate::workspace::models::{Collection, Folder, Request};
use crate::workspace::validation::WorkspaceError;

/// Failure modes of the importers in this module.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ImportError {
    #[error("invalid JSON: {message}")]
    InvalidJson { message: String },
    #[error("unsupported document: {message}")]
    UnsupportedDocument { message: String },
    #[error("no request URL found in the cURL command")]
    MissingUrl,
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
}

/// Collection, folders, requests and warnings produced by an importer.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedCollection {
    pub collection: Collection,
    pub folders: Vec<Folder>,
    pub requests: Vec<Request>,
    pub warnings: Vec<String>,
}
