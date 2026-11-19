//! Adapter selection and the balance entry point: the custom query, then the adapter of the
//! preset or of a known vendor host, then the one of the detected family. Every request of a check
//! goes through one [`Session`], so the budget and the safety rules hold for every adapter alike.

use crate::llm::safe_http::SafeHttp;
use crate::llm::wire::GatewayFamily;
use pingdeck_base::cancel::CancelToken;

use super::error::{BalanceError, BalanceErrorKind};
use super::fetch::Session;
use super::rules::host_matches;
use super::target::{host_of, normalise_origin};
use super::{
    AdapterId, BalanceOutcome, BalanceReport, BalanceTarget, custom, declarative, deepseek,
    litellm, newapi, oneapi, openrouter, sub2api, voapi,
};

/// Presets whose adapter is code rather than a declarative entry.
const CODE_PRESETS: [(&str, AdapterId); 2] = [
    ("deepseek", AdapterId::DeepSeek),
    ("openrouter", AdapterId::OpenRouter),
];
/// Vendor hosts (and their subdomains) whose adapter is code.
const CODE_HOSTS: [(&str, AdapterId); 2] = [
    ("deepseek.com", AdapterId::DeepSeek),
    ("openrouter.ai", AdapterId::OpenRouter),
];

const fn adapter_for_family(family: GatewayFamily) -> Option<AdapterId> {
    match family {
        GatewayFamily::NewApi | GatewayFamily::Veloera | GatewayFamily::RixApi => {
            Some(AdapterId::NewApi)
        }
        GatewayFamily::DoneHub => Some(AdapterId::DoneHub),
        GatewayFamily::OneApi => Some(AdapterId::OneApi),
        GatewayFamily::Sub2Api => Some(AdapterId::Sub2Api),
        GatewayFamily::LiteLlm => Some(AdapterId::LiteLlm),
        GatewayFamily::VoApi => Some(AdapterId::VoApiV2),
        GatewayFamily::OpenRouter => Some(AdapterId::OpenRouter),
        _ => None,
    }
}

/// The adapter a balance check of `target` would use; `None` is "No balance API" and means no
/// request will be sent. Order: the saved custom query, the adapter named for the preset or the
/// vendor host, the adapter of the detected family.
#[must_use]
pub fn select_adapter(target: &BalanceTarget<'_>) -> Option<AdapterId> {
    if target.custom.is_some() {
        return Some(AdapterId::Custom);
    }
    let host = host_of(target.origin);
    let code = CODE_PRESETS
        .iter()
        .find(|(preset, _)| target.preset_id == Some(*preset))
        .or_else(|| {
            CODE_HOSTS.iter().find(|(suffix, _)| {
                host.as_deref()
                    .is_some_and(|host| host_matches(host, suffix))
            })
        })
        .map(|(_, adapter)| *adapter);
    code.or_else(|| declarative::select(target.preset_id, host.as_deref(), target.family))
        .or_else(|| adapter_for_family(target.family))
}

async fn run(
    session: &mut Session<'_>,
    target: &BalanceTarget<'_>,
    adapter: AdapterId,
    now_ms: i64,
) -> Result<BalanceReport, BalanceError> {
    match adapter {
        AdapterId::Custom => match target.custom {
            Some(query) => custom::query(session, target, query, now_ms).await,
            None => Err(BalanceError::new(
                BalanceErrorKind::NotFound,
                "The custom query is gone",
            )),
        },
        AdapterId::DeepSeek => deepseek::query(session, target, now_ms).await,
        AdapterId::OpenRouter => openrouter::query(session, target, now_ms).await,
        AdapterId::NewApi | AdapterId::DoneHub => {
            newapi::query(session, target, adapter, now_ms).await
        }
        AdapterId::OneApi => oneapi::query(session, target, now_ms).await,
        AdapterId::Sub2Api => sub2api::query(session, target, now_ms).await,
        AdapterId::LiteLlm => litellm::query(session, target, now_ms).await,
        AdapterId::VoApiV2 => voapi::query(session, target, now_ms).await,
        AdapterId::Moonshot | AdapterId::SiliconFlow | AdapterId::StepFun | AdapterId::Zhipu => {
            declarative::query(session, target, adapter, now_ms).await
        }
    }
}

/// Checks the balance of `target` with the selected adapter.
///
/// Sends only `GET`s through `http`, at most `target.request_budget` of them, to the provider's
/// own origin (a custom query may name another host only when the user confirmed it). No URL is
/// ever taken from a response body.
pub async fn query_balance(
    http: &dyn SafeHttp,
    target: &BalanceTarget<'_>,
    cancel: &CancelToken,
    now_ms: i64,
) -> BalanceOutcome {
    let Some(adapter) = select_adapter(target) else {
        return BalanceOutcome::NoAdapter;
    };
    let failed = |error| BalanceOutcome::Failed {
        error,
        requests_used: 0,
    };
    let origin = match normalise_origin(target.origin) {
        Ok(origin) => origin,
        Err(error) => return failed(error),
    };
    if !target.allowed_origins().contains(&origin) {
        return failed(BalanceError::new(
            BalanceErrorKind::Blocked,
            "The provider's origin is not one of its stored endpoints",
        ));
    }
    if target.request_budget == 0 {
        return failed(BalanceError::new(
            BalanceErrorKind::Blocked,
            "The request budget does not allow a balance request",
        ));
    }
    let mut session = Session::new(http, cancel, origin, target);
    let result = run(&mut session, target, adapter, now_ms)
        .await
        .map(|report| BalanceReport {
            evidence: adapter.evidence(),
            ..report
        });
    let requests_used = session.used();
    match result {
        Ok(report) => BalanceOutcome::Report {
            report,
            requests_used,
        },
        Err(error) => BalanceOutcome::Failed {
            error,
            requests_used,
        },
    }
}
