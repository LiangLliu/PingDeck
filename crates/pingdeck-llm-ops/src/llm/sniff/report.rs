//! Assembling the [`SniffReport`] from what a scan collected: endpoints and the default primary
//! protocol, models with their prefill, the key verdict, identity, warnings, per-phase outcome
//! and evidence references. Everything taken from a response is sanitised (control characters,
//! length, secrets) here.

mod phase_outcome;
mod warnings;

use std::collections::HashSet;

use serde_json::Value;

use self::{
    phase_outcome::PhaseInputs,
    warnings::{add_preset_warnings, scan_warnings},
};
use super::{
    auth::AuthFamily,
    balance_hook::BalanceStep,
    diff::diff_against_provider,
    exec::{Collected, Exec},
    identify::{Identity, score},
    key_guard::KeyConcern,
    local::LocalHit,
    models::{RawModel, finalize, parse_page},
    plan::{Plan, route_template, sdk_base_url},
    route_probe::RouteResult,
    route_verdict::{KeyFacts, key_verdict, route_conclusion},
    scan::{Scan, State},
    share::models_curl,
    types::{
        BalanceFinding, EvidenceItem, EvidenceRefs, KeyCheck, KeyCheckStatus, KeyEvidence,
        SkippedItem, SniffContext, SniffReport, SniffedEndpoint, SniffedModel,
    },
    url::NormalizedUrl,
};
use crate::llm::{
    balance::BalanceReport,
    capabilities::{Capability, CapabilitySource},
    presets::{ProviderPreset, presets},
    wire::{EndpointUrlMode, Protocol},
};
use pingdeck_base::{crypto::hex_lower, secrets::redact_secrets};

/// The order of the default primary protocol: a host that speaks only a decision API marks its
/// decision endpoint the default; one that also speaks chat keeps chat first.
const PRIMARY_ORDER: [Protocol; 7] = [
    Protocol::OpenaiChat,
    Protocol::AnthropicMessages,
    Protocol::OpenaiResponses,
    Protocol::Gemini,
    Protocol::Embeddings,
    Protocol::DecisionSystemone,
    Protocol::DecisionOpenai,
];

/// A fresh random report id.
pub(super) fn new_report_id() -> String {
    hex_lower(&pingdeck_base::id::random_bytes::<16>())
}

/// The primary protocol among those `accept` admits: the pasted tail's protocol, else the first
/// of [`PRIMARY_ORDER`].
pub(super) fn primary_among(
    tail_hint: Option<Protocol>,
    accept: impl Fn(Protocol) -> bool,
) -> Option<Protocol> {
    tail_hint
        .filter(|hint| accept(*hint))
        .or_else(|| PRIMARY_ORDER.into_iter().find(|protocol| accept(*protocol)))
}

/// Fills missing context and declared capabilities of local-runtime models from the runtime's
/// own endpoints (LM Studio's model list, `/api/v1/models` else `/api/v0/models`; llama.cpp
/// `n_ctx`).
fn merge_local(raw: &mut [RawModel], lm_studio: Option<&Value>, n_ctx: Option<u64>) {
    let lm_rows = lm_studio.map(parse_page).unwrap_or_default().models;
    for entry in raw {
        if let Some(row) = lm_rows.iter().find(|row| row.model.id == entry.model.id) {
            entry.model.context_window = entry.model.context_window.or(row.model.context_window);
            for capability in Capability::ALL {
                let state = row.model.capabilities.get(capability);
                if state.source == CapabilitySource::Declared {
                    entry.model.capabilities.apply(
                        capability,
                        state.supported,
                        CapabilitySource::Declared,
                    );
                }
            }
            entry.declared_kind = entry.declared_kind.or(row.declared_kind);
        }
        entry.model.context_window = entry.model.context_window.or(n_ctx);
    }
}

impl Scan<'_> {
    /// The scored identity of the host so far. A local host that lists `owned_by: "vllm"` is
    /// vLLM even when `/health` and `/version` did not say. Writes the local hit back to
    /// `facts.local`; calling it again gives the same result while the model list is still in
    /// place (`assemble` takes it only after scoring).
    pub(super) fn score_identity(&self) -> Identity {
        let mut state = self.state();
        let hit = state.local.classify(self.url.port);
        let listed_vllm = state.models.raw.iter().any(|raw| {
            raw.model
                .owned_by
                .as_deref()
                .is_some_and(|owner| owner.eq_ignore_ascii_case("vllm"))
        });
        state.facts.local = match hit {
            None if listed_vllm && self.url.host_class.is_local() => {
                Some(LocalHit::listed_vllm(self.url.port))
            }
            other => other,
        };
        score(&state.facts)
    }
}

