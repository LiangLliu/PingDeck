//! Serializable workspace domain models (requests, collections, environments, cookies, TLS).

mod assertion;
mod cookie;
mod environment;
mod history;
mod network;
mod request;
#[cfg(test)]
mod tests;

pub use assertion::*;
pub use cookie::*;
pub use environment::*;
pub use history::*;
pub use network::*;
pub use request::*;

use serde::{Deserialize, Serialize};

use crate::validation::{WorkspaceError, validate_id, validate_name};

macro_rules! id_type {
    ($name:ident, $entity:literal) => {
        #[doc = concat!("Validated, non-blank identifier of a ", $entity, ".")]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(String);

        impl $name {
            /// Validates `value` as a non-blank identifier and wraps it.
            pub fn new(value: impl AsRef<str>) -> Result<Self, WorkspaceError> {
                validate_id($entity, value.as_ref()).map(Self)
            }

            /// Returns the identifier as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

id_type!(WorkspaceId, "workspace");
id_type!(CollectionId, "collection");
id_type!(FolderId, "folder");
id_type!(RequestId, "request");
id_type!(EnvironmentId, "environment");
id_type!(HistoryEntryId, "history entry");
id_type!(CookieJarEntryId, "cookie jar entry");
id_type!(TlsCertificateId, "tls certificate");

/// A workspace that owns collections, requests, environments, and cookies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    pub sync_state: SyncState,
}

impl Workspace {
    /// Creates a local-only workspace with a validated id and name.
    pub fn new(id: impl AsRef<str>, name: impl AsRef<str>) -> Result<Self, WorkspaceError> {
        Ok(Self {
            id: WorkspaceId::new(id)?,
            name: validate_name("workspace", name.as_ref())?,
            sync_state: SyncState::LocalOnly,
        })
    }
}

/// Whether and how a workspace is synchronized with a remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncState {
    LocalOnly,
}

/// A named group of folders and requests with shared auth, variables, and scripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub id: CollectionId,
    pub workspace_id: WorkspaceId,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub variables: Vec<EnvironmentVariable>,
    #[serde(default)]
    pub pre_request_script: String,
    #[serde(default)]
    pub test_script: String,
    #[serde(default)]
    pub position: i64,
}

impl Collection {
    /// Creates an empty collection with a validated id, workspace id, and name.
    pub fn new(
        id: impl AsRef<str>,
        workspace_id: impl AsRef<str>,
        name: impl AsRef<str>,
    ) -> Result<Self, WorkspaceError> {
        Ok(Self {
            id: CollectionId::new(id)?,
            workspace_id: WorkspaceId::new(workspace_id)?,
            name: validate_name("collection", name.as_ref())?,
            description: String::new(),
            auth: AuthConfig::default(),
            variables: Vec::new(),
            pre_request_script: String::new(),
            test_script: String::new(),
            position: 0,
        })
    }
}

/// A folder nested in a collection (or another folder) with its own auth and scripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub id: FolderId,
    pub collection_id: CollectionId,
    pub parent_folder_id: Option<FolderId>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub pre_request_script: String,
    #[serde(default)]
    pub test_script: String,
    #[serde(default)]
    pub position: i64,
    /// Opaque version of the saved folder the reader got (filled by the use cases that answer
    /// the editor, never stored or exported); a save that echoes it is refused with `Conflict`
    /// (`stale_write`) when the stored folder changed since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

impl Folder {
    /// Creates a root-level folder with a validated id, collection id, and name.
    pub fn new(
        id: impl AsRef<str>,
        collection_id: impl AsRef<str>,
        name: impl AsRef<str>,
    ) -> Result<Self, WorkspaceError> {
        Ok(Self {
            id: FolderId::new(id)?,
            collection_id: CollectionId::new(collection_id)?,
            parent_folder_id: None,
            name: validate_name("folder", name.as_ref())?,
            description: String::new(),
            auth: AuthConfig::default(),
            pre_request_script: String::new(),
            test_script: String::new(),
            position: 0,
            revision: None,
        })
    }

    /// Places the folder under the given parent folder, validating the parent id.
    pub fn with_parent(
        mut self,
        parent_folder_id: impl AsRef<str>,
    ) -> Result<Self, WorkspaceError> {
        self.parent_folder_id = Some(FolderId::new(parent_folder_id)?);
        Ok(self)
    }
}
