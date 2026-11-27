//! The scan itself: [`sniff_provider`] runs the phases under the whole-scan cap and returns a
//! report.
//!
//! It validates the input, builds the plan and the executor, and runs the phases in order: the
//! local fingerprints, identify, the model list and the route probes, the LiteLLM read, the
//! optional real requests, then the balance step. It enforces the whole-scan limit and
//! cancellation (both end in a partial report once something answered) and hands the collected
//! state to `report::assemble`. Nothing is saved.

use std::{borrow::Cow, sync::Mutex, time::Instant};

use futures_util::future::{join, join_all};

use super::{
    exec::{
        Collected, Exec, ExecConfig, Limits, RowPlan, TIER_BALANCE, TIER_IDENTIFY, TIER_KEY_CHECK,
        TIER_MODELS,
    },
    identify::{Facts, domain_family},
    key_guard,
    models::{ModelKind, finalize},
    plan::{self, ModelAttempt, route_template},
    report::{ScanEnd, assemble, primary_among},
    route_probe::{VerifyTargets, route_label},
    scan::{Scan, State, Trials},
    types::{SniffContext, SniffError, SniffEvent, SniffInput, SniffPhase, SniffReport},
    url::{NormalizedUrl, UrlHostClass, normalize_base_url},
};
use crate::llm::{
    presets,
    safe_http::{SafeHttp, SafeMethod, SafeTransportError},
    wire::Protocol,
};
use spanbay_base::{cancel::CancelToken, net_guard::classify_host_literal};
use spanbay_model::events::EventSink;

/// Header names the sniffer never lets the user override.
const FORBIDDEN_HEADERS: [&str; 5] = [
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "proxy-authorization",
];

/// Most extra headers a scan accepts.
const MAX_EXTRA_HEADERS: usize = 8;

/// The symbols of an HTTP header-name token (RFC 9110 `tchar`); letters and digits are the rest.
const TCHAR_SYMBOLS: &str = "!#$%&'*+-.^_`|~";

/// The default port of CLIProxyAPI, a hint when it answers on loopback.
const CPA_DEFAULT_PORT: u16 = 8317;

/// Rejects extra headers before any request: more than [`MAX_EXTRA_HEADERS`] is
/// `too_many_headers`; an empty or non-token name, one of [`FORBIDDEN_HEADERS`], or a control
/// character in a value or in the user agent is `bad_header`.
fn validate_input(input: &SniffInput) -> Result<(), SniffError> {
    let invalid = |reason: &str| SniffError::InvalidInput {
        reason: reason.to_owned(),
    };
    if input.extra_headers.len() > MAX_EXTRA_HEADERS {
        return Err(invalid("too_many_headers"));
    }
    let token = |c: char| c.is_ascii_alphanumeric() || TCHAR_SYMBOLS.contains(c);
    for (name, value) in &input.extra_headers {
        if name.is_empty()
            || !name.chars().all(token)
            || FORBIDDEN_HEADERS
                .iter()
                .any(|forbidden| name.eq_ignore_ascii_case(forbidden))
            || value.chars().any(char::is_control)
        {
            return Err(invalid("bad_header"));
        }
    }
    if input
        .user_agent
        .as_deref()
        .is_some_and(|agent| agent.chars().any(char::is_control))
    {
        return Err(invalid("bad_header"));
    }
    Ok(())
}

/// Scans `input.base_url`: identity, protocols with their mounts, models, key verdict and
/// balance. It sends at most 12 budgeted requests with a key, 8 without one and 20 in an
/// extended scan, plus the opt-in real requests, and follows no redirect. Progress goes to
/// `sink`; the report is the caller's to cache.
///
/// # Errors
/// [`SniffError::InvalidInput`] before any request, [`SniffError::BlockedAddress`] for a
/// link-local target, [`SniffError::PrivateResolution`] for a public name that resolves
/// privately, [`SniffError::Transport`] when no request got any answer and
/// [`SniffError::Cancelled`] when `cancel` fired.
pub async fn sniff_provider(
    http: &dyn SafeHttp,
    input: SniffInput,
    ctx: &SniffContext<'_>,
    sink: &dyn EventSink<SniffEvent>,
    cancel: &CancelToken,
) -> Result<SniffReport, SniffError> {
    scan_with_limits(http, input, ctx, sink, cancel, Limits::PRODUCTION).await
}

