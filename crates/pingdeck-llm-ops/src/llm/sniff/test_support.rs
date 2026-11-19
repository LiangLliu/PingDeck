//! Builders shared by the sniffer's unit tests: blank reports, endpoints and models, so a test
//! only states the fields it is about.

use super::{
    models::ModelKind,
    types::{
        BalanceFinding, EvidenceRefs, IdentityLabel, KeyCheck, KeyCheckStatus, KeyEvidence,
        RouteVerdict, SniffReport, SniffedEndpoint, SniffedModel,
    },
};
use crate::llm::{
    capabilities::ModelCapabilities,
    wire::{AuthStyle, EndpointUrlMode, GatewayFamily, Protocol},
};

/// A report with every field empty.
pub(super) fn blank_report(report_id: &str) -> SniffReport {
    SniffReport {
        report_id: report_id.to_owned(),
        typed_url: "https://x.test".to_owned(),
        root_url: "https://x.test".to_owned(),
        family: GatewayFamily::UnknownCompatible,
        brand_name: None,
        version: None,
        confidence_score: 0,
        confirmed: false,
        label: IdentityLabel::CompatibleUnidentified,
        runner_up: None,
        quota_per_unit: None,
        display_in_currency: None,
        quota_is_remaining: false,
        endpoints: Vec::new(),
        models_endpoint: None,
        models_mount: None,
        models: Vec::new(),
        models_enabled: 0,
        models_disabled: 0,
        key_status: KeyCheckStatus::Untested,
        key_check: KeyCheck::default(),
        needs_key_confirmation: false,
        tokens_spent_unknown: false,
        key_evidence: KeyEvidence::default(),
        balance: None,
        balance_adapter: None,
        balance_finding: BalanceFinding::NoEndpoint,
        suggested_preset_id: None,
        warnings: Vec::new(),
        evidence: Vec::new(),
        evidence_refs: EvidenceRefs::default(),
        requests_used: 0,
        verify_requests_used: 0,
        created_at_ms: 0,
        elapsed_ms: 0,
        tokens_spent: 0,
        suggested_url: None,
        skipped: Vec::new(),
        phases: Vec::new(),
        models_curl: String::new(),
        diff: None,
    }
}

/// An endpoint with the given verdict.
pub(super) fn endpoint(protocol: Protocol, route: RouteVerdict) -> SniffedEndpoint {
    SniffedEndpoint {
        protocol,
        path_prefix: "/v1".to_owned(),
        url_mode: EndpointUrlMode::Standard,
        full_request_url_preview: String::new(),
        auth: AuthStyle::Bearer,
        supported: route.is_supported(),
        route,
        http_status: None,
        latency_ms: None,
        is_default: false,
        probe_method: "POST".to_owned(),
        probe_path_template: String::new(),
        sdk_base_url: String::new(),
        mount_candidates_tried: Vec::new(),
        evidence_ids: Vec::new(),
        lenient_2xx: false,
        rate_limited: false,
    }
}

/// A chat model with only an id.
pub(super) fn model(id: &str) -> SniffedModel {
    SniffedModel {
        id: id.to_owned(),
        display_name: None,
        owned_by: None,
        kind: ModelKind::Chat,
        context_window: None,
        max_output: None,
        capabilities: ModelCapabilities::default(),
        declared_endpoint_types: Vec::new(),
        reasoning_options: Vec::new(),
        modalities: Vec::new(),
        input_cost_per_mtok: None,
        output_cost_per_mtok: None,
        price_declared: false,
        default_enabled: true,
    }
}
