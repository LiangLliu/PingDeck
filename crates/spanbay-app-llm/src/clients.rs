//! Per-client gateway tokens: list, create, update, rotate, revoke, delete.
//!
//! Single responsibility: the shell-facing calls over `llm::store::clients`. A token is returned
//! once, by create and by "rotate and copy"; only its SHA-256 and a masked hint are stored. Every
//! change refreshes the gateway's token cache at once (a revoked or rotated token gets 401 on
//! the next request), turns "Serve on local network" off when the last active client goes
//! and pushes the new gateway status (the active client count). Connecting a
//! tool's config file (which creates or rotates clients too) is `client_writers`.

use super::{current_time_ms, with_connection};
use crate::app::{AppState, settings_update_impl};
use crate::error::AppError;
use crate::llm::events::LlmEvent;
use crate::llm::store::{self, ClientInput, ClientPolicyInput, ClientWithToken, GatewayClient};
use crate::logging::{LogLevel, log_line};
use crate::settings::SettingsSection;

/// Refreshes what depends on the client list after a change: the gateway's token cache, the LAN
/// rule (when no active client is left LAN serving is turned off, restarted on loopback and
/// stored as off), the pushed status and the `ClientsChanged` event. Blocks while a LAN gateway
/// drains.
pub(super) fn clients_changed(state: &AppState) {
    if state.llm.gateway.on_clients_changed(&state.llm) {
        let off = serde_json::json!({"gateway": {"serve_lan": false}});
        if let Err(error) = settings_update_impl(state, SettingsSection::Ai, &off) {
            log_line(
                LogLevel::Warn,
                "llm.clients",
                &format!("could not store serve_lan = false: {}", error.message()),
            );
        }
    }
    state.llm.emit(LlmEvent::GatewayStatus {
        status: state.llm.gateway.status(&state.llm),
    });
    state.llm.emit(LlmEvent::ClientsChanged);
}

/// Every client, oldest first, active and revoked (the token is never part of a record).
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_gateway_client_list_impl(state: &AppState) -> Result<Vec<GatewayClient>, AppError> {
    with_connection(state, store::list_clients)
}

/// Creates a client and returns its token **once**: the caller shows it, the store
/// keeps only the hash. The first-request watcher starts for the new client.
///
/// # Errors
/// [`AppError::Validation`] for a bad name or allow-list, a duplicate name or the 65th record;
/// [`AppError::Internal`] without OS randomness (nothing is created).
pub fn llm_gateway_client_create_impl(
    state: &AppState,
    input: ClientInput,
) -> Result<ClientWithToken, AppError> {
    let created = with_connection(state, |connection| {
        store::create_client(connection, &input, current_time_ms())
    })?;
    state
        .llm
        .services
        .client_watch
        .first_requests
        .arm(&created.client.id);
    clients_changed(state);
    Ok(created)
}

/// Saves a client's name and allow-list; takes effect on the next request.
///
/// # Errors
/// [`AppError::Validation`] as for create, [`AppError::NotFound`] for an unknown id.
pub fn llm_gateway_client_update_impl(
    state: &AppState,
    client_id: &str,
    input: ClientInput,
) -> Result<GatewayClient, AppError> {
    let client = with_connection(state, |connection| {
        store::update_client(connection, client_id, &input)
    })?;
    clients_changed(state);
    Ok(client)
}

/// Saves a client's limits (requests per minute, requests in flight, spend and token budgets,
/// expiry) and, when given, whether bare unknown model names go to its default model; takes
/// effect on the next request.
///
/// # Errors
/// [`AppError::Validation`] for limits out of range, [`AppError::NotFound`] for an unknown id.
pub fn llm_gateway_client_set_policy_impl(
    state: &AppState,
    client_id: &str,
    input: ClientPolicyInput,
) -> Result<GatewayClient, AppError> {
    let client = with_connection(state, |connection| {
        store::set_client_policy(connection, client_id, &input)
    })?;
    clients_changed(state);
    Ok(client)
}

/// "Rotate and copy token": replaces the token of an active client and returns the new
/// one **once**. The old token stops working at once (no grace period) and the files that hold
/// it keep the dead token; "Rotate and update N files" is
/// [`super::llm_client_rotate_and_update_impl`].
///
/// # Errors
/// [`AppError::NotFound`] for an unknown id, [`AppError::Validation`] for a revoked client.
pub fn llm_gateway_client_rotate_impl(
    state: &AppState,
    client_id: &str,
) -> Result<ClientWithToken, AppError> {
    let rotated = with_connection(state, |connection| {
        store::rotate_client_token(connection, client_id)
    })?;
    state
        .llm
        .services
        .client_watch
        .first_requests
        .arm(client_id);
    clients_changed(state);
    Ok(rotated)
}

/// Revokes a client: its token gets 401 from the next request, the record stays for the log's
/// attribution. Revoking twice keeps the first time. The files that hold its token are not
/// changed; the dialog lists them (`llm_client_applied_list`) and "Restore original config" puts
/// them back per tool.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown id.
pub fn llm_gateway_client_revoke_impl(
    state: &AppState,
    client_id: &str,
) -> Result<GatewayClient, AppError> {
    let client = with_connection(state, |connection| {
        store::revoke_client(connection, client_id, current_time_ms())
    })?;
    clients_changed(state);
    Ok(client)
}

/// Deletes a revoked client for good; its log rows keep the name. Its ledger rows go too (no
/// rotation can follow its files any more), and the backups only they named are pruned; the
/// tools' originals stay for "Restore original config".
///
/// # Errors
/// [`AppError::NotFound`] for an unknown id, [`AppError::Validation`] for an active client.
pub fn llm_gateway_client_delete_impl(state: &AppState, client_id: &str) -> Result<(), AppError> {
    let apps = with_connection(state, |connection| {
        let apps: Vec<crate::client_config::ClientApp> =
            store::latest_applied(connection, client_id)?
                .into_iter()
                .map(|entry| entry.app)
                .collect();
        store::delete_client(connection, client_id)?;
        store::delete_applied_for_client(connection, client_id)?;
        Ok(apps)
    })?;
    // The backups live in the data folder; an in-memory state (tests) has none to prune.
    if state.data_dirs().is_some()
        && let Ok(home) = crate::client_config::home_dir()
    {
        let mut apps = apps;
        apps.dedup();
        super::client_rotation::tidy_kept(state, &home, &apps);
    }
    state
        .llm
        .services
        .client_watch
        .first_requests
        .disarm(client_id);
    clients_changed(state);
    Ok(())
}
