//! Model-list use cases of a provider: the read-only refresh and its drift, "Mark as seen",
//! capability overrides, and bulk enable and disable with Undo.
//!
//! Single responsibility: glue the store's model functions to the app. The refresh fetches the
//! list through the safe transport with the provider's own origin and auth style and parses it
//! with the sniffer's tolerant parser; it never adds, enables or deletes a row.

use std::collections::{BTreeSet, HashSet};
use std::time::Duration;

use super::{
    AppState, SAVED_PROVIDER_ALLOW_PRIVATE, ensure_catalog, safe_http_for, with_connection,
    with_db_blocking,
};
use crate::app::current_time_ms;
use crate::cancel::CancelToken;
use crate::error::{AppError, ErrorCode};
use crate::llm::capabilities::ModelCapabilities;
use crate::llm::catalog::CatalogIndex;
use crate::llm::events::LlmEvent;
use crate::llm::probe::listing::{KeyListFailure, ModelDrift};
use crate::llm::safe_http::{SafeHttp, SafeMethod, SafeRequest, SafeResponse, SafeTransportError};
use crate::llm::sniff::{ListCursor, MAX_PAGES, parse_list_page};
use crate::llm::store::{
    self, BulkEnableReport, ListedModel, Provider, ProviderKey, ProviderModel, UndoReport,
};
use crate::llm::wire::{AuthStyle, Protocol, Quirks, models_url, request_headers};

/// Time limit of one list request.
const LIST_TIMEOUT: Duration = Duration::from_secs(10);

/// Time limit of connection setup.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Body cap of one list page: OpenRouter's list exceeds 256 KiB.
const LIST_BODY_CAP: usize = 2 * 1024 * 1024;

/// Entries asked per page where the protocol takes a page size.
const PAGE_SIZE: &str = "1000";

/// The chat protocol whose headers authenticate the list request: the provider's primary
/// protocol, else the first of the chat preference order it speaks, else OpenAI-style.
pub(super) fn list_protocol(provider: &Provider) -> Protocol {
    let speaks = |protocol: Protocol| {
        provider
            .endpoints
            .iter()
            .any(|endpoint| endpoint.enabled && endpoint.protocol == protocol)
    };
    provider
        .primary_protocol
        .filter(|protocol| speaks(*protocol))
        .or_else(|| {
            [
                Protocol::OpenaiChat,
                Protocol::AnthropicMessages,
                Protocol::OpenaiResponses,
                Protocol::Gemini,
            ]
            .into_iter()
            .find(|protocol| speaks(*protocol))
        })
        .unwrap_or(Protocol::OpenaiChat)
}

/// The URL of one list page: the provider's list URL plus the protocol's page-size parameter and
/// the cursor of the previous page.
fn page_url(base: &str, protocol: Protocol, cursor: Option<&ListCursor>) -> String {
    let Ok(mut url) = url::Url::parse(base) else {
        return base.to_owned();
    };
    {
        let mut query = url.query_pairs_mut();
        match protocol {
            Protocol::Gemini => {
                query.append_pair("pageSize", PAGE_SIZE);
            }
            Protocol::AnthropicMessages => {
                query.append_pair("limit", PAGE_SIZE);
            }
            _ => {}
        }
        if let Some(cursor) = cursor {
            let (name, value) = cursor.query_pair();
            query.append_pair(name, value);
        }
    }
    if url.query() == Some("") {
        url.set_query(None);
    }
    url.into()
}

fn transport_error(error: &SafeTransportError) -> AppError {
    match error {
        SafeTransportError::Cancelled => AppError::cancelled(),
        SafeTransportError::Blocked { .. } => {
            AppError::validation("the provider's address is not allowed (blocked_address)")
        }
        SafeTransportError::Timeout => {
            AppError::network("the provider did not answer the model list request in time")
                .coded(ErrorCode::ModelListTimeout, None)
        }
        other => AppError::network(format!(
            "the provider did not answer the model list request ({})",
            other.kind()
        ))
        .coded(ErrorCode::ModelListTransport, Some(other.kind().to_owned())),
    }
}

