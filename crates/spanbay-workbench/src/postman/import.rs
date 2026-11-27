use serde_json::{Map, Value};

use super::fidelity::{
    import_examples, merge_behavior, network_options_from_behavior, take_assertions,
};
use super::parse::{
    key_values_from_array, key_values_from_query_string, local_path_warning, parse_auth,
    parse_body, parse_description, parse_events, parse_method, parse_url, scalar_string,
    split_raw_url,
};
use super::{PostmanError, PostmanImport};
use crate::workspace::models::{
    Collection, EnvironmentVariable, Folder, ProtocolKind, Request, RequestBody,
};

const MAX_SLUG_LEN: usize = 32;

/// Parses a Postman v2.1 collection JSON into SpanBay entities plus import warnings.
pub fn import_collection(json: &str, workspace_id: &str) -> Result<PostmanImport, PostmanError> {
    let value: Value = serde_json::from_str(json).map_err(|error| PostmanError::InvalidJson {
        message: error.to_string(),
    })?;
    let root = value.as_object().ok_or_else(|| PostmanError::InvalidJson {
        message: "collection root must be a JSON object".to_string(),
    })?;

    let name = root
        .get("info")
        .and_then(Value::as_object)
        .and_then(|info| info.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or(PostmanError::MissingCollectionName)?;

    let mut ids = IdGenerator::new(name);
    let collection = Collection::new(ids.next("col"), workspace_id, name)?;

    let mut importer = Importer {
        collection,
        ids,
        folders: Vec::new(),
        requests: Vec::new(),
        warnings: Vec::new(),
    };

    let collection_label = format!("collection '{name}'");
    importer.note_behavior(root.get("protocolProfileBehavior"), &collection_label);
    let behavior = merge_behavior(&Map::new(), root.get("protocolProfileBehavior"));
    let info = root.get("info").and_then(Value::as_object);
    importer.collection.description =
        parse_description(info.and_then(|info| info.get("description")));
    let (pre_request_script, test_script) = parse_events(root.get("event"));
    importer.collection.pre_request_script = pre_request_script;
    importer.collection.test_script = test_script;
    importer.collection.auth =
        parse_auth(root.get("auth"), &mut importer.warnings, &collection_label);
    importer.import_variables(root.get("variable"))?;
    importer.import_items(root.get("item"), None, &behavior)?;

    let variables = importer.collection.variables.clone();
    Ok(PostmanImport {
        collection: importer.collection,
        folders: importer.folders,
        requests: importer.requests,
        variables,
        warnings: importer.warnings,
    })
}

struct Importer {
    collection: Collection,
    ids: IdGenerator,
    folders: Vec<Folder>,
    requests: Vec<Request>,
    warnings: Vec<String>,
}

impl Importer {
    /// Imports `items` under `parent_folder_id`; `behavior` is the `protocolProfileBehavior` the
    /// collection and the enclosing folders set, which each request inherits.
    fn import_items(
        &mut self,
        items: Option<&Value>,
        parent_folder_id: Option<&str>,
        behavior: &Map<String, Value>,
    ) -> Result<(), PostmanError> {
        let items = match items {
            None | Some(Value::Null) => return Ok(()),
            Some(Value::Array(items)) => items,
            Some(_) => {
                self.warnings
                    .push("collection item list is not an array; ignored".to_string());
                return Ok(());
            }
        };

        for (position, item) in items.iter().enumerate() {
            let position = position as i64;
            let Some(object) = item.as_object() else {
                self.warnings
                    .push("collection item is not an object; skipped".to_string());
                continue;
            };
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty());
            let has_request = object
                .get("request")
                .is_some_and(|request| request.is_object() || request.is_string());

            if has_request {
                let Some(name) = name else {
                    self.warnings
                        .push("request without a name skipped".to_string());
                    continue;
                };
                self.import_request(object, name, parent_folder_id, position, behavior)?;
            } else if object.contains_key("item") {
                let Some(name) = name else {
                    self.warnings
                        .push("folder without a name skipped together with its items".to_string());
                    continue;
                };
                let label = format!("folder '{name}'");
                self.note_behavior(object.get("protocolProfileBehavior"), &label);
                let folder_behavior =
                    merge_behavior(behavior, object.get("protocolProfileBehavior"));

                let folder_id = self.ids.next("fold");
                let collection_id = self.collection.id.as_str();
                let mut folder = match parent_folder_id {
                    Some(parent) => {
                        Folder::new(&folder_id, collection_id, name)?.with_parent(parent)?
                    }
                    None => Folder::new(&folder_id, collection_id, name)?,
                };
                folder.description = parse_description(object.get("description"));
                folder.auth = parse_auth(object.get("auth"), &mut self.warnings, &label);
                let (pre_request_script, test_script) = parse_events(object.get("event"));
                folder.pre_request_script = pre_request_script;
                folder.test_script = test_script;
                folder.position = position;
                self.folders.push(folder);
                self.import_items(object.get("item"), Some(&folder_id), &folder_behavior)?;
            } else {
                let label = name.unwrap_or("<unnamed>");
                self.warnings.push(format!(
                    "item '{label}' has neither a request nor nested items; skipped"
                ));
            }
        }

        Ok(())
    }

    fn import_request(
        &mut self,
        item: &Map<String, Value>,
        name: &str,
        folder_id: Option<&str>,
        position: i64,
        behavior: &Map<String, Value>,
    ) -> Result<(), PostmanError> {
        let label = format!("request '{name}'");
        self.note_behavior(item.get("protocolProfileBehavior"), &label);
        let mut behavior = merge_behavior(behavior, item.get("protocolProfileBehavior"));

        let request_value = item.get("request");
        let request_object = request_value.and_then(Value::as_object);
        let string_url = request_value.and_then(Value::as_str).map(str::to_string);
        if request_value.is_some() && request_object.is_none() && string_url.is_none() {
            self.warnings.push(format!(
                "{label}: request object is missing or invalid; skipped"
            ));
            return Ok(());
        }

        if let Some(request) = request_object {
            self.note_behavior(request.get("protocolProfileBehavior"), &label);
            behavior = merge_behavior(&behavior, request.get("protocolProfileBehavior"));
        }
        let examples = import_examples(item.get("response"), &mut self.warnings, &label);

        let parsed_url = match (&string_url, request_object) {
            (Some(raw), _) => {
                let (base, query_string) = split_raw_url(raw.trim());
                Some((base, key_values_from_query_string(query_string), Vec::new()))
            }
            (None, Some(request)) => parse_url(request.get("url"), &mut self.warnings, &label),
            (None, None) => None,
        };
        let Some((url, query_params, path_variables)) = parsed_url else {
            self.warnings
                .push(format!("{label}: missing or invalid url; skipped"));
            return Ok(());
        };
        if url.trim().is_empty() {
            self.warnings
                .push(format!("{label} has no URL and was skipped"));
            return Ok(());
        }

        let (method_value, header_value, body_value, auth_value) = match request_object {
            Some(request) => (
                request.get("method"),
                request.get("header"),
                request.get("body"),
                request.get("auth"),
            ),
            None => (None, None, None, None),
        };

        let method = parse_method(method_value, &mut self.warnings, &label);
        let headers = key_values_from_array(header_value);
        let mut dropped_local_path = false;
        let body = parse_body(
            body_value,
            &mut self.warnings,
            &label,
            &mut dropped_local_path,
        );
        if dropped_local_path {
            self.warnings.push(local_path_warning(&label));
        }
        let auth = parse_auth(auth_value, &mut self.warnings, &label);

        let mut description = parse_description(item.get("description"));
        if description.is_empty() {
            description =
                parse_description(request_object.and_then(|request| request.get("description")));
        }
        let (mut pre_request_script, mut test_script) = parse_events(item.get("event"));
        if pre_request_script.is_empty()
            && test_script.is_empty()
            && let Some(request) = request_object
        {
            (pre_request_script, test_script) = parse_events(request.get("event"));
        }
        let (test_script, assertions) = take_assertions(&test_script, &mut self.warnings, &label);

        let request_id = self.ids.next("req");
        let mut request = Request::new(
            &request_id,
            self.collection.workspace_id.as_str(),
            name,
            &url,
        )?;
        request = request.with_collection(self.collection.id.as_str())?;
        if let Some(folder_id) = folder_id {
            request = request.with_folder(folder_id)?;
        }
        request.method = method;
        request.query_params = query_params;
        request.path_variables = path_variables;
        request.headers = headers;
        request.auth = auth;
        request.body = body;
        request.description = description;
        request.pre_request_script = pre_request_script;
        request.test_script = test_script;
        request.assertions = assertions;
        request.examples = examples;
        // Unknown keys were reported where they were declared.
        request.network_options = network_options_from_behavior(&behavior, &mut Vec::new(), &label);
        request.position = position;
        if matches!(request.body, RequestBody::Graphql { .. }) {
            request.protocol = ProtocolKind::Graphql;
        }
        self.requests.push(request);

        Ok(())
    }

    fn import_variables(&mut self, value: Option<&Value>) -> Result<(), PostmanError> {
        let entries = match value {
            None | Some(Value::Null) => return Ok(()),
            Some(Value::Array(entries)) => entries,
            Some(_) => {
                self.warnings
                    .push("collection variable list is not an array; ignored".to_string());
                return Ok(());
            }
        };

        for entry in entries {
            let Some(object) = entry.as_object() else {
                self.warnings
                    .push("collection variable is not an object; skipped".to_string());
                continue;
            };
            let key = object
                .get("key")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|key| !key.is_empty());
            let Some(key) = key else {
                self.warnings
                    .push("collection variable with a blank key skipped".to_string());
                continue;
            };
            let variable_value = match object.get("value") {
                None | Some(Value::Null) => String::new(),
                Some(value) => match scalar_string(value) {
                    Some(text) => text,
                    None => {
                        self.warnings.push(format!(
                            "collection variable '{key}' has a non-scalar value; skipped"
                        ));
                        continue;
                    }
                },
            };
            let enabled = !object
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let is_secret_type = object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|value_type| value_type.eq_ignore_ascii_case("secret"));
            let mut variable = if is_secret_type {
                EnvironmentVariable::masked(key, variable_value)?
            } else {
                EnvironmentVariable::plain(key, variable_value)?
            };
            variable.enabled = enabled;
            self.collection.variables.push(variable);
        }

        Ok(())
    }

    /// Reports the `protocolProfileBehavior` keys of one level that SpanBay has no setting for.
    fn note_behavior(&mut self, value: Option<&Value>, label: &str) {
        if let Some(object) = value.and_then(Value::as_object) {
            network_options_from_behavior(object, &mut self.warnings, label);
        }
    }
}

struct IdGenerator {
    slug: String,
    next_index: u32,
}

impl IdGenerator {
    fn new(collection_name: &str) -> Self {
        Self {
            slug: slugify(collection_name),
            next_index: 1,
        }
    }

    fn next(&mut self, prefix: &str) -> String {
        let index = self.next_index;
        self.next_index += 1;
        format!("{prefix}_{}_{}", self.slug, index)
    }
}

fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_was_separator = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator && !slug.is_empty() {
            slug.push('_');
            last_was_separator = true;
        }
        if slug.len() >= MAX_SLUG_LEN {
            break;
        }
    }

    let slug = slug.trim_end_matches('_');
    if slug.is_empty() {
        "collection".to_string()
    } else {
        slug.to_string()
    }
}