fn evidence_ids(evidence: &[EvidenceItem], keep: impl Fn(&EvidenceItem) -> bool) -> Vec<u32> {
    evidence
        .iter()
        .filter(|item| keep(item))
        .map(|item| item.id)
        .collect()
}

/// `own`, or the first evidence row when `own` is empty: every conclusion cites a row.
fn own_or_first(own: Vec<u32>, evidence: &[EvidenceItem]) -> Vec<u32> {
    if own.is_empty() {
        evidence
            .first()
            .map(|item| vec![item.id])
            .unwrap_or_default()
    } else {
        own
    }
}

fn evidence_refs(evidence: &[EvidenceItem]) -> EvidenceRefs {
    let identity = evidence_ids(evidence, |item| item.conclusion == "identity");
    let key = evidence_ids(evidence, |item| {
        matches!(item.phase.as_str(), "models" | "routes" | "verify")
    });
    EvidenceRefs {
        identity: own_or_first(identity, evidence),
        models: evidence_ids(evidence, |item| item.conclusion == "models"),
        key: own_or_first(key, evidence),
        balance: evidence_ids(evidence, |item| item.conclusion == "balance"),
    }
}

/// The listed models with local-runtime data, catalogue and preset prefill, every string from
/// the list sanitised; a model whose sanitised id an earlier one has is dropped. Takes the model
/// list out of `state`.
fn sniffed_models(
    ex: &Exec<'_>,
    ctx: &SniffContext<'_>,
    state: &mut State,
    preset: Option<&ProviderPreset>,
) -> Vec<SniffedModel> {
    let mut raw = std::mem::take(&mut state.models.raw);
    let n_ctx = state.facts.local.as_ref().and_then(|hit| hit.context);
    merge_local(&mut raw, state.local.lm_studio_list(), n_ctx);
    let mut models = finalize(raw, ctx.catalog, preset);
    let sanitize_all = |texts: &mut Vec<String>| {
        for text in texts {
            *text = ex.sanitize(text);
        }
    };
    let mut seen = HashSet::new();
    models.retain_mut(|model| {
        model.id = ex.sanitize(&model.id);
        model.display_name = model.display_name.take().map(|name| ex.sanitize(&name));
        model.owned_by = model.owned_by.take().map(|owner| ex.sanitize(&owner));
        sanitize_all(&mut model.declared_endpoint_types);
        sanitize_all(&mut model.reasoning_options);
        sanitize_all(&mut model.modalities);
        seen.insert(model.id.clone())
    });
    models
}

/// The route results in plan order.
fn ordered_routes(plan: &Plan, mut routes: Vec<RouteResult>) -> Vec<RouteResult> {
    routes.sort_by_key(|route| {
        plan.routes
            .iter()
            .position(|spec| spec.protocol == route.protocol)
            .unwrap_or(usize::MAX)
    });
    routes
}

/// One endpoint per probed route; `default` marks the primary protocol.
fn endpoints(
    ex: &Exec<'_>,
    url: &NormalizedUrl,
    plan: &Plan,
    routes: &[RouteResult],
    evidence: &[EvidenceItem],
    default: Option<Protocol>,
) -> Vec<SniffedEndpoint> {
    routes
        .iter()
        .map(|route| {
            let verbatim = plan.verbatim == Some(route.protocol);
            let conclusion = route_conclusion(route.protocol);
            SniffedEndpoint {
                protocol: route.protocol,
                path_prefix: route.mount.clone(),
                url_mode: if verbatim {
                    EndpointUrlMode::AsTyped
                } else {
                    EndpointUrlMode::Standard
                },
                full_request_url_preview: ex.sanitize(&format!("{}{}", url.origin, route.path)),
                auth: route.auth,
                supported: route.verdict.is_supported(),
                route: route.verdict,
                http_status: route.status,
                latency_ms: route.latency_ms,
                is_default: default == Some(route.protocol),
                probe_method: "POST".to_owned(),
                probe_path_template: route_template(route.protocol, &route.mount, verbatim),
                sdk_base_url: sdk_base_url(&url.origin, route.protocol, &route.mount, verbatim),
                mount_candidates_tried: route.tried.clone(),
                evidence_ids: evidence_ids(evidence, |item| item.conclusion == conclusion),
                lenient_2xx: route.lenient_2xx,
                rate_limited: route.status == Some(429),
            }
        })
        .collect()
}

