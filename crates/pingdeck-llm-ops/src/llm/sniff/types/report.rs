//! What a scan hands out: the report and its parts. The summary kept with a saved provider and the
//! evidence rows live in `pingdeck_llm_domain::llm::sniff_summary`.

use serde::{Deserialize, Serialize};

use super::{
    super::{diff::SniffDiff, models::ModelKind},
    PhaseSummary,
};
use crate::llm::{
    balance::BalanceReport,
    capabilities::ModelCapabilities,
    wire::{AuthStyle, EndpointUrlMode, GatewayFamily, Protocol},
};
pub(crate) use pingdeck_llm_domain::llm::sniff_summary::{EvidenceItem, RunnerUp};

/// A non-fatal observation of the scan. Serialised with a stable `code`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum SniffWarning {
    /// `user:pass@` was dropped from the address.
    UserinfoIgnored,
    /// A `?key=` style query was dropped; it is never used as a credential.
    KeyInUrlIgnored,
    /// A dashboard path (`/console/...`) was cut off.
    DashboardPathIgnored,
    /// The address is plain http on a public host.
    PlainHttpPublic,
    /// A probe answered 3xx; it was not followed (the target host only, no query).
    RedirectIgnored {
        /// Host of the `Location`.
        location_host: Option<String>,
    },
    /// A body was cut at the cap.
    BodyTruncated,
    /// A model list had more than 5 pages.
    ListTruncated,
    /// A probe was rate limited twice.
    RateLimited,
    /// The host looks like a typo of a known provider host; the key was not sent.
    LookalikeHost,
    /// The key shape belongs to another vendor than the host; the key was not sent.
    VendorKeyOnOtherHost,
    /// A route the preset lists was not found on the host ("Differs from preset").
    PresetRouteMismatch {
        /// The protocol the preset lists.
        protocol: Protocol,
    },
    /// The key probably belongs to the sibling region of the preset.
    RegionMismatch {
        /// The region to try instead.
        suggested_region_id: String,
    },
    /// The host answered with a web page, not an API.
    HtmlPage,
    /// The host answered with a bot-challenge page.
    ChallengePage,
    /// The 30 s scan limit ended the scan; the report is partial.
    ScanTimeout,
    /// The user stopped the scan; the report holds what answered before and can still be added.
    ScanCancelled,
    /// The identity rests on third-party sources only (Veloera).
    BrandThirdParty,
}

/// How sure the identity is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityLabel {
    /// Accepted and at least one signal came from a response of this host.
    Confirmed,
    /// Accepted on the domain rule alone.
    Preset,
    /// Not accepted; the model list and routes still show.
    CompatibleUnidentified,
}

/// Whether a protocol's route exists (`Works` after a verified real request).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteVerdict {
    /// The route exists.
    Exists,
    /// The route does not exist.
    Absent,
    /// No verdict (3xx, 5xx, or a 401/403 without the protocol's error envelope).
    Unclear,
    /// A real request succeeded.
    Works,
}

impl RouteVerdict {
    /// Whether the protocol can be saved (`Exists` or `Works`).
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Exists | Self::Works)
    }
}

/// One probed protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SniffedEndpoint {
    /// The protocol.
    pub protocol: Protocol,
    /// Mount relative to the origin (`/v1`, `/v1beta`, ``).
    pub path_prefix: String,
    /// `AsTyped` when the user typed `#`.
    pub url_mode: EndpointUrlMode,
    /// The full probe URL (no query), e.g. `https://x/v1/chat/completions`.
    pub full_request_url_preview: String,
    /// The auth style that worked (or the first one tried when none did).
    pub auth: AuthStyle,
    /// Whether the route exists (`route` is `Exists` or `Works`).
    pub supported: bool,
    /// The verdict.
    pub route: RouteVerdict,
    /// HTTP status of the probe.
    pub http_status: Option<u16>,
    /// Probe latency.
    pub latency_ms: Option<u64>,
    /// The default primary protocol; at most one endpoint has it.
    pub is_default: bool,
    /// `POST`.
    pub probe_method: String,
    /// The probe path with `{m}` for the model.
    pub probe_path_template: String,
    /// What a user would paste into an SDK (`https://x/v1`; Messages and Gemini move the
    /// trailing version to the request path).
    pub sdk_base_url: String,
    /// Every mount tried for this protocol.
    pub mount_candidates_tried: Vec<String>,
    /// Evidence rows supporting the verdict.
    pub evidence_ids: Vec<u32>,
    /// The probe answered 2xx to the empty request: the host may have run a short completion.
    pub lenient_2xx: bool,
    /// The probe was still rate limited after its one retry.
    pub rate_limited: bool,
}