/// A failed list answer, coded by what the UI tells the user to fix: the key, the list
/// address, or an address that serves a web page.
fn status_error(response: &SafeResponse) -> AppError {
    let status = response.status;
    match status {
        401 | 403 => AppError::network(format!(
            "the provider rejected the key on the model list ({status})"
        ))
        .coded(ErrorCode::ModelListUnauthorized, Some(status.to_string())),
        404 => AppError::network("the provider answered the model list request with 404")
            .coded(ErrorCode::ModelListNotFound, Some(status.to_string())),
        _ if response.looks_like_html() => AppError::network(format!(
            "the provider answered the model list request with a web page ({status})"
        ))
        .coded(ErrorCode::ModelListHtml, Some(status.to_string())),
        _ => AppError::network(format!(
            "the provider answered the model list request with {status}"
        )),
    }
}

/// A list page that does not parse: a web page is coded (`model_list_html`), any other shape is not.
fn shape_error(response: &SafeResponse) -> AppError {
    if response.looks_like_html() {
        AppError::network("the model list address answered with a web page, not a list")
            .coded(ErrorCode::ModelListHtml, None)
    } else {
        AppError::network("the model list has an unrecognised shape")
    }
}

/// The `NotFound` for a provider id that no longer exists.
pub(crate) fn provider_not_found(id: &str) -> AppError {
    AppError::not_found(format!("provider {id} not found"))
        .coded(ErrorCode::ProviderNotFound, Some(id.to_owned()))
}

/// Fetches the provider's model list with each of its keys and stores the comparison with the
/// stored rows. Read-only for the rows: it adds, enables and deletes nothing; it updates
/// the metadata of rows that exist and flags those no key lists any more.
///
/// Relays such as sub2api and new-api bind each key to a group with its own models
/// (D-258), so every key's list is read and stored as that
/// key's (`llm_key_models`): with `key_id` that key and every enabled key never read before,
/// else every enabled key. A key whose list fails keeps its previous one (in
/// [`ModelDrift::key_failures`]); the refresh fails only when every key fails.
///
/// Each list is fetched through the safe transport (no redirects, pinned addresses, 2 MiB cap per
/// page, at most 5 pages) from the provider's origin only.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown provider or key or a provider that needs a key it does not
/// have, [`AppError::Unsupported`] when the provider has no list endpoint, [`AppError::Network`]
/// for an upstream failure or an unrecognised answer, [`AppError::Cancelled`].
pub async fn llm_models_refresh_impl(
    state: &AppState,
    provider_id: &str,
    key_id: Option<&str>,
    cancel_token: Option<String>,
) -> Result<ModelDrift, AppError> {
    let id = provider_id.to_owned();
    let (base_url, proxy_url) = with_db_blocking(state, move |connection| {
        store::get_provider(connection, &id)?
            .map(|provider| (provider.base_url, provider.proxy_url))
            .ok_or_else(|| provider_not_found(&id))
    })
    .await?;
    let http = safe_http_for(state, &base_url, &proxy_url)?;
    llm_models_refresh_in(state, &http, provider_id, key_id, cancel_token).await
}

/// One key's model list.
struct KeyList {
    /// The models with their metadata.
    listed: Vec<ListedModel>,
    /// Each model with the protocols the relay declared for it.
    declared: Vec<(String, Option<Vec<Protocol>>)>,
    /// The native protocol the list's shape suggests.
    preferred: Option<Protocol>,
    /// False when a page was cut at the size cap, unreadable after the first, or the pages ran
    /// past [`MAX_PAGES`]: the list may lack models the provider still serves.
    complete: bool,
}

/// What one list request needs besides the key.
struct ListContext<'a> {
    http: &'a dyn SafeHttp,
    provider: &'a Provider,
    base: String,
    quirks: Quirks,
    index: &'a CatalogIndex,
}

/// The protocols new-api's `supported_endpoint_types` name; `None` when it names none of them.
fn declared_protocols(types: &[String]) -> Option<Vec<Protocol>> {
    let mut protocols: Vec<Protocol> = types
        .iter()
        .filter_map(|word| match word.to_ascii_lowercase().as_str() {
            "openai" => Some(Protocol::OpenaiChat),
            "openai-response" | "openai-responses" => Some(Protocol::OpenaiResponses),
            "anthropic" => Some(Protocol::AnthropicMessages),
            "gemini" => Some(Protocol::Gemini),
            "embeddings" => Some(Protocol::Embeddings),
            _ => None,
        })
        .collect();
    protocols.sort_unstable();
    protocols.dedup();
    (!protocols.is_empty()).then_some(protocols)
}

