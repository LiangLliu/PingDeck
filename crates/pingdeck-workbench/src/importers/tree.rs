//! The collection tree the Insomnia, Bruno and Hoppscotch importers fill: deterministic ids,
//! folders, requests and warnings, plus the small conversions they share.

use serde_json::Value;

use super::helpers::{Ids, RequestDraft, build_request, http_method, non_empty_or};
use super::{ImportError, ImportedCollection};
use crate::workspace::models::{Collection, Folder, FolderId, HttpMethod, KeyValue, Request};

pub(super) struct TreeBuilder {
    ids: Ids,
    workspace_id: String,
    pub(super) collection: Collection,
    folders: Vec<Folder>,
    requests: Vec<Request>,
    pub(super) warnings: Vec<String>,
}

impl TreeBuilder {
    pub(super) fn new(name: &str, workspace_id: &str) -> Result<Self, ImportError> {
        let mut ids = Ids::new(name);
        let collection = Collection::new(ids.next("col"), workspace_id, name)?;
        Ok(Self {
            ids,
            workspace_id: workspace_id.to_owned(),
            collection,
            folders: Vec::new(),
            requests: Vec::new(),
            warnings: Vec::new(),
        })
    }

    /// A folder named `name` (a blank name becomes `Folder`) under `parent`.
    pub(super) fn folder(
        &mut self,
        name: &str,
        parent: Option<&FolderId>,
        position: i64,
    ) -> Result<FolderId, ImportError> {
        let mut folder = Folder::new(
            self.ids.next("folder"),
            self.collection.id.as_str(),
            non_empty_or(name, "Folder"),
        )?;
        if let Some(parent) = parent {
            folder = folder.with_parent(parent.as_str())?;
        }
        folder.position = position;
        let id = folder.id.clone();
        self.folders.push(folder);
        Ok(id)
    }

    /// Adds the request of `draft` and hands it back for the fields a draft does not carry.
    pub(super) fn request(
        &mut self,
        draft: RequestDraft,
        position: i64,
    ) -> Result<&mut Request, ImportError> {
        let collection_id = self.collection.id.clone();
        let request = build_request(
            draft,
            &mut self.ids,
            &self.workspace_id,
            &collection_id,
            position,
        )?;
        self.requests.push(request);
        let last = self.requests.len() - 1;
        Ok(&mut self.requests[last])
    }

    pub(super) fn finish(self) -> ImportedCollection {
        ImportedCollection {
            collection: self.collection,
            folders: self.folders,
            requests: self.requests,
            warnings: self.warnings,
        }
    }
}

/// A method name as an [`HttpMethod`]; an unknown one is reported and becomes GET.
pub(super) fn method_or_get(raw: &str, warnings: &mut Vec<String>, label: &str) -> HttpMethod {
    if raw.trim().is_empty() {
        return HttpMethod::Get;
    }
    http_method(raw).unwrap_or_else(|| {
        warnings.push(format!(
            "{label}: unsupported method '{}'; using GET",
            raw.trim()
        ));
        HttpMethod::Get
    })
}

/// `value[key]` as a string; numbers and booleans as text, anything else empty.
pub(super) fn text(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        _ => String::new(),
    }
}

/// Rows `[{ <key_field>, value, <flag> }]` as key-value pairs; `enabled_when` is the flag's
/// value that means enabled (absent means enabled). Rows with a blank key are dropped.
pub(super) fn rows(
    value: Option<&Value>,
    key_field: &str,
    flag: &str,
    enabled_when: bool,
    convert: fn(&str) -> String,
) -> Vec<KeyValue> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let key = text(row, key_field);
            if key.trim().is_empty() {
                return None;
            }
            let enabled = row
                .get(flag)
                .and_then(Value::as_bool)
                .is_none_or(|set| set == enabled_when);
            Some(KeyValue {
                key: convert(key.trim()),
                value: convert(&text(row, "value")),
                enabled,
                description: Some(text(row, "description")).filter(|text| !text.is_empty()),
            })
        })
        .collect()
}

/// Pushes "`one`" for a count of 1 and "`many`" (with `{count}`) for more.
pub(super) fn note_count(warnings: &mut Vec<String>, count: usize, one: &str, many: &str) {
    match count {
        0 => {}
        1 => warnings.push(one.to_owned()),
        count => warnings.push(many.replace("{count}", &count.to_string())),
    }
}