/// One listed model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SniffedModel {
    /// The id as the provider spells it.
    pub id: String,
    /// A friendlier name when the list gives one.
    pub display_name: Option<String>,
    /// `owned_by` of the list entry.
    pub owned_by: Option<String>,
    /// Chat, embedding, image, audio, rerank or moderation.
    pub kind: ModelKind,
    /// Context window in tokens.
    pub context_window: Option<u64>,
    /// Maximum output tokens.
    pub max_output: Option<u64>,
    /// Declared and catalogue capabilities (never `Measured`).
    pub capabilities: ModelCapabilities,
    /// `supported_endpoint_types` as declared (new-api).
    pub declared_endpoint_types: Vec<String>,
    /// Reasoning-effort options the model declares (LM Studio `allowed_options`).
    pub reasoning_options: Vec<String>,
    /// Input and output modalities as declared.
    pub modalities: Vec<String>,
    /// USD per million input tokens.
    pub input_cost_per_mtok: Option<f64>,
    /// USD per million output tokens.
    pub output_cost_per_mtok: Option<f64>,
    /// The host's own list declared both prices (`pricing.prompt` and `pricing.completion`):
    /// adding the provider saves them as its price of the model. `false` when the prices come
    /// from the catalog or are missing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub price_declared: bool,
    /// Whether the row starts enabled.
    pub default_enabled: bool,
}

/// Evidence rows behind each report-level conclusion (every conclusion has one).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRefs {
    /// Rows supporting the identity.
    pub identity: Vec<u32>,
    /// Rows supporting the model list.
    pub models: Vec<u32>,
    /// Rows supporting the key verdict.
    pub key: Vec<u32>,
    /// Rows supporting the balance finding.
    pub balance: Vec<u32>,
}

/// The key verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyCheckStatus {
    /// No key typed, no credentialed answer, or answers that decide nothing (the reason is in
    /// `KeyEvidence::untested_reason`).
    Untested,
    /// A credentialed request succeeded.
    Accepted,
    /// 401 or 403 on every credentialed request.
    Rejected,
    /// 402 or a quota body.
    QuotaExhausted,
    /// Models list refused but a route probe passed auth.
    ModelsBlockedButChatWorks,
}

/// Connectivity and Auth results of a scan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyCheck {
    /// Some request got an answer.
    pub connectivity_ok: bool,
    /// The protocol of the request the status and latency come from (the primary route).
    pub protocol: Option<Protocol>,
    /// HTTP status of that request.
    pub http_status: Option<u16>,
    /// Latency of that request.
    pub latency_ms: Option<u64>,
}

/// The statuses behind the key line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEvidence {
    /// Status of the models request.
    pub models_status: Option<u16>,
    /// Status of every route probe.
    pub route_statuses: Vec<(Protocol, u16)>,
    /// Why the verdict is `Untested` (`no_key`, `unreachable`, `inconclusive`, `budget`,
    /// `stopped`), or `needs_confirmation` whatever the verdict when the key was held back.
    pub untested_reason: Option<String>,
}

/// What the balance step found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BalanceFinding {
    /// A balance was read (see `SniffReport::balance`).
    Found,
    /// No adapter or no endpoint answered ("No balance endpoint found").
    NoEndpoint,
    /// An adapter ran and failed; `error` is its short kind.
    Failed {
        /// The adapter's error kind.
        error: String,
    },
    /// Not attempted: `no_key`, `budget`, `stopped`, or `not_run` when the step never ran.
    NotAttempted {
        /// Why.
        reason: String,
    },
}

