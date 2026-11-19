//! The notice of a gateway start: what an upgrade made stricter for this configuration.
//!
//! Single responsibility: name, on the status that announces a start, the stored settings that
//! still say "no token" next to extra allowed hosts (the gateway requires tokens anyway)
//! and the restricted clients whose gateway rules lead to fallbacks they may not use (those
//! fallbacks are skipped). Nothing here changes routing or access.

use serde_json::Value;

use super::GatewayNoticeCode;
use crate::llm::router::RoutingSnapshot;
use crate::llm::runtime::LlmRuntime;
use crate::llm::store::ClientHashEntry;

/// The forced-tokens part of the notice.
pub(super) const FORCED_TOKEN_NOTICE: &str = "Tokens are required: extra allowed hosts are set, \
     so a call without a client token gets 401. Saving the gateway settings keeps tokens on.";

/// Clients named in the restricted-clients part before it says how many more there are.
const MAX_CLIENTS_NAMED: usize = 5;

/// The parts of a start's notice, each with its code. Blocking SQLite work.
pub(super) fn notices(llm: &LlmRuntime) -> Vec<(GatewayNoticeCode, String)> {
    let forced = forced_token(llm).map(|text| (GatewayNoticeCode::TokensForced, text.to_owned()));
    let narrowed = narrowed(llm).map(|text| (GatewayNoticeCode::FallbacksNarrowed, text));
    [forced, narrowed].into_iter().flatten().collect()
}

/// The status `notice` of a run: the parts of [`notices`] in one line, `None` when none apply.
pub(super) fn joined(parts: &[(GatewayNoticeCode, String)]) -> Option<String> {
    (!parts.is_empty()).then(|| {
        parts
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// The stored `ai` row turns tokens off and lists extra hosts. The loaded settings
/// already read it as tokens on (`AiSettings::normalize_stored`), so the row is read as stored.
fn forced_token(llm: &LlmRuntime) -> Option<&'static str> {
    let connection = llm.db().open().ok()?;
    let row = pingdeck_store::storage::raw_app_setting_json(&connection, "ai")
        .ok()
        .flatten()?;
    let gateway = serde_json::from_str::<Value>(&row)
        .ok()?
        .get("gateway")?
        .clone();
    let open = gateway.get("require_token") == Some(&Value::Bool(false));
    let hosts = gateway
        .get("extra_allowed_hosts")
        .and_then(Value::as_array)
        .is_some_and(|hosts| !hosts.is_empty());
    (open && hosts).then_some(FORCED_TOKEN_NOTICE)
}

fn narrowed(llm: &LlmRuntime) -> Option<String> {
    let snapshot = llm.snapshot_blocking().ok()?;
    let clients = llm.client_hashes_blocking().ok()?;
    narrowed_notice(&snapshot, &clients)
}

/// Per active client with an allow-list, the fallbacks outside it of the rules whose
/// primary it may use.
pub(super) fn narrowed_notice(
    snapshot: &RoutingSnapshot,
    clients: &[ClientHashEntry],
) -> Option<String> {
    let mut named: Vec<String> = Vec::new();
    for client in clients
        .iter()
        .map(|entry| &entry.client)
        .filter(|client| client.is_active() && !client.allow_all)
    {
        let allows = |name: &str| client.allows(name);
        let mut left_out: Vec<String> = snapshot
            .gateway_fallbacks
            .iter()
            .filter(|(primary, _)| snapshot.admits(primary, &allows))
            .flat_map(|(_, chain)| chain)
            .filter(|fallback| !snapshot.admits(fallback, &allows))
            .map(|fallback| snapshot.display_ref(&fallback.provider_id, &fallback.model))
            .collect();
        if left_out.is_empty() {
            continue;
        }
        left_out.sort();
        left_out.dedup();
        named.push(format!("{} ({})", client.name, left_out.join(", ")));
    }
    if named.is_empty() {
        return None;
    }
    let more = named.len().saturating_sub(MAX_CLIENTS_NAMED);
    named.truncate(MAX_CLIENTS_NAMED);
    let tail = if more > 0 {
        format!(" and {more} more")
    } else {
        String::new()
    };
    Some(format!(
        "Restricted clients no longer fall back to models outside their allowed list: {}{tail}. \
         Add a fallback to a client's allowed models to keep it.",
        named.join("; ")
    ))
}