/// Fills `url.known_host` from the typed preset's region when the host is not itself a known
/// host but belongs to that region.
fn resolve_known_host(url: &mut NormalizedUrl, input: &SniffInput) {
    if url.known_host.is_none()
        && let Some(preset) = input.preset_id.as_deref()
    {
        url.known_host = presets::preset_region_for_host(
            preset,
            input.region_id.as_deref(),
            &url.host,
            url.port,
        );
    }
}

/// The headers of every request: `accept`, the user agent, then the extra headers whose
/// (lower-cased) names are not taken yet.
fn base_headers(input: &SniffInput, ctx: &SniffContext<'_>) -> Vec<(String, String)> {
    let user_agent = input
        .user_agent
        .as_deref()
        .map(str::trim)
        .filter(|agent| !agent.is_empty())
        .map_or_else(|| format!("SpanBay/{}", ctx.app_version), str::to_owned);
    let mut headers = vec![
        ("accept".to_owned(), "application/json".to_owned()),
        ("user-agent".to_owned(), user_agent),
    ];
    for (name, value) in &input.extra_headers {
        let name = name.to_ascii_lowercase();
        if !headers.iter().any(|(own, _)| *own == name) {
            headers.push((name, value.clone()));
        }
    }
    headers
}

/// The error of a scan in which no request got any answer, from the first transport failure.
fn no_answer_error(
    collected: &Collected,
    url: &NormalizedUrl,
    input: &SniffInput,
) -> Option<SniffError> {
    if collected.any_response {
        return None;
    }
    let host = || url.host.clone();
    match collected.first_error.as_ref()? {
        SafeTransportError::Blocked { reason }
            if matches!(reason.as_str(), "private" | "refused_address")
                && !input.confirm_private =>
        {
            Some(SniffError::PrivateResolution { host: host() })
        }
        SafeTransportError::Blocked { .. } => Some(SniffError::BlockedAddress { host: host() }),
        error => Some(SniffError::Transport {
            error_kind: error.kind().to_owned(),
            host: host(),
        }),
    }
}

/// [`sniff_provider`] with explicit limits (tests scale them down).
pub(super) async fn scan_with_limits(
    http: &dyn SafeHttp,
    input: SniffInput,
    ctx: &SniffContext<'_>,
    sink: &dyn EventSink<SniffEvent>,
    cancel: &CancelToken,
    limits: Limits,
) -> Result<SniffReport, SniffError> {
    let started = Instant::now();
    let mut url = normalize_base_url(&input.base_url)?;
    validate_input(&input)?;
    if url.host_class == UrlHostClass::LinkLocal {
        return Err(SniffError::BlockedAddress {
            host: url.host.clone(),
        });
    }
    if cancel.is_cancelled() {
        return Err(SniffError::Cancelled);
    }
    resolve_known_host(&mut url, &input);
    let plan = plan::build(&url, &input)?;
    let key = input.key();

    let exec = Exec::new(ExecConfig {
        origin: url.origin.clone(),
        typed_mount: url.base_mount.clone(),
        allow_private: url.host_class.is_local() || input.confirm_private,
        budget: plan.budget,
        limits,
        base_headers: base_headers(&input, ctx),
        secrets: input.secrets(),
        proxy_dns: ctx.uses_proxy && classify_host_literal(&url.host).is_none(),
        http,
        cancel,
        sink,
    });
    let facts = Facts {
        domain: domain_family(url.known_host.as_ref(), &url.host),
        cpa_default_port: url.host_class == UrlHostClass::Loopback
            && url.port == Some(CPA_DEFAULT_PORT),
        ..Facts::default()
    };
    let scan = Scan {
        ex: &exec,
        input: &input,
        url: &url,
        plan: &plan,
        key,
        trials: Trials::new(&url.origin, ctx),
        state: Mutex::new(State {
            facts,
            ..State::default()
        }),
    };

    // A look-alike host or a foreign key shape stops the scan before any request.
    let key_concern = key
        .filter(|_| !input.confirm_key_send)
        .and_then(|key| key_guard::concern(&url, key));
    let ended = if key_concern.is_some() {
        exec.plan(&[]);
        Ended::Done
    } else {
        tokio::select! {
            () = run_phases(&scan, ctx) => Ended::Done,
            () = tokio::time::sleep(limits.total) => Ended::TimedOut,
            () = cancel.cancelled() => Ended::Cancelled,
        }
    };
    let cancelled = ended == Ended::Cancelled || cancel.is_cancelled();
    let timed_out = ended == Ended::TimedOut && !cancelled;
    if ended != Ended::Done {
        // The dropped phases leave rows queued or in flight; each still gets its end.
        exec.finish_open_rows(if cancelled { "cancelled" } else { "timeout" });
    }
    let collected = exec.collect();
    // Stop and the time limit end a scan the same way: what was found so far becomes a partial
    // report that can still be added. A stop before any answer has nothing to report.
    if cancelled && !collected.any_response {
        return Err(SniffError::Cancelled);
    }
    if let Some(error) = no_answer_error(&collected, &url, &input) {
        return Err(error);
    }
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let ends = ScanEnd {
        timed_out,
        cancelled,
    };
    let report = assemble(&scan, ctx, collected, elapsed_ms, ends, key_concern);
    exec.announce_done(&report.report_id);
    Ok(report)
}