/// A request or conclusion left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedItem {
    /// `identify`, `models`, `route:<protocol>`, `balance`, `verify`.
    pub what: String,
    /// `budget`, `stopped`, `no_key`, `not_applicable`.
    pub reason: String,
}

/// The result of a scan. Nothing is saved; the caller keeps it in the report cache.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SniffReport {
    /// Cache key.
    pub report_id: String,
    /// The normalised address as typed.
    pub typed_url: String,
    /// The origin the scan was confined to.
    pub root_url: String,
    /// The detected family.
    pub family: GatewayFamily,
    /// Brand (`system_name`, `site_name` or the host).
    pub brand_name: Option<String>,
    /// Version from the family's field or header.
    pub version: Option<String>,
    /// Score of the winner.
    pub confidence_score: i32,
    /// Whether a signal came from a response of this host and the winner was accepted.
    pub confirmed: bool,
    /// Confirmed, Preset or Compatible, unidentified.
    pub label: IdentityLabel,
    /// The second-best family.
    pub runner_up: Option<RunnerUp>,
    /// `quota_per_unit` harvested from `/api/status`.
    pub quota_per_unit: Option<f64>,
    /// `display_in_currency` harvested from `/api/status`.
    pub display_in_currency: Option<bool>,
    /// done-hub semantics: `quota` is what is left.
    pub quota_is_remaining: bool,
    /// One entry per probed protocol.
    pub endpoints: Vec<SniffedEndpoint>,
    /// Model list path relative to the origin that answered.
    pub models_endpoint: Option<String>,
    /// The mount that answered the model list (OpenAI-style routes reuse it).
    pub models_mount: Option<String>,
    /// Listed models, uncapped (≤ 2 MiB body).
    pub models: Vec<SniffedModel>,
    /// Listed models that start enabled (Chat and Embedding kinds).
    pub models_enabled: u32,
    /// Listed models that are saved disabled (every other kind).
    pub models_disabled: u32,
    /// The key verdict.
    pub key_status: KeyCheckStatus,
    /// The Connectivity and Auth results of the scan, for the measured results saved with the
    /// provider (source `sniff`).
    pub key_check: KeyCheck,
    /// The scan stopped before any keyed request: a look-alike host or a foreign key shape
    /// needs "Send key anyway".
    pub needs_key_confirmation: bool,
    /// Some route answered 2xx to the empty request, so no "no tokens spent" claim is made.
    pub tokens_spent_unknown: bool,
    /// The statuses behind the key verdict.
    pub key_evidence: KeyEvidence,
    /// The balance reading.
    pub balance: Option<BalanceReport>,
    /// The balance adapter that was (or would be) used (`balance::AdapterId::as_str`).
    pub balance_adapter: Option<String>,
    /// What the balance step found.
    pub balance_finding: BalanceFinding,
    /// The preset that matches the host (or the one the user picked).
    pub suggested_preset_id: Option<String>,
    /// Non-fatal observations.
    pub warnings: Vec<SniffWarning>,
    /// Redacted request log.
    pub evidence: Vec<EvidenceItem>,
    /// Evidence rows behind each conclusion.
    pub evidence_refs: EvidenceRefs,
    /// Requests sent, retries included; local fingerprints are not counted.
    pub requests_used: u32,
    /// The opt-in verification requests among `requests_used`: outside the scan budget and
    /// listed apart because they may cost money.
    #[serde(default)]
    pub verify_requests_used: u32,
    /// When the scan finished, Unix ms: the wizard warns before saving a stale report.
    #[serde(default)]
    pub created_at_ms: i64,
    /// Wall time of the scan.
    pub elapsed_ms: u64,
    /// Tokens spent by real requests (0 unless `allow_billable_probe`).
    pub tokens_spent: u64,
    /// Same-host address a 3xx pointed to.
    pub suggested_url: Option<String>,
    /// Requests left out and why.
    pub skipped: Vec<SkippedItem>,
    /// Per-phase outcome.
    pub phases: Vec<PhaseSummary>,
    /// `curl` line of the first model-list request with `$PROVIDER_API_KEY`.
    pub models_curl: String,
    /// Changes since the saved provider's last scan (re-sniff only).
    pub diff: Option<SniffDiff>,
}
