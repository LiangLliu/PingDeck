//! The balance step of a scan.
//!
//! The balance adapter chosen by the known-host table or the detected family is asked at most
//! once, through the scan's executor: its requests share the budget (tier 9 for the first
//! request, 10 for the rest), the stop rules and the evidence log, and cannot leave the typed
//! origin.

use futures_util::future::BoxFuture;

use super::{
    exec::{
        BODY_CAP, Call, Exec, Purpose, Sent, SkipReason, Spend, TIER_BALANCE, TIER_BALANCE_EXTRA,
    },
    identify::Identity,
    scan::Scan,
    types::BalanceFinding,
};
use crate::llm::{
    balance::{BalanceOutcome, BalanceReport, BalanceTarget, query_balance, select_adapter},
    safe_http::{SafeHttp, SafeRequest, SafeResponse, SafeTransportError},
    wire::GatewayFamily,
};
use pingdeck_base::cancel::CancelToken;

/// What the balance step found.
#[derive(Debug, Clone)]
pub(super) struct BalanceStep {
    /// The reading.
    pub report: Option<BalanceReport>,
    /// The finding for the result card.
    pub finding: BalanceFinding,
    /// The adapter that was (or would be) used.
    pub adapter: Option<String>,
}

/// The path and query of `url` when it is on `origin` (`origin` itself, or `origin` followed by
/// `/` or `?`); `None` for any other host, including one that merely starts with the origin's
/// text (`https://x.test.evil`, `https://x.test@evil`).
fn origin_path<'u>(origin: &str, url: &'u str) -> Option<&'u str> {
    url.strip_prefix(origin)
        .filter(|rest| rest.is_empty() || rest.starts_with(['/', '?']))
}

/// The spend of the next balance request: the reserved slot for the first one, tier 10 for the
/// rest.
fn balance_spend(ex: &Exec<'_>) -> Spend {
    let hold = ex.take_balance_hold();
    Spend::held(
        if hold.is_some() {
            TIER_BALANCE
        } else {
            TIER_BALANCE_EXTRA
        },
        hold,
    )
}

/// A [`SafeHttp`] whose requests are the scan's own: budgeted, logged, confined to its origin.
struct ExecHttp<'a, 'b> {
    ex: &'a Exec<'b>,
}

impl SafeHttp for ExecHttp<'_, '_> {
    fn send<'x>(
        &'x self,
        request: SafeRequest,
        _cancel: &'x CancelToken,
    ) -> BoxFuture<'x, Result<SafeResponse, SafeTransportError>> {
        Box::pin(async move {
            let Some(path) = origin_path(&self.ex.origin, &request.url) else {
                return Err(SafeTransportError::Blocked {
                    reason: "origin".to_owned(),
                });
            };
            let path = if path.is_empty() { "/" } else { path };
            let call = Call {
                headers: request.headers,
                body: request.body,
                max_body: request.max_body.min(BODY_CAP),
                ..Call::new(
                    Purpose::Balance,
                    "Balance",
                    request.method,
                    path,
                    request.timeout.min(self.ex.limits.models),
                    balance_spend(self.ex),
                )
            };
            match self.ex.call(call).await {
                Sent::Done { result } => result,
                Sent::Skipped(SkipReason::Cancelled) => Err(SafeTransportError::Cancelled),
                Sent::Skipped(reason) => Err(SafeTransportError::Blocked {
                    reason: reason.as_str().to_owned(),
                }),
            }
        })
    }
}

/// Families whose console takes a numeric user-id header; it is handed on only once the family
/// was identified.
const fn takes_user_id(family: GatewayFamily) -> bool {
    matches!(
        family,
        GatewayFamily::NewApi
            | GatewayFamily::DoneHub
            | GatewayFamily::Veloera
            | GatewayFamily::RixApi
    )
}