/// How the phases of a scan ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// Every phase finished.
    Done,
    /// The scan's time limit ended it.
    TimedOut,
    /// The cancel token stopped it.
    Cancelled,
}

/// The protocol whose first route probe doubles as the key check: the typed tail's protocol
/// when probed, else the first probed of Chat, Messages, Responses, Gemini, Embeddings,
/// Systemone, Decisions.
fn primary_protocol(scan: &Scan<'_>) -> Option<Protocol> {
    primary_among(scan.url.tail_hint, |protocol| {
        scan.plan
            .routes
            .iter()
            .any(|route| route.protocol == protocol)
    })
}

/// Budget tier of a route probe: the primary protocol's is the key check (tier 2); otherwise
/// Chat and Messages 4, Gemini 5, Responses 6, and Embeddings and the decision routes 7.
fn tier_of(protocol: Protocol, primary: Option<Protocol>) -> u8 {
    if Some(protocol) == primary {
        return TIER_KEY_CHECK;
    }
    match protocol {
        Protocol::OpenaiChat | Protocol::AnthropicMessages => 4,
        Protocol::Gemini => 5,
        Protocol::OpenaiResponses => 6,
        Protocol::Embeddings | Protocol::DecisionSystemone | Protocol::DecisionOpenai => 7,
    }
}

/// The slots reserved before anything is sent.
struct Holds {
    models: Option<u32>,
    identify: Option<u32>,
    /// Per planned route, in plan order.
    routes: Vec<Option<u32>>,
    balance: Option<u32>,
}

/// Reserves the scan's slots in priority order, so a reservation never takes a more important
/// slot: the model list, then the routes in ascending tier with identify placed before the
/// first route above its tier, then the balance step (keyed scans only).
fn reserve_slots(scan: &Scan<'_>, has_attempts: bool, primary: Option<Protocol>) -> Holds {
    let ex = scan.ex;
    let plan = scan.plan;
    let models = if has_attempts {
        ex.hold(TIER_MODELS)
    } else {
        None
    };
    let mut by_tier: Vec<(usize, u8)> = plan
        .routes
        .iter()
        .enumerate()
        .map(|(index, spec)| (index, tier_of(spec.protocol, primary)))
        .collect();
    by_tier.sort_by_key(|(_, tier)| *tier);
    let mut routes: Vec<Option<u32>> = vec![None; plan.routes.len()];
    let mut identify = None;
    let mut identify_held = !plan.identify;
    for (index, tier) in by_tier {
        if !identify_held && tier > TIER_IDENTIFY {
            identify = ex.hold(TIER_IDENTIFY);
            identify_held = true;
        }
        routes[index] = ex.hold(tier);
    }
    if !identify_held {
        identify = ex.hold(TIER_IDENTIFY);
    }
    let balance = if scan.key.is_some() {
        ex.hold(TIER_BALANCE)
    } else {
        None
    };
    Holds {
        models,
        identify,
        routes,
        balance,
    }
}

/// The ids of the rows announced up front.
struct Rows {
    identify: Option<u32>,
    models: Option<u32>,
    /// Per planned route, in plan order.
    routes: Vec<u32>,
}

