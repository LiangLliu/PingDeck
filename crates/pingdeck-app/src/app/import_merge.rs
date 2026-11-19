//! Re-importing a collection into the one it came from without changing ids.
//!
//! Single responsibility: pair the imported tree with the stored collection of the same name
//! and compute what to save, so flow steps, history rows and open tabs that name a request keep
//! working. Folders pair by their path of names; requests by method and URL path (in the same
//! folder first, then anywhere), and failing that by name in the same folder. Nothing here
//! touches the store.
//!
//! [`MergeMode::Update`] lets the file own only what a re-import should refresh: names, method,
//! URL, body, description, placement; parameters and headers by key (a blank value in the file
//! keeps the stored value, rows the file lacks are kept). What the user added stays: assertions,
//! scripts, network options and saved examples unless the file carries its own, auth unless the
//! file names a different kind, AI mode, and every stored item the file no longer has.
//! [`MergeMode::Replace`] makes the file the whole truth, but paired items keep their ids and
//! stored items the file lacks are removed; a secret variable the file leaves blank keeps its
//! stored value.

use std::collections::{HashMap, HashSet};
use std::mem::discriminant;

use crate::workspace::models::{AuthConfig, HttpMethod, KeyValue, NetworkOptions};
use crate::workspace::{Collection, EnvironmentVariable, Folder, FolderId, Request, RequestId};

use super::factory_service::new_entity_id;

/// How a re-import treats the stored collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MergeMode {
    Update,
    Replace,
}

/// What to write for a re-import into an existing collection.
#[derive(Debug)]
pub(crate) struct MergePlan {
    /// The stored collection's id with the merged fields.
    pub(crate) collection: Collection,
    /// Every folder to save: paired, new and (update) kept ones.
    pub(crate) folders: Vec<Folder>,
    /// The requests to save: paired and new ones.
    pub(crate) requests: Vec<Request>,
    /// Folders and requests the import created (an undo deletes them).
    pub(crate) added_folders: Vec<FolderId>,
    pub(crate) added_requests: Vec<RequestId>,
    /// Stored folders and requests the file no longer has (replace only; deleted after the save).
    pub(crate) removed_folders: Vec<FolderId>,
    pub(crate) removed_requests: Vec<RequestId>,
    /// Requests of the file paired with a stored request (their ids were kept).
    pub(crate) matched: usize,
    /// Stored requests the file lacks that an update leaves as they are.
    pub(crate) kept_requests: usize,
}

/// Each folder's path of lower-cased, trimmed names from the collection root.
fn folder_paths(folders: &[Folder]) -> HashMap<FolderId, Vec<String>> {
    let by_id: HashMap<&FolderId, &Folder> = folders.iter().map(|f| (&f.id, f)).collect();
    let mut paths = HashMap::new();
    for folder in folders {
        let mut path = Vec::new();
        let mut seen = HashSet::new();
        let mut current = Some(folder);
        while let Some(node) = current {
            if !seen.insert(&node.id) {
                break;
            }
            path.push(node.name.trim().to_lowercase());
            current = node
                .parent_folder_id
                .as_ref()
                .and_then(|parent| by_id.get(parent).copied());
        }
        path.reverse();
        paths.insert(folder.id.clone(), path);
    }
    paths
}

/// What a request is paired by.
struct RequestKey {
    method: HttpMethod,
    url: String,
    folder: Vec<String>,
    name: String,
}

/// The URL a request is paired by: no query, no fragment, no trailing slash.
fn url_key(url: &str) -> String {
    let end = url.find(['?', '#']).unwrap_or(url.len());
    url[..end].trim().trim_end_matches('/').to_owned()
}

/// Imported rows with stored values filled in where the file leaves them blank, then the stored
/// rows whose key the file lacks. Header keys compare case-insensitively.
fn merge_rows(
    imported: Vec<KeyValue>,
    stored: &[KeyValue],
    case_insensitive: bool,
) -> Vec<KeyValue> {
    let same = |left: &str, right: &str| {
        if case_insensitive {
            left.trim().eq_ignore_ascii_case(right.trim())
        } else {
            left.trim() == right.trim()
        }
    };
    let mut merged: Vec<KeyValue> = imported
        .into_iter()
        .map(|mut row| {
            if row.value.is_empty()
                && let Some(old) = stored.iter().find(|old| same(&old.key, &row.key))
            {
                row.value.clone_from(&old.value);
                row.enabled = old.enabled;
            }
            row
        })
        .collect();
    for old in stored {
        if !merged.iter().any(|row| same(&row.key, &old.key)) {
            merged.push(old.clone());
        }
    }
    merged
}