/// The native protocol a list body's shape suggests: Anthropic's entries (`type: "model"` with a
/// `display_name`, as sub2api answers for a Claude group) or Gemini's (`models[]` with
/// `supportedGenerationMethods`). An OpenAI-shaped list says nothing: relays answer it for chat,
/// Responses and converted groups alike.
fn list_shape_protocol(body: &[u8]) -> Option<Protocol> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let first = |field: &str| {
        value
            .get(field)
            .and_then(serde_json::Value::as_array)
            .and_then(|items| items.first())
            .cloned()
    };
    if let Some(entry) = first("data")
        && entry.get("type").and_then(serde_json::Value::as_str) == Some("model")
        && entry.get("display_name").is_some()
        && entry.get("object").is_none()
    {
        return Some(Protocol::AnthropicMessages);
    }
    first("models")
        .filter(|entry| entry.get("supportedGenerationMethods").is_some())
        .map(|_| Protocol::Gemini)
}

/// The protocol whose headers authenticate `key`'s list request: the first chat protocol the
/// user limited the key to, else the key's native protocol, when the provider speaks it; else
/// the provider's list protocol.
fn key_list_protocol(provider: &Provider, key: Option<&ProviderKey>) -> Protocol {
    let speaks = |protocol: &Protocol| {
        protocol.is_chat()
            && provider
                .endpoints
                .iter()
                .any(|endpoint| endpoint.enabled && endpoint.protocol == *protocol)
    };
    key.and_then(|key| {
        key.protocols
            .as_deref()
            .and_then(|limit| limit.iter().copied().find(speaks))
            .or(key.preferred_protocol.filter(speaks))
    })
    .unwrap_or_else(|| list_protocol(provider))
}

