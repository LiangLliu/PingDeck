//! Data-bucket backed `Crud` routes: list, read, create, replace and delete items.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::atomic::Ordering;

use serde_json::Value;

use crate::workspace::items::{MockRoute, MockRouteType, MockServer, json_value_to_string};

use super::http::ParsedRequest;
use super::logs::now_ms;
use super::reply::MockReply;
use super::routing::{decode_percent, method_matches};
use super::server::{ServerState, lock};

/// Items a bucket may hold; a `POST` beyond this is refused with `507`.
const MAX_CRUD_ITEMS: usize = 10_000;

/// Serialized size a bucket may reach through `POST`, `PUT` and `PATCH`; beyond it the write is
/// refused with `413`.
const MAX_CRUD_BYTES: usize = 16 * 1024 * 1024;

/// The live data buckets of a server, with the serialized size of each one that CRUD wrote to
/// kept by delta, so a write does not re-serialize a bucket of up to [`MAX_CRUD_BYTES`].
#[derive(Debug, Default)]
pub(super) struct LiveBuckets {
    values: HashMap<String, Value>,
    /// Serialized bytes of a bucket, measured on its first CRUD access after it was (re)seeded.
    bytes: HashMap<String, usize>,
}

impl LiveBuckets {
    /// Buckets holding `values`.
    pub(super) fn new(values: HashMap<String, Value>) -> Self {
        Self {
            values,
            bytes: HashMap::new(),
        }
    }

    /// The contents of bucket `id`.
    pub(super) fn get(&self, id: &str) -> Option<&Value> {
        self.values.get(id)
    }

    /// Whether bucket `id` exists.
    pub(super) fn contains_key(&self, id: &str) -> bool {
        self.values.contains_key(id)
    }

    /// Seeds bucket `id` with `value`, forgetting its measured size.
    pub(super) fn insert(&mut self, id: String, value: Value) {
        self.bytes.remove(&id);
        self.values.insert(id, value);
    }

    /// Keeps the buckets whose id `keep` accepts.
    pub(super) fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.values.retain(|id, _| keep(id));
        self.bytes.retain(|id, _| keep(id));
    }

    /// A copy of every bucket's contents keyed by id.
    pub(super) fn snapshot(&self) -> HashMap<String, Value> {
        self.values.clone()
    }

    /// Bucket `id` with its serialized size, measured now when this is the first access.
    fn tracked(&mut self, id: &str) -> Option<(&mut Value, &mut usize)> {
        let value = self.values.get_mut(id)?;
        if !self.bytes.contains_key(id) {
            self.bytes.insert(id.to_owned(), serialized_len(&*value));
        }
        Some((value, self.bytes.get_mut(id)?))
    }
}

/// Finds an enabled `Crud` route for `path`, returning the trailing item id when
/// the path addresses one item (`/users/42`) instead of the collection.
pub(super) fn find_crud_route<'a>(
    config: &'a MockServer,
    method: &str,
    path: &str,
) -> Option<(&'a MockRoute, Option<String>)> {
    let mut routes: Vec<&MockRoute> = config
        .routes
        .iter()
        .filter(|route| {
            route.enabled
                && route.route_type == MockRouteType::Crud
                && method_matches(&route.method, method)
        })
        .collect();
    routes.sort_by_key(|route| route.position);

    let trimmed = path.trim_end_matches('/');
    for route in routes {
        let base = route.path.trim_end_matches('/');
        if trimmed == base {
            return Some((route, None));
        }
        let Some(rest) = trimmed.strip_prefix(base) else {
            continue;
        };
        let Some(item) = rest.strip_prefix('/') else {
            continue;
        };
        if item.is_empty() || item.contains('/') {
            continue;
        }
        return Some((route, Some(decode_percent(item))));
    }

    None
}