/// The file's auth unless it inherits or names the same kind as the stored one, whose secrets
/// the user may have filled in.
fn merge_auth(imported: AuthConfig, stored: &AuthConfig) -> AuthConfig {
    if matches!(imported, AuthConfig::Inherit) || discriminant(&imported) == discriminant(stored) {
        stored.clone()
    } else {
        imported
    }
}

fn keep_if_blank(imported: String, stored: &str) -> String {
    if imported.trim().is_empty() {
        stored.to_owned()
    } else {
        imported
    }
}

/// Collection variables: update keeps stored values the file leaves blank and stored rows it
/// lacks; replace takes the file's rows, keeping only a blank secret's stored value.
fn merge_variables(
    imported: Vec<EnvironmentVariable>,
    stored: &[EnvironmentVariable],
    mode: MergeMode,
) -> Vec<EnvironmentVariable> {
    let mut merged: Vec<EnvironmentVariable> = imported
        .into_iter()
        .map(|mut variable| {
            if let Some(old) = stored.iter().find(|old| old.key == variable.key) {
                variable.id.clone_from(&old.id);
                let blank = variable.value.as_deref().is_none_or(str::is_empty);
                if blank && (mode == MergeMode::Update || (variable.is_secret && old.is_secret)) {
                    variable.value.clone_from(&old.value);
                    variable.is_secret = old.is_secret;
                }
            }
            variable
        })
        .collect();
    if mode == MergeMode::Update {
        for old in stored {
            if !merged.iter().any(|variable| variable.key == old.key) {
                merged.push(old.clone());
            }
        }
    }
    merged
}