/// Announces the rows the scan intends to send, in one `Planned` event: identify, the first
/// model-list attempt, then each route's probe path template.
fn plan_rows(scan: &Scan<'_>, attempts: &[ModelAttempt]) -> Rows {
    let plan = scan.plan;
    let get = SafeMethod::Get.as_str();
    let mut rows: Vec<RowPlan<'_>> = Vec::with_capacity(plan.routes.len() + 2);
    if plan.identify {
        rows.push(RowPlan {
            method: get,
            path: Cow::Borrowed("/api/status"),
            label: "Identify",
            counted: true,
        });
    }
    if let Some(first) = attempts.first() {
        rows.push(RowPlan {
            method: get,
            path: Cow::Borrowed(&first.path),
            label: "Models",
            counted: true,
        });
    }
    for spec in &plan.routes {
        let mount = spec.candidates.first().map_or("", String::as_str);
        let verbatim = plan.verbatim == Some(spec.protocol);
        rows.push(RowPlan {
            method: SafeMethod::Post.as_str(),
            path: Cow::Owned(route_template(spec.protocol, mount, verbatim)),
            label: route_label(spec.protocol),
            counted: true,
        });
    }
    let mut ids = scan.ex.plan(&rows).into_iter();
    let identify = plan.identify.then(|| ids.next()).flatten();
    let models = attempts.first().and_then(|_| ids.next());
    Rows {
        identify,
        models,
        routes: ids.collect(),
    }
}

/// The model list and the route probes. Routes of an unknown host reuse the mount the model
/// list answered at, so they follow it; a known or local preset has fixed mounts and both run
/// together.
async fn run_models_and_routes(
    scan: &Scan<'_>,
    attempts: &[ModelAttempt],
    rows: Rows,
    holds: &Holds,
    primary: Option<Protocol>,
) {
    let ex = scan.ex;
    ex.announce(SniffPhase::Models);
    let models = async {
        if attempts.is_empty() {
            ex.skip("models", "not_applicable");
        } else {
            scan.models(attempts, rows.models, holds.models).await;
        }
    };
    let routes = async {
        ex.announce(SniffPhase::Routes);
        join_all(
            scan.plan
                .routes
                .iter()
                .zip(rows.routes)
                .zip(&holds.routes)
                .map(|((spec, row), hold)| {
                    scan.probe_route(spec, row, tier_of(spec.protocol, primary), *hold)
                }),
        )
        .await;
    };
    if scan.plan.known.is_some() {
        join(models, routes).await;
    } else {
        models.await;
        routes.await;
    }
}

/// The opt-in real requests: one 1-token request per existing route, naming listed models.
async fn run_verify(scan: &Scan<'_>, ctx: &SniffContext<'_>) {
    scan.ex.announce(SniffPhase::Verify);
    let raw = scan.state().models.raw.clone();
    let listed = finalize(raw, ctx.catalog, None);
    let targets = VerifyTargets {
        chat: listed
            .iter()
            .filter(|model| model.kind == ModelKind::Chat)
            .map(|model| model.id.clone())
            .collect(),
        embedding: listed
            .iter()
            .find(|model| model.kind == ModelKind::Embedding)
            .map(|model| model.id.clone()),
    };
    let tokens = scan.verify(&targets).await;
    scan.state().tokens_spent = tokens;
}

/// The balance step. Every reservation but the balance slot is given back first.
async fn run_balance(scan: &Scan<'_>, ctx: &SniffContext<'_>, balance_hold: Option<u32>) {
    let ex = scan.ex;
    ex.announce(SniffPhase::Balance);
    ex.release_all_except(balance_hold);
    ex.set_balance_hold(balance_hold);
    let identity = scan.score_identity();
    let step = scan.balance(&identity, ctx.now_ms).await;
    scan.state().balance = Some(step);
}

/// The phases in order; every phase runs even when an earlier one failed, except after a stop.
/// Slots are reserved in priority order before anything is sent (see [`reserve_slots`]).
async fn run_phases(scan: &Scan<'_>, ctx: &SniffContext<'_>) {
    let ex = scan.ex;
    if scan.plan.local {
        ex.announce(SniffPhase::Local);
        scan.local().await;
    }

    let attempts = scan.model_attempts();
    let primary = primary_protocol(scan);
    let holds = reserve_slots(scan, !attempts.is_empty(), primary);
    let rows = plan_rows(scan, &attempts);

    // The key-free identify finishes before any request that carries the key.
    if let Some(row) = rows.identify {
        ex.announce(SniffPhase::Identify);
        scan.identify(row, holds.identify).await;
    }
    run_models_and_routes(scan, &attempts, rows, &holds, primary).await;
    scan.litellm_model_info().await;

    // The key verdict needs no request; the real requests are opt-in.
    ex.announce(SniffPhase::Key);
    if scan.input.allow_billable_probe && scan.key.is_some() && ex.stop_reason().is_none() {
        run_verify(scan, ctx).await;
    }

    run_balance(scan, ctx, holds.balance).await;
}