/// Serves one CRUD request against the live bucket of `route`.
pub(super) fn crud_reply(
    state: &ServerState,
    route: &MockRoute,
    item_id: Option<&str>,
    request: &ParsedRequest,
    body_json: Option<&Value>,
) -> MockReply {
    let route_id = Some(route.id.clone());
    let id_key = {
        let trimmed = route.crud_id_key.trim();
        if trimmed.is_empty() { "id" } else { trimmed }
    };

    let mut buckets = lock(&state.buckets);
    let Some((bucket, bucket_bytes)) = buckets.tracked(&route.data_bucket_id) else {
        return MockReply::json(
            404,
            &serde_json::json!({
                "error": "data_bucket_not_found",
                "bucket": route.data_bucket_id,
            }),
            route_id,
        );
    };
    let Some(items) = bucket.as_array_mut() else {
        return MockReply::json(
            400,
            &serde_json::json!({ "error": "data bucket is not a JSON array" }),
            route_id,
        );
    };

    match (request.method.as_str(), item_id) {
        ("GET", None) => {
            let filtered = filter_crud_items(items, &request.query);
            MockReply::json(200, &Value::Array(filtered), route_id)
        }
        ("GET", Some(id)) => match find_crud_item(items, id_key, id) {
            Some(index) => MockReply::json(200, &items[index], route_id),
            None => MockReply::json(404, &serde_json::json!({"error": "Not found"}), route_id),
        },
        ("POST", None) => {
            let Some(object) = body_json.and_then(Value::as_object) else {
                return MockReply::json(
                    400,
                    &serde_json::json!({ "error": "request body must be a JSON object" }),
                    route_id,
                );
            };
            let mut object = object.clone();
            // `null` and blank ids count as missing, like an absent key.
            let has_id = object.get(id_key).is_some_and(|value| {
                !value.is_null() && !json_value_to_string(value).trim().is_empty()
            });
            if !has_id {
                object.insert(id_key.to_owned(), next_crud_id(state, items, id_key));
            }
            let created = Value::Object(object);
            if items.len() >= MAX_CRUD_ITEMS {
                return MockReply::json(
                    507,
                    &serde_json::json!({ "error": "data_bucket_full", "max_items": MAX_CRUD_ITEMS }),
                    route_id,
                );
            }
            // The element plus the comma that separates it from the previous one.
            let added = serialized_len(&created) + usize::from(!items.is_empty());
            if bucket_bytes.saturating_add(added) > MAX_CRUD_BYTES {
                return bucket_too_large(route_id);
            }
            *bucket_bytes += added;
            items.push(created.clone());
            MockReply::json(201, &created, route_id)
        }
        ("PUT" | "PATCH", Some(id)) => {
            let Some(index) = find_crud_item(items, id_key, id) else {
                return MockReply::json(404, &serde_json::json!({"error": "Not found"}), route_id);
            };
            let Some(object) = body_json.and_then(Value::as_object) else {
                return MockReply::json(
                    400,
                    &serde_json::json!({ "error": "request body must be a JSON object" }),
                    route_id,
                );
            };
            let updated = if request.method == "PATCH" {
                let mut merged = items[index].as_object().cloned().unwrap_or_default();
                for (key, value) in object {
                    merged.insert(key.clone(), value.clone());
                }
                Value::Object(merged)
            } else {
                let mut replaced = object.clone();
                if !replaced.contains_key(id_key)
                    && let Some(existing) = items[index].get(id_key)
                {
                    replaced.insert(id_key.to_owned(), existing.clone());
                }
                Value::Object(replaced)
            };
            let total = bucket_bytes
                .saturating_sub(serialized_len(&items[index]))
                .saturating_add(serialized_len(&updated));
            if total > MAX_CRUD_BYTES {
                return bucket_too_large(route_id);
            }
            *bucket_bytes = total;
            items[index] = updated;
            MockReply::json(200, &items[index], route_id)
        }
        ("DELETE", Some(id)) => match find_crud_item(items, id_key, id) {
            Some(index) => {
                let removed = items.remove(index);
                // The element plus one comma (a single-element array has none, and is now empty).
                *bucket_bytes = if items.is_empty() {
                    2
                } else {
                    bucket_bytes.saturating_sub(serialized_len(&removed) + 1)
                };
                MockReply::json(200, &serde_json::json!({"deleted": true}), route_id)
            }
            None => MockReply::json(404, &serde_json::json!({"error": "Not found"}), route_id),
        },
        _ => MockReply::json(
            405,
            &serde_json::json!({ "error": "method_not_allowed" }),
            route_id,
        ),
    }
}

fn find_crud_item(items: &[Value], id_key: &str, id: &str) -> Option<usize> {
    items.iter().position(|item| {
        item.get(id_key)
            .map(json_value_to_string)
            .is_some_and(|value| value == id)
    })
}

/// Applies the `?key=value` filters and `?_limit=N` of a CRUD collection read.
fn filter_crud_items(items: &[Value], query: &str) -> Vec<Value> {
    let mut limit = None;
    let mut filters: Vec<(String, String)> = Vec::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if key == "_limit" {
            limit = value.parse::<usize>().ok();
        } else if !key.starts_with('_') {
            filters.push((key.into_owned(), value.into_owned()));
        }
    }

    let mut filtered: Vec<Value> = items
        .iter()
        .filter(|item| {
            filters.iter().all(|(key, expected)| {
                item.get(key)
                    .map(json_value_to_string)
                    .is_some_and(|actual| &actual == expected)
            })
        })
        .cloned()
        .collect();
    if let Some(limit) = limit {
        filtered.truncate(limit);
    }

    filtered
}

/// The `413` reply for a write that would push a bucket past [`MAX_CRUD_BYTES`].
fn bucket_too_large(route_id: Option<String>) -> MockReply {
    MockReply::json(
        413,
        &serde_json::json!({ "error": "data_bucket_too_large", "max_bytes": MAX_CRUD_BYTES }),
        route_id,
    )
}

/// JSON-serialized size of `value`, counted without building the string.
fn serialized_len(value: &(impl serde::Serialize + ?Sized)) -> usize {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(buf.len());
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    // A `Value` always serializes; counting cannot fail.
    let _ = serde_json::to_writer(&mut counter, value);
    counter.0
}

/// An id no item of the bucket uses, in either numeric or string form (an item with the string
/// id `"8"` holds the id `8`): the next number after the largest numeric id, otherwise a unique
/// timestamped string.
pub(super) fn next_crud_id(state: &ServerState, items: &[Value], id_key: &str) -> Value {
    let taken: HashSet<String> = items
        .iter()
        .filter_map(|item| item.get(id_key))
        .map(json_value_to_string)
        .collect();
    let mut next = items
        .iter()
        .filter_map(|item| item.get(id_key).and_then(Value::as_i64))
        .max()
        .and_then(|max| max.checked_add(1));
    while let Some(candidate) = next {
        if !taken.contains(&candidate.to_string()) {
            return Value::from(candidate);
        }
        next = candidate.checked_add(1);
    }

    loop {
        let sequence = state
            .sequence
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        let candidate = format!("{}-{sequence}", now_ms());
        if !taken.contains(&candidate) {
            return Value::String(candidate);
        }
    }
}