/// The key verdict from the credentialed answers: the model list first, then the routes in
/// order.
fn key_outcome(
    has_key: bool,
    state: &State,
    routes: &[RouteResult],
    auth_budget_gap: bool,
    stopped: bool,
    needs_confirmation: bool,
) -> (KeyCheckStatus, KeyEvidence) {
    let mut responses: Vec<(u16, &str)> = Vec::new();
    if has_key {
        if let Some(status) = state.models.status {
            responses.push((status, state.models.error_text.as_str()));
        }
        responses.extend(routes.iter().filter_map(|route| {
            route
                .status
                .map(|status| (status, route.error_text.as_str()))
        }));
    }
    let route_statuses = routes
        .iter()
        .filter_map(|route| route.status.map(|status| (route.protocol, status)))
        .collect();
    key_verdict(
        KeyFacts {
            has_key,
            responses: &responses,
            models_status: state.models.status.filter(|_| has_key),
            auth_budget_gap,
            stopped,
            needs_confirmation,
        },
        route_statuses,
    )
}

/// Connectivity and the status and latency of the primary route (else the first one).
fn key_check(
    endpoints: &[SniffedEndpoint],
    evidence: &[EvidenceItem],
    key_evidence: &KeyEvidence,
) -> KeyCheck {
    let primary_route = endpoints
        .iter()
        .find(|endpoint| endpoint.is_default)
        .or_else(|| endpoints.first());
    KeyCheck {
        connectivity_ok: evidence.iter().any(|item| item.status.is_some()),
        protocol: primary_route.map(|endpoint| endpoint.protocol),
        http_status: primary_route
            .and_then(|endpoint| endpoint.http_status)
            .or(key_evidence.models_status),
        latency_ms: primary_route.and_then(|endpoint| endpoint.latency_ms),
    }
}

/// Remembers, for the app session, the credential style that worked per family.
fn remember_auth_styles(scan: &Scan<'_>, ctx: &SniffContext<'_>) {
    for family in AuthFamily::ALL {
        if let Some(style) = scan.trials.get(family).winner() {
            ctx.auth_memory
                .remember(&scan.url.origin, family.representative(), style);
        }
    }
}

/// Adds the skips the plan decided up front, unless an item of the same name is there.
fn note_plan_skips(skipped: &mut Vec<SkippedItem>, had_models_attempts: bool, has_key: bool) {
    let mut note = |what: &str, reason: &str| {
        if !skipped.iter().any(|item| item.what == what) {
            skipped.push(SkippedItem {
                what: what.to_owned(),
                reason: reason.to_owned(),
            });
        }
    };
    if !had_models_attempts {
        note("models", "not_applicable");
    }
    if !has_key {
        note("balance", "no_key");
    }
}

/// The balance finding, reading and adapter; a step that never ran is "not attempted".
fn balance_parts(
    step: Option<BalanceStep>,
    stopped: bool,
) -> (BalanceFinding, Option<BalanceReport>, Option<String>) {
    match step {
        Some(step) => (step.finding, step.report, step.adapter),
        None => (
            BalanceFinding::NotAttempted {
                reason: if stopped { "stopped" } else { "not_run" }.to_owned(),
            },
            None,
            None,
        ),
    }
}

/// How a scan ended early, if it did: both leave a partial report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ScanEnd {
    /// The time limit ended the scan.
    pub timed_out: bool,
    /// The user stopped the scan.
    pub cancelled: bool,
}