/// Pairs `imported` with the stored collection (`stored`, its folders and requests) and computes
/// the save. `imported` folders and requests may carry any ids; the plan's are stored or fresh.
pub(crate) fn plan_merge(
    stored: (&Collection, &[Folder], &[Request]),
    imported: (Collection, Vec<Folder>, Vec<Request>),
    mode: MergeMode,
) -> MergePlan {
    let (stored_collection, stored_folders, stored_requests) = stored;
    let (mut collection, imported_folders, imported_requests) = imported;
    let collection_id = stored_collection.id.clone();

    // Folders by path.
    let stored_paths = folder_paths(stored_folders);
    let imported_paths = folder_paths(&imported_folders);
    let mut stored_by_path: HashMap<&Vec<String>, Vec<&Folder>> = HashMap::new();
    for folder in stored_folders {
        if let Some(path) = stored_paths.get(&folder.id) {
            stored_by_path.entry(path).or_default().push(folder);
        }
    }
    let mut folder_map: HashMap<FolderId, FolderId> = HashMap::new();
    let mut paired_folders: HashSet<FolderId> = HashSet::new();
    let mut added_folders = Vec::new();
    for folder in &imported_folders {
        let stored = imported_paths
            .get(&folder.id)
            .and_then(|path| stored_by_path.get_mut(path))
            .and_then(|candidates| {
                let at = candidates
                    .iter()
                    .position(|candidate| !paired_folders.contains(&candidate.id))?;
                Some(candidates.remove(at))
            });
        let id = match stored {
            Some(stored) => {
                paired_folders.insert(stored.id.clone());
                stored.id.clone()
            }
            None => {
                let fresh =
                    FolderId::new(new_entity_id("fold")).unwrap_or_else(|_| folder.id.clone());
                added_folders.push(fresh.clone());
                fresh
            }
        };
        folder_map.insert(folder.id.clone(), id);
    }
    let mut folders = Vec::new();
    for mut folder in imported_folders {
        let Some(id) = folder_map.get(&folder.id).cloned() else {
            continue;
        };
        folder.parent_folder_id = folder
            .parent_folder_id
            .as_ref()
            .and_then(|parent| folder_map.get(parent).cloned());
        folder.collection_id = collection_id.clone();
        if mode == MergeMode::Update
            && let Some(old) = stored_folders.iter().find(|old| old.id == id)
        {
            folder.description = keep_if_blank(folder.description, &old.description);
            folder.auth = merge_auth(folder.auth, &old.auth);
            folder.pre_request_script =
                keep_if_blank(folder.pre_request_script, &old.pre_request_script);
            folder.test_script = keep_if_blank(folder.test_script, &old.test_script);
        }
        folder.id = id;
        folders.push(folder);
    }
    let mut removed_folders = Vec::new();
    for old in stored_folders
        .iter()
        .filter(|old| !paired_folders.contains(&old.id))
    {
        match mode {
            MergeMode::Update => folders.push(old.clone()),
            MergeMode::Replace => removed_folders.push(old.id.clone()),
        }
    }

    // Requests: method + URL in the same folder, method + URL anywhere, then name in the folder.
    let key = |request: &Request, paths: &HashMap<FolderId, Vec<String>>| RequestKey {
        method: request.method,
        url: url_key(&request.url),
        folder: request
            .folder_id
            .as_ref()
            .and_then(|id| paths.get(id))
            .cloned()
            .unwrap_or_default(),
        name: request.name.trim().to_lowercase(),
    };
    let new_keys: Vec<RequestKey> = imported_requests
        .iter()
        .map(|request| key(request, &imported_paths))
        .collect();
    let old_keys: Vec<RequestKey> = stored_requests
        .iter()
        .map(|request| key(request, &stored_paths))
        .collect();
    let passes: [fn(&RequestKey, &RequestKey) -> bool; 3] = [
        |new, old| new.method == old.method && new.url == old.url && new.folder == old.folder,
        |new, old| new.method == old.method && new.url == old.url,
        |new, old| new.name == old.name && new.folder == old.folder,
    ];
    let mut paired_requests = vec![false; stored_requests.len()];
    let mut pairs: Vec<Option<usize>> = vec![None; imported_requests.len()];
    for pass in passes {
        for (index, new) in new_keys.iter().enumerate() {
            if pairs[index].is_some() {
                continue;
            }
            if let Some(found) = (0..old_keys.len())
                .find(|&candidate| !paired_requests[candidate] && pass(new, &old_keys[candidate]))
            {
                paired_requests[found] = true;
                pairs[index] = Some(found);
            }
        }
    }

    let mut requests = Vec::new();
    let mut added_requests = Vec::new();
    let mut matched = 0;
    for (mut request, old) in imported_requests.into_iter().zip(
        pairs
            .into_iter()
            .map(|found| found.map(|at| &stored_requests[at])),
    ) {
        request.folder_id = request
            .folder_id
            .as_ref()
            .and_then(|folder| folder_map.get(folder).cloned());
        request.collection_id = Some(collection_id.clone());
        match old {
            Some(old) => {
                matched += 1;
                request.id = old.id.clone();
                request.workspace_id = old.workspace_id.clone();
                if mode == MergeMode::Update {
                    request.query_params =
                        merge_rows(request.query_params, &old.query_params, false);
                    request.headers = merge_rows(request.headers, &old.headers, true);
                    request.path_variables =
                        merge_rows(request.path_variables, &old.path_variables, false);
                    request.auth = merge_auth(request.auth, &old.auth);
                    if request.assertions.is_empty() {
                        request.assertions.clone_from(&old.assertions);
                    }
                    request.pre_request_script =
                        keep_if_blank(request.pre_request_script, &old.pre_request_script);
                    request.test_script = keep_if_blank(request.test_script, &old.test_script);
                    if request.network_options == NetworkOptions::default() {
                        request.network_options = old.network_options.clone();
                    }
                    if request.examples.is_empty() {
                        request.examples.clone_from(&old.examples);
                    }
                    request.mode = old.mode;
                    request.ai.clone_from(&old.ai);
                }
            }
            None => {
                if let Ok(fresh) = RequestId::new(new_entity_id("req")) {
                    request.id = fresh;
                }
                added_requests.push(request.id.clone());
            }
        }
        requests.push(request);
    }
    let unpaired = paired_requests.iter().filter(|paired| !**paired).count();
    let kept_requests = if mode == MergeMode::Update {
        unpaired
    } else {
        0
    };
    let removed_requests = match mode {
        MergeMode::Update => Vec::new(),
        MergeMode::Replace => stored_requests
            .iter()
            .zip(&paired_requests)
            .filter(|(_, paired)| !**paired)
            .map(|(old, _)| old.id.clone())
            .collect(),
    };

    // The collection keeps its id and stored name (the names match, ignoring case).
    collection.id = collection_id;
    collection.workspace_id = stored_collection.workspace_id.clone();
    collection.name.clone_from(&stored_collection.name);
    collection.position = stored_collection.position;
    collection.variables = merge_variables(
        std::mem::take(&mut collection.variables),
        &stored_collection.variables,
        mode,
    );
    if mode == MergeMode::Update {
        collection.description =
            keep_if_blank(collection.description, &stored_collection.description);
        collection.auth = merge_auth(collection.auth, &stored_collection.auth);
        collection.pre_request_script = keep_if_blank(
            collection.pre_request_script,
            &stored_collection.pre_request_script,
        );
        collection.test_script =
            keep_if_blank(collection.test_script, &stored_collection.test_script);
    }

    MergePlan {
        collection,
        folders,
        requests,
        added_folders,
        added_requests,
        removed_folders,
        removed_requests,
        matched,
        kept_requests,
    }
}

/// How many requests of `imported` would pair with a stored one (the preview's count).
pub(crate) fn count_matches(
    stored: (&Collection, &[Folder], &[Request]),
    imported: (Collection, Vec<Folder>, Vec<Request>),
) -> (usize, Vec<RequestId>) {
    let plan = plan_merge(stored, imported, MergeMode::Replace);
    (plan.matched, plan.removed_requests)
}