impl Scan<'_> {
    /// What the balance adapters are told about the scanned provider.
    fn balance_target<'s>(&'s self, identity: &Identity) -> BalanceTarget<'s> {
        BalanceTarget {
            origin: &self.ex.origin,
            endpoint_origins: std::slice::from_ref(&self.ex.origin),
            family: identity.family,
            preset_id: self
                .plan
                .known
                .as_ref()
                .map(|known| known.preset_id.as_str()),
            api_key: self.key.unwrap_or_default(),
            management_token: self.input.management_token.as_deref(),
            api_user_id: self
                .input
                .api_user_id
                .as_deref()
                .filter(|_| identity.accepted && takes_user_id(identity.family)),
            quota_per_unit: identity.quota_per_unit,
            display_in_currency: identity.display_in_currency,
            allow_private: self.ex.allow_private(),
            custom: None,
            request_budget: self.ex.remaining(),
        }
    }

    /// Runs the balance step for `identity`.
    pub(super) async fn balance(&self, identity: &Identity, now_ms: i64) -> BalanceStep {
        let target = self.balance_target(identity);
        let adapter = select_adapter(&target).map(|adapter| adapter.as_str().to_owned());
        let step = |finding| BalanceStep {
            report: None,
            finding,
            adapter: adapter.clone(),
        };
        let not_attempted = |reason: &str| {
            step(BalanceFinding::NotAttempted {
                reason: reason.to_owned(),
            })
        };
        if adapter.is_none() {
            self.ex.release_balance_hold();
            return step(BalanceFinding::NoEndpoint);
        }
        if self.key.is_none() {
            self.ex.release_balance_hold();
            return not_attempted("no_key");
        }
        if self.ex.stop_reason().is_some() {
            self.ex.release_balance_hold();
            self.ex.skip("balance", "stopped");
            return not_attempted("stopped");
        }
        if self.ex.remaining() == 0 {
            self.ex.skip("balance", "budget");
            return not_attempted("budget");
        }
        let http = ExecHttp { ex: self.ex };
        match query_balance(&http, &target, self.ex.cancel(), now_ms).await {
            BalanceOutcome::Report { report, .. } => BalanceStep {
                report: Some(report),
                finding: BalanceFinding::Found,
                adapter,
            },
            BalanceOutcome::NoAdapter => step(BalanceFinding::NoEndpoint),
            BalanceOutcome::Failed { error, .. } => step(BalanceFinding::Failed {
                error: error.kind.as_str().to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::llm::{
        safe_http::{SafeMethod, scripted::ScriptedHttp},
        sniff::exec::{ExecConfig, Limits},
    };

    #[test]
    fn only_paths_on_the_typed_origin_pass() {
        let origin = "https://x.test";
        assert_eq!(origin_path(origin, "https://x.test"), Some(""));
        assert_eq!(
            origin_path(origin, "https://x.test/api/a?b"),
            Some("/api/a?b")
        );
        assert_eq!(origin_path(origin, "https://x.test?b"), Some("?b"));
        for foreign in [
            "https://x.test.evil/api",
            "https://x.test@evil/api",
            "https://x.testing/api",
            "http://x.test/api",
        ] {
            assert_eq!(origin_path(origin, foreign), None, "{foreign}");
        }
    }

    #[tokio::test]
    async fn a_balance_request_never_leaves_the_typed_origin() {
        let http = ScriptedHttp::new();
        let cancel = CancelToken::new();
        let ex = Exec::new(ExecConfig {
            origin: "https://x.test".to_owned(),
            typed_mount: String::new(),
            allow_private: false,
            budget: 8,
            limits: Limits::PRODUCTION,
            base_headers: Vec::new(),
            secrets: Vec::new(),
            proxy_dns: false,
            http: &http,
            cancel: &cancel,
            sink: &|_| true,
        });
        let adapter = ExecHttp { ex: &ex };
        for url in ["https://x.test.evil/api", "https://x.test@evil/api"] {
            let request = SafeRequest {
                method: SafeMethod::Get,
                url: url.to_owned(),
                headers: Vec::new(),
                body: None,
                timeout: Duration::from_secs(1),
                connect_timeout: Duration::from_secs(1),
                max_body: 1024,
                allow_private: false,
            };
            assert_eq!(
                adapter.send(request, &cancel).await,
                Err(SafeTransportError::Blocked {
                    reason: "origin".to_owned()
                }),
                "{url}"
            );
        }
        assert_eq!(http.count(), 0);
    }
}