/// Reads one key's list. A first page rejected (401/403) with headers that carried no
/// `x-api-key` is asked once more the Anthropic way (`Authorization` and `x-api-key`), with
/// headers that carried no `Authorization` once more the OpenAI way: relays disagree on the
/// header a key travels in, and a key's group can decide it.
async fn read_key_list(
    context: &ListContext<'_>,
    key: Option<&ProviderKey>,
    secret: &str,
    token: &CancelToken,
) -> Result<KeyList, AppError> {
    let mut protocol = key_list_protocol(context.provider, key);
    let auth_for = |protocol: Protocol| {
        context
            .provider
            .endpoints
            .iter()
            .find(|endpoint| endpoint.enabled && endpoint.protocol == protocol)
            .and_then(|endpoint| endpoint.auth)
            .unwrap_or(context.provider.auth)
    };
    let mut headers = request_headers(protocol, auth_for(protocol), secret, &context.quirks);
    let lookup = |id: &str| context.index.lookup(id);
    let mut list = KeyList {
        listed: Vec::new(),
        declared: Vec::new(),
        preferred: None,
        complete: true,
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut cursor: Option<ListCursor> = None;
    let mut retried = secret.trim().is_empty();
    let mut page = 0;
    while page < MAX_PAGES {
        let request = SafeRequest {
            method: SafeMethod::Get,
            url: page_url(&context.base, protocol, cursor.as_ref()),
            headers: headers.clone(),
            body: None,
            timeout: LIST_TIMEOUT,
            connect_timeout: CONNECT_TIMEOUT,
            max_body: LIST_BODY_CAP,
            allow_private: SAVED_PROVIDER_ALLOW_PRIVATE,
        };
        let response = context
            .http
            .send(request, token)
            .await
            .map_err(|error| transport_error(&error))?;
        if !response.is_success() {
            if page == 0 && !retried && matches!(response.status, 401 | 403) {
                retried = true;
                let has = |name: &str| headers.iter().any(|(header, _)| header == name);
                let other = if has("x-api-key") {
                    (!has("authorization")).then_some(Protocol::OpenaiChat)
                } else {
                    Some(Protocol::AnthropicMessages)
                };
                if let Some(other) = other {
                    protocol = other;
                    headers = request_headers(other, AuthStyle::Bearer, secret, &context.quirks);
                    continue;
                }
            }
            return Err(status_error(&response));
        }
        list.complete &= !response.truncated;
        let Some(parsed) = parse_list_page(&response.body, response.truncated, &lookup) else {
            if page == 0 {
                return Err(shape_error(&response));
            }
            list.complete = false;
            break;
        };
        if page == 0 && !response.truncated {
            list.preferred = list_shape_protocol(&response.body);
        }
        for model in parsed.models {
            if seen.insert(model.id.clone()) {
                list.declared.push((
                    model.id.clone(),
                    declared_protocols(&model.declared_endpoint_types),
                ));
                list.listed.push(ListedModel {
                    id: model.id,
                    context: model.context_window,
                    max_output: model.max_output,
                    capabilities: model.capabilities,
                });
            }
        }
        cursor = parsed.next;
        if cursor.is_none() {
            break;
        }
        page += 1;
    }
    list.complete &= cursor.is_none();
    Ok(list)
}

/// Records how reading `key_id`'s list ended: a success or a cancel forgets the key, a failure
/// keeps its code for the key's `models_listing`.
fn note_listing(state: &AppState, key_id: &str, error: Option<&AppError>) {
    let mut listing = state
        .llm
        .services
        .key_listing
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match error.filter(|error| !error.is_cancelled()) {
        Some(error) => {
            let code = error
                .code()
                .and_then(|code| serde_json::to_value(code).ok())
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            listing.insert(key_id.to_owned(), Some(code));
        }
        None => {
            listing.remove(key_id);
        }
    }
}

/// Marks `key_id`'s list as being read (its `models_listing` says `reading` until the read
/// ends), or, with `reading = false`, drops a mark no read cleared (the refresh failed before it
/// reached the key).
pub(crate) fn mark_listing(state: &AppState, key_id: &str, reading: bool) {
    let mut listing = state
        .llm
        .services
        .key_listing
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if reading {
        listing.insert(key_id.to_owned(), None);
    } else if listing.get(key_id).is_some_and(Option::is_none) {
        listing.remove(key_id);
    }
}

/// [`llm_models_refresh_impl`] over an explicit transport (tests script it).
pub(crate) async fn llm_models_refresh_in(
    state: &AppState,
    http: &dyn SafeHttp,
    provider_id: &str,
    key_id: Option<&str>,
    cancel_token: Option<String>,
) -> Result<ModelDrift, AppError> {
    let guard = state.cancellations.register(cancel_token.as_deref())?;
    let (provider_id_owned, key_id_owned) = (provider_id.to_owned(), key_id.map(str::to_owned));
    let (provider, endpoint, keys, extra_headers, index) =
        with_db_blocking(state, move |connection| {
            ensure_catalog(connection)?;
            let provider_id = provider_id_owned.as_str();
            let key_id = key_id_owned.as_deref();
            let provider = store::get_provider(connection, provider_id)?
                .ok_or_else(|| provider_not_found(provider_id))?;
            let endpoint = provider.models_endpoint.clone().ok_or_else(|| {
                AppError::unsupported(format!(
                    "{} has no model list endpoint: add models by hand",
                    provider.name
                ))
            })?;
            if let Some(id) = key_id
                && store::get_key(connection, id)?.is_none_or(|key| key.provider_id != provider.id)
            {
                return Err(AppError::not_found("the provider has no usable key"));
            }
            let mut keys: Vec<(Option<ProviderKey>, String)> = Vec::new();
            for key in store::list_keys(connection, Some(&provider.id))? {
                // The asked key, plus the enabled keys whose list was never read: the union of
                // the stored lists then covers every key.
                let wanted = match key_id {
                    Some(id) => key.id == id || (key.enabled && key.models_listed_at_ms.is_none()),
                    None => key.enabled,
                };
                if wanted && let Some(resolved) = store::get_resolved_key(connection, &key.id)? {
                    keys.push((Some(resolved.key), resolved.secret));
                }
            }
            if keys.is_empty() {
                if key_id.is_none() && provider.auth == AuthStyle::None {
                    keys.push((None, String::new()));
                } else {
                    return Err(AppError::not_found("the provider has no usable key"));
                }
            }
            let headers = store::resolve_provider_headers(connection, &provider)?;
            Ok((
                provider,
                endpoint,
                keys,
                headers,
                CatalogIndex::load(connection)?,
            ))
        })
        .await?;
    let mut quirks = provider.quirks.clone();
    quirks.extra_headers = extra_headers;
    let context = ListContext {
        http,
        provider: &provider,
        base: models_url(&provider.base_url, &endpoint),
        quirks,
        index: &index,
    };
    let mut fresh: Vec<(Option<String>, KeyList)> = Vec::new();
    let mut failures: Vec<KeyListFailure> = Vec::new();
    let mut first_error: Option<AppError> = None;
    for (key, secret) in &keys {
        let result = read_key_list(&context, key.as_ref(), secret, guard.token()).await;
        if let Some(key) = key {
            note_listing(state, &key.id, result.as_ref().err());
        }
        match result {
            Ok(list) => fresh.push((key.as_ref().map(|key| key.id.clone()), list)),
            Err(error) if error.is_cancelled() => return Err(error),
            Err(error) => {
                if let Some(key) = key {
                    failures.push(KeyListFailure {
                        key_id: key.id.clone(),
                        message: error.to_string(),
                    });
                }
                first_error.get_or_insert(error);
            }
        }
    }
    if fresh.is_empty() {
        return Err(first_error.unwrap_or_else(|| AppError::network("no key could read the list")));
    }
    // A failed key that was never read leaves the union incomplete: keep every stored row listed.
    let keep_stored = keys.iter().any(|(key, _)| {
        key.as_ref().is_some_and(|key| {
            key.models_listed_at_ms.is_none() && failures.iter().any(|f| f.key_id == key.id)
        })
    });
    let complete = fresh.iter().all(|(_, list)| list.complete);
    let id = provider_id.to_owned();
    let mut drift = with_db_blocking(state, move |connection| {
        let now = current_time_ms();
        let mut listed: Vec<ListedModel> = Vec::new();
        let mut refreshed: HashSet<String> = HashSet::new();
        let mut confirmed: BTreeSet<String> = BTreeSet::new();
        for (key_id, list) in fresh {
            confirmed.extend(list.listed.iter().map(|model| model.id.clone()));
            if let Some(key_id) = key_id {
                store::replace_key_models(
                    connection,
                    &key_id,
                    &list.declared,
                    list.preferred,
                    now,
                )?;
                refreshed.insert(key_id);
            }
            listed.extend(list.listed);
        }
        // The other keys' stored lists (not asked, or failed this time) still count.
        for row in store::list_key_models(connection, Some(&id))? {
            if !refreshed.contains(&row.key_id) {
                listed.push(ListedModel {
                    id: row.model_id,
                    ..ListedModel::default()
                });
            }
        }
        if keep_stored {
            for model in store::list_models(connection, Some(&id))? {
                listed.push(ListedModel {
                    id: model.model_id,
                    ..ListedModel::default()
                });
            }
        }
        let drift = store::apply_model_listing(connection, &id, &listed, complete, now)?;
        store::confirm_listed(connection, &id, &confirmed, now)?;
        Ok(drift)
    })
    .await?;
    drift.key_failures = failures;
    // The listing rewrites capability bits and limits the routing snapshot reads.
    state.llm.touch();
    state.sync.notify_changed();
    state.llm.emit(LlmEvent::ModelsChanged {
        provider_id: provider_id.to_owned(),
    });
    state.llm.emit(LlmEvent::KeysChanged {
        provider_id: provider_id.to_owned(),
    });
    state.llm.emit(LlmEvent::ProvidersChanged);
    Ok(drift)
}

/// "Mark as seen": clears the stored drift and the "New" chips; the "Not in provider"
/// flags stay.
///
/// # Errors
/// [`AppError::NotFound`] when the provider does not exist.
pub fn llm_models_drift_ack_impl(state: &AppState, provider_id: &str) -> Result<(), AppError> {
    with_connection(state, |connection| {
        if store::get_provider(connection, provider_id)?.is_none() {
            return Err(provider_not_found(provider_id));
        }
        store::ack_model_drift(connection, provider_id, current_time_ms())
    })?;
    state.llm.emit(LlmEvent::ModelsChanged {
        provider_id: provider_id.to_owned(),
    });
    state.llm.emit(LlmEvent::ProvidersChanged);
    Ok(())
}

/// Applies the user's capability overrides from the Settings matrix (a `UserOverride` state is
/// stored, an `Unknown` state resets the override, anything else is ignored) and returns the row.
///
/// # Errors
/// [`AppError::NotFound`] when the model does not exist.
pub fn llm_model_capabilities_set_impl(
    state: &AppState,
    provider_id: &str,
    model_id: &str,
    overrides: ModelCapabilities,
) -> Result<ProviderModel, AppError> {
    let model = with_connection(state, |connection| {
        store::set_model_capabilities_override(connection, provider_id, model_id, &overrides)
    })?;
    state.llm.touch();
    state.sync.notify_changed();
    state.llm.emit(LlmEvent::ModelsChanged {
        provider_id: provider_id.to_owned(),
    });
    Ok(model)
}

/// Switches models on or off in bulk (or one row), logging the previous values for Undo (the newest
/// 5 operations). A model without a row or that already has the value is
/// skipped.
///
/// # Errors
/// [`AppError::NotFound`] when the provider does not exist.
pub fn llm_models_set_enabled_impl(
    state: &AppState,
    provider_id: &str,
    changes: &[(String, bool)],
) -> Result<BulkEnableReport, AppError> {
    let report = with_connection(state, |connection| {
        if store::get_provider(connection, provider_id)?.is_none() {
            return Err(provider_not_found(provider_id));
        }
        store::set_models_enabled_bulk(connection, provider_id, changes, current_time_ms())
    })?;
    if !report.previous.is_empty() {
        state.llm.touch();
        state.sync.notify_changed();
        state.llm.emit(LlmEvent::ModelsChanged {
            provider_id: provider_id.to_owned(),
        });
    }
    Ok(report)
}

/// Undoes a bulk switch or an applied probe run: every model whose flag still has the value the
/// operation wrote gets its previous value back; a model edited since is left alone.
///
/// # Errors
/// [`AppError::NotFound`] when the operation is unknown, already undone or older than the newest 5.
pub fn llm_models_undo_impl(state: &AppState, op_id: &str) -> Result<UndoReport, AppError> {
    let (provider_id, report) = with_connection(state, |connection| {
        let provider_id = store::get_probe_apply(connection, op_id)?
            .map(|record| record.provider_id)
            .ok_or_else(|| AppError::not_found(format!("operation {op_id} not found")))?;
        Ok((provider_id, store::undo_bulk(connection, op_id)?))
    })?;
    state.llm.touch();
    state.sync.notify_changed();
    state.llm.emit(LlmEvent::ModelsChanged { provider_id });
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::safe_http::scripted::{Reply, ScriptedHttp};
    use crate::test_support::{deepseek, open};

    async fn refresh_with(reply: Reply) -> AppError {
        let state = open();
        let (provider, _) = deepseek(&state);
        let http = ScriptedHttp::new().route(SafeMethod::Get, "/v1/models", vec![reply]);
        llm_models_refresh_in(&state, &http, &provider.id, None, None)
            .await
            .expect_err("the list fails")
    }

    #[tokio::test]
    async fn a_failed_model_list_carries_the_code_the_ui_branches_on() {
        let html = "<!doctype html><title>Sign in</title>";
        let cases = [
            (
                Reply::json(401, &serde_json::json!({})),
                Some(ErrorCode::ModelListUnauthorized),
            ),
            (
                Reply::json(404, &serde_json::json!({})),
                Some(ErrorCode::ModelListNotFound),
            ),
            (
                Reply::text(200, "text/html", html),
                Some(ErrorCode::ModelListHtml),
            ),
            (
                Reply::Error(SafeTransportError::Timeout),
                Some(ErrorCode::ModelListTimeout),
            ),
            (
                Reply::Error(SafeTransportError::Dns),
                Some(ErrorCode::ModelListTransport),
            ),
            (Reply::json(502, &serde_json::json!({})), None),
        ];
        for (reply, code) in cases {
            let label = format!("{reply:?}");
            assert_eq!(refresh_with(reply).await.code(), code, "{label}");
        }
    }

    #[tokio::test]
    async fn an_unknown_provider_is_coded_provider_not_found() {
        let state = open();
        let error = llm_models_refresh_in(&state, &ScriptedHttp::new(), "gone", None, None)
            .await
            .expect_err("no provider");
        assert_eq!(error.code(), Some(ErrorCode::ProviderNotFound));
        assert!(matches!(error, AppError::NotFound { .. }));
    }
}