/// Builds the report. The scan is over: the model list, the route results and the balance step
/// are taken out of the scan state.
pub(super) fn assemble(
    scan: &Scan<'_>,
    ctx: &SniffContext<'_>,
    collected: Collected,
    elapsed_ms: u64,
    ends: ScanEnd,
    key_concern: Option<KeyConcern>,
) -> SniffReport {
    let ex = scan.ex;
    let url = scan.url;
    let plan = scan.plan;
    let has_key = scan.key.is_some();
    let identity = scan.score_identity();
    let had_models_attempts = !scan.model_attempts().is_empty();
    let report_id = new_report_id();
    let Collected {
        evidence,
        warnings: exec_warnings,
        suggested_url,
        mut skipped,
        auth_budget_gap,
        stop,
        ..
    } = collected;
    let preset = plan
        .known
        .as_ref()
        .and_then(|known| presets().ok()?.provider(&known.preset_id));

    let mut state = scan.state();
    let local_hit = state.facts.local.is_some();
    let models = sniffed_models(ex, ctx, &mut state, preset);
    let routes = ordered_routes(plan, std::mem::take(&mut state.routes));
    let supported: Vec<Protocol> = routes
        .iter()
        .filter(|route| route.verdict.is_supported())
        .map(|route| route.protocol)
        .collect();
    let default = primary_among(url.tail_hint, |protocol| supported.contains(&protocol));
    let endpoints = endpoints(ex, url, plan, &routes, &evidence, default);
    let (key_status, key_evidence) = key_outcome(
        has_key,
        &state,
        &routes,
        auth_budget_gap,
        stop.is_some(),
        key_concern.is_some(),
    );
    let key_check = key_check(&endpoints, &evidence, &key_evidence);
    let models_enabled = u32::try_from(models.iter().filter(|model| model.default_enabled).count())
        .unwrap_or(u32::MAX);
    let models_disabled = u32::try_from(models.len())
        .unwrap_or(u32::MAX)
        .saturating_sub(models_enabled);
    let balance_step = state.balance.take();
    let tokens_spent = state.tokens_spent;
    let (models_endpoint, models_mount) = state
        .models
        .winner
        .take()
        .map(|attempt| (attempt.path, attempt.mount))
        .unzip();
    let models_curl_line = state
        .models
        .first_request
        .as_ref()
        .map(|request| {
            let url = redact_secrets(&request.url, &scan.input.secrets());
            models_curl(&url, request.protocol, request.style, has_key)
        })
        .unwrap_or_default();
    drop(state);

    if has_key {
        remember_auth_styles(scan, ctx);
    }

    let mut warnings = scan_warnings(
        &url.warnings,
        exec_warnings,
        key_concern,
        ends,
        identity.third_party,
    );
    add_preset_warnings(&mut warnings, plan, &routes, key_status, preset);

    note_plan_skips(&mut skipped, had_models_attempts, has_key);
    let (balance_finding, balance, balance_adapter) = balance_parts(balance_step, stop.is_some());
    let phases = PhaseInputs {
        plan,
        local_hit,
        evidence: &evidence,
        skipped: &skipped,
        any_models: !models.is_empty(),
        had_models_attempts,
        any_supported: !supported.is_empty(),
        no_routes: routes.is_empty(),
        stop,
        key_status,
        key_evidence: &key_evidence,
        balance_finding: &balance_finding,
        allow_billable_probe: scan.input.allow_billable_probe,
    }
    .summaries();
    let brand = identity
        .brand
        .as_deref()
        .or(preset.map(|provider| provider.name.as_str()))
        .unwrap_or(&url.host);

    let mut report = SniffReport {
        report_id,
        typed_url: url.typed.clone(),
        root_url: url.origin.clone(),
        family: identity.family,
        brand_name: Some(ex.sanitize(brand)),
        version: identity
            .version
            .as_deref()
            .map(|version| ex.sanitize(version)),
        confidence_score: identity.score,
        confirmed: identity.confirmed,
        label: identity.label,
        runner_up: identity.runner_up,
        quota_per_unit: identity.quota_per_unit,
        display_in_currency: identity.display_in_currency,
        quota_is_remaining: identity.quota_is_remaining,
        tokens_spent_unknown: endpoints.iter().any(|endpoint| endpoint.lenient_2xx),
        endpoints,
        models_endpoint,
        models_mount,
        models,
        models_enabled,
        models_disabled,
        key_status,
        key_check,
        needs_key_confirmation: key_concern.is_some(),
        key_evidence,
        balance,
        balance_adapter,
        balance_finding,
        suggested_preset_id: plan.known.as_ref().map(|known| known.preset_id.clone()),
        warnings: warnings.into_vec(),
        evidence_refs: evidence_refs(&evidence),
        evidence,
        requests_used: ex.used(),
        verify_requests_used: ex.verified(),
        created_at_ms: ctx
            .now_ms
            .saturating_add(i64::try_from(elapsed_ms).unwrap_or(i64::MAX)),
        elapsed_ms,
        tokens_spent,
        suggested_url,
        skipped,
        phases,
        models_curl: models_curl_line,
        diff: None,
    };
    report.diff = ctx
        .stored
        .map(|stored| diff_against_provider(&report, stored));
    report
}
