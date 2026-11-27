//! Collection/folder hierarchy: inheritance helpers (folder chain, effective auth) and the wire
//! summaries of a workspace's tree, its delete-confirm counts and folder moves.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::ai_request::RequestMode;
use crate::models::{AuthConfig, Collection, Folder, FolderId, HttpMethod, ProtocolKind, Request};

/// Folder ancestry for a request, ordered outermost first (root → leaf).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderChain {
    pub folders: Vec<Folder>,
    pub broken: bool,
}

impl FolderChain {
    /// Returns the folder names in chain order, outermost first.
    #[must_use]
    pub fn labels(&self) -> Vec<String> {
        self.folders
            .iter()
            .map(|folder| folder.name.clone())
            .collect()
    }
}

/// Builds the root → leaf folder chain for a request. Cycles and missing
/// parents are reported with `broken = true` and the walk stops safely.
#[must_use]
pub fn folder_chain(request: &Request, folders: &[Folder]) -> FolderChain {
    let Some(folder_id) = request.folder_id.as_ref() else {
        return FolderChain::default();
    };

    let by_id: HashMap<&str, &Folder> = folders
        .iter()
        .map(|folder| (folder.id.as_str(), folder))
        .collect();
    let mut chain = Vec::new();
    let mut seen = Vec::new();
    let mut current = Some(folder_id.clone());
    let mut broken = false;

    while let Some(id) = current {
        if seen.iter().any(|existing: &FolderId| existing == &id) {
            broken = true;
            break;
        }
        seen.push(id.clone());

        let Some(folder) = by_id.get(id.as_str()) else {
            broken = true;
            break;
        };
        chain.push((*folder).clone());
        current = folder.parent_folder_id.clone();
    }

    chain.reverse();
    FolderChain {
        folders: chain,
        broken,
    }
}

/// Resolves `Inherit` from the request through its folder chain to the
/// collection. Collection-level `Inherit` resolves to `None`.
#[must_use]
pub fn effective_auth(
    request: &Request,
    collection: Option<&Collection>,
    folders: &[Folder],
) -> AuthConfig {
    if !matches!(request.auth, AuthConfig::Inherit) {
        return request.auth.clone();
    }

    let chain = folder_chain(request, folders);
    for folder in chain.folders.iter().rev() {
        if !matches!(folder.auth, AuthConfig::Inherit) {
            return folder.auth.clone();
        }
    }

    match collection {
        Some(collection) if !matches!(collection.auth, AuthConfig::Inherit) => {
            collection.auth.clone()
        }
        _ => AuthConfig::None,
    }
}

/// How many characters of a request URL a [`RequestSummary`] keeps.
pub const URL_SUMMARY_MAX_CHARS: usize = 256;

/// A collection in the tree: no variables, auth or scripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionSummary {
    pub id: String,
    pub name: String,
    pub position: i64,
}

/// A folder in the tree: no auth or scripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderSummary {
    pub id: String,
    pub collection_id: String,
    /// The folder it is nested in; `None` at the collection root.
    pub parent_folder_id: Option<String>,
    pub name: String,
    pub position: i64,
}

/// A request in the tree: no headers, body, auth or scripts, and a cut URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestSummary {
    pub id: String,
    /// The collection the request is in; `None` for a loose request.
    pub collection_id: Option<String>,
    /// The folder the request is in; `None` at the collection root.
    pub folder_id: Option<String>,
    pub name: String,
    pub protocol: ProtocolKind,
    pub method: HttpMethod,
    pub mode: RequestMode,
    /// The first [`URL_SUMMARY_MAX_CHARS`] characters of the URL.
    pub url: String,
    pub position: i64,
    /// When the agent last created or changed the request, until the user sends it from the UI:
    /// the tree's "changed by the agent" badge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_touched_ms: Option<i64>,
}

/// The collections, folders and requests of one workspace as summaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceTree {
    /// Collections by position.
    pub collections: Vec<CollectionSummary>,
    /// Every folder of every collection by position.
    pub folders: Vec<FolderSummary>,
    /// Every request (also loose ones) by position.
    pub requests: Vec<RequestSummary>,
}

/// How much a workspace holds, for the "delete workspace" confirm.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCounts {
    pub collections: u64,
    /// Saved requests in all collections.
    pub requests: u64,
    /// Environments (not the globals).
    pub environments: u64,
    /// Mock servers, running or not.
    pub mock_servers: u64,
    pub flows: u64,
    /// History entries, mock hits included.
    pub history_entries: u64,
    /// Live top-level chats bound to the workspace; they stay, unbound, after the delete.
    pub chats: u64,
}

/// Folders and requests changed by a folder move.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FolderMoveResult {
    pub folders: Vec<Folder>,
    pub requests: Vec<Request>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: &str, collection_id: &str, parent: Option<&str>, auth: AuthConfig) -> Folder {
        let mut folder = Folder::new(id, collection_id, id).unwrap();
        if let Some(parent) = parent {
            folder = folder.with_parent(parent).unwrap();
        }
        folder.auth = auth;
        folder
    }

    #[test]
    fn builds_root_to_leaf_chain() {
        let request = Request::new("req_1", "ws_1", "Get user", "https://example.com")
            .unwrap()
            .with_folder("fold_leaf")
            .unwrap();
        let folders = vec![
            folder("fold_root", "col_1", None, AuthConfig::Inherit),
            folder("fold_mid", "col_1", Some("fold_root"), AuthConfig::Inherit),
            folder("fold_leaf", "col_1", Some("fold_mid"), AuthConfig::Inherit),
        ];

        let chain = folder_chain(&request, &folders);
        assert!(!chain.broken);
        assert_eq!(
            chain
                .folders
                .iter()
                .map(|folder| folder.id.as_str())
                .collect::<Vec<_>>(),
            vec!["fold_root", "fold_mid", "fold_leaf"]
        );
    }

    #[test]
    fn request_auth_wins_over_parent() {
        let mut request = Request::new("req_1", "ws_1", "Get user", "https://example.com").unwrap();
        request.auth = AuthConfig::Bearer {
            token_ref: "{{token}}".to_owned(),
        };
        let mut collection = Collection::new("col_1", "ws_1", "API").unwrap();
        collection.auth = AuthConfig::Basic {
            username: "user".to_owned(),
            password_ref: "pass".to_owned(),
        };

        assert_eq!(
            effective_auth(&request, Some(&collection), &[]),
            AuthConfig::Bearer {
                token_ref: "{{token}}".to_owned()
            }
        );
    }

    #[test]
    fn nearest_folder_auth_wins_then_collection() {
        let request = Request::new("req_1", "ws_1", "Get user", "https://example.com")
            .unwrap()
            .with_folder("fold_leaf")
            .unwrap();
        let folders = vec![
            folder(
                "fold_root",
                "col_1",
                None,
                AuthConfig::Bearer {
                    token_ref: "root".to_owned(),
                },
            ),
            folder(
                "fold_leaf",
                "col_1",
                Some("fold_root"),
                AuthConfig::ApiKey {
                    key: "X-Key".to_owned(),
                    value_ref: "leaf".to_owned(),
                    location: super::super::models::ApiKeyLocation::Header,
                },
            ),
        ];
        let mut collection = Collection::new("col_1", "ws_1", "API").unwrap();
        collection.auth = AuthConfig::None;

        assert_eq!(
            effective_auth(&request, Some(&collection), &folders),
            AuthConfig::ApiKey {
                key: "X-Key".to_owned(),
                value_ref: "leaf".to_owned(),
                location: super::super::models::ApiKeyLocation::Header,
            }
        );
    }

    #[test]
    fn unresolved_inherit_falls_back_to_none() {
        let request = Request::new("req_1", "ws_1", "Get user", "https://example.com").unwrap();
        let collection = Collection::new("col_1", "ws_1", "API").unwrap();
        assert_eq!(
            effective_auth(&request, Some(&collection), &[]),
            AuthConfig::None
        );
    }
}
