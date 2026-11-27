//! Headless Layer 2 projection adapter mapping HTTP Request [`Flow`] blueprints and live/past
//! execution overlays onto the unified [`super::process_graph`] layout engine.
//!
//! Responsibilities:
//! - Translates linear (`position`-ordered) and DAG (`depends_on` / `Condition` branches / `Join`)
//!   flows into heterogeneous [`ProcessNodeSpec`] + layered [`ProcessEdgeSpec`] graphs.
//! - Statically extracts `{{var}}` references from URLs, headers, queries, bodies, and condition
//!   expressions to synthesize secondary [`ProcessEdgeLayer::DataFlow`] dependency arcs.
//! - Maps HTTP [`PhaseTiming`] into segmented [`ProcessTimingPhase`] bars for the shared Timeline.
//! - Computes adaptive default layout policies (`LR` for 1-4 steps or swimlanes, 4-col `Serpentine`
//!   wrap for 5+ step pipelines without swimlanes).

use std::collections::{HashMap, HashSet};

use spanbay_core::flows::has_dag_topology;
use spanbay_core::model::Request;
use spanbay_core::model::items::{
    Flow, FlowLayoutDirectionPref, FlowRunResult, FlowRunSummary, FlowStep, FlowStepKind,
    FlowStepResult, FlowStepSummary,
};
use spanbay_core::model::models::PhaseTiming;

use super::process_graph::{
    CrossAlignment, DataFlowVisibility, LayoutDirection, LayoutPolicy, PositionedPort,
    ProcessEdgeKind, ProcessEdgeLayer, ProcessEdgeSpec, ProcessEdgeTone, ProcessGraphGeometry,
    ProcessGraphInput, ProcessLaneSpec, ProcessNodeArchetype, ProcessNodeSpec, ProcessNodeStatus,
    ProcessPortSpec, ProcessTimingPhase, layout_process_graph,
};

pub const FLOW_CARD_W: f32 = 256.0;
pub const FLOW_CARD_H: f32 = 82.0;
pub const FLOW_CARD_ALERT_H: f32 = 104.0;
pub const FLOW_JOIN_W: f32 = 196.0;
pub const FLOW_JOIN_H: f32 = 64.0;
pub const FLOW_COND_W: f32 = 252.0;

/// Orthogonal presentation modes for `FlowsView`, mirroring Agent's `RunViewMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FlowViewMode {
    Steps,
    #[default]
    Graph,
    Timeline,
}

impl FlowViewMode {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Steps => 0,
            Self::Graph => 1,
            Self::Timeline => 2,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            0 => Self::Steps,
            2 => Self::Timeline,
            _ => Self::Graph,
        }
    }
}

/// Normalized runtime overlay for one flow step, unified across live events, full results, and
/// persisted historical summaries.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FlowStepRunOverlay {
    pub status: ProcessNodeStatus,
    pub status_code: Option<u16>,
    pub elapsed_ms: Option<u64>,
    pub started_offset_ms: Option<u64>,
    pub attempts: u32,
    pub passed: Option<bool>,
    pub skipped: bool,
    pub error: Option<String>,
    pub condition: Option<String>,
    pub matched_branch_id: Option<String>,
    pub assertions_passed: usize,
    pub assertions_failed: usize,
    pub phases: Vec<ProcessTimingPhase>,
}

/// Active execution data source overlaid on a [`Flow`] blueprint.
#[derive(Debug, Clone, Copy)]
pub enum FlowOverlaySource<'a> {
    DesignOnly,
    Live {
        steps: &'a HashMap<String, FlowStepSummary>,
    },
    Result(&'a FlowRunResult),
    PastSummary(&'a FlowRunSummary),
}

/// One laid-out Flow step card with geometry, runtime overlay, and variable dataflow metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowGraphNode {
    pub step: FlowStep,
    pub order_index: usize,
    pub stage: usize,
    pub lane_id: Option<String>,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub pinned: bool,
    pub ports: Vec<PositionedPort>,
    pub method: String,
    pub target_url: String,
    pub linked_request_name: Option<String>,
    pub overlay: FlowStepRunOverlay,
    pub produces_vars: Vec<(String, bool)>,
    pub consumes_vars: Vec<String>,
}

/// Complete laid-out HTTP Flow graph ready for `Graph`, `Steps`, and `Timeline` views.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowGraphLayout {
    pub geometry: ProcessGraphGeometry,
    pub nodes: Vec<FlowGraphNode>,
    pub has_dag: bool,
    pub has_pinned: bool,
    pub total_steps: usize,
    pub passed_steps: usize,
    pub failed_steps: usize,
    pub skipped_steps: usize,
    pub running_steps: usize,
}

impl FlowGraphLayout {
    #[must_use]
    pub fn node(&self, step_id: &str) -> Option<&FlowGraphNode> {
        self.geometry
            .node_index
            .get(step_id)
            .and_then(|&i| self.nodes.get(i))
    }

    /// Smart default focus after a run completes or when inspecting a failed historical run:
    /// selects the first failed step so the Inspector opens directly to the failure explanation.
    #[must_use]
    pub fn default_focus_step_id(&self) -> Option<String> {
        self.nodes
            .iter()
            .find(|n| n.overlay.status.is_terminal_error())
            .map(|n| n.step.id.clone())
    }

    /// Immediate control-flow upstream `(step_id, step_name)` pairs of `step_id`.
    #[must_use]
    pub fn upstream_steps(&self, step_id: &str) -> Vec<(String, String)> {
        self.geometry
            .edges
            .iter()
            .filter(|e| e.layer == ProcessEdgeLayer::Control && e.to_node == step_id)
            .filter_map(|e| {
                self.node(&e.from_node)
                    .map(|n| (n.step.id.clone(), n.step.name.clone()))
            })
            .collect()
    }

    /// Immediate control-flow downstream `(step_id, step_name)` pairs of `step_id`.
    #[must_use]
    pub fn downstream_steps(&self, step_id: &str) -> Vec<(String, String)> {
        self.geometry
            .edges
            .iter()
            .filter(|e| e.layer == ProcessEdgeLayer::Control && e.from_node == step_id)
            .filter_map(|e| {
                self.node(&e.to_node)
                    .map(|n| (n.step.id.clone(), n.step.name.clone()))
            })
            .collect()
    }
}

/// Resolves the effective [`LayoutPolicy`] for `flow` (best-practice adaptive defaults).
#[must_use]
pub fn effective_flow_layout_policy(flow: &Flow) -> LayoutPolicy {
    let pref = &flow.options.layout;
    let swimlanes = pref.swimlanes && !flow.options.lanes.is_empty();
    let direction = match pref.direction {
        FlowLayoutDirectionPref::Auto => {
            if !swimlanes && !has_dag_topology(flow) && flow.steps.len() >= 5 {
                LayoutDirection::Serpentine {
                    max_per_row: LayoutDirection::DEFAULT_SERPENTINE_COLS,
                }
            } else {
                LayoutDirection::LeftToRight
            }
        }
        FlowLayoutDirectionPref::LeftToRight => LayoutDirection::LeftToRight,
        FlowLayoutDirectionPref::TopToBottom => LayoutDirection::TopToBottom,
        FlowLayoutDirectionPref::Serpentine => {
            if swimlanes {
                LayoutDirection::LeftToRight
            } else {
                LayoutDirection::Serpentine {
                    max_per_row: LayoutDirection::DEFAULT_SERPENTINE_COLS,
                }
            }
        }
    };
    LayoutPolicy {
        direction,
        alignment: if pref.center_align {
            CrossAlignment::Center
        } else {
            CrossAlignment::Start
        },
        swimlanes,
        data_flow: if pref.show_data_flow {
            DataFlowVisibility::OnFocus
        } else {
            DataFlowVisibility::Hidden
        },
    }
}

/// Projects `flow` + saved `requests` + execution `overlay_source` into a laid-out
/// [`FlowGraphLayout`] using the unified [`layout_process_graph`] engine.
#[must_use]
pub fn build_flow_graph_layout(
    flow: &Flow,
    requests: &[Request],
    overlay_source: FlowOverlaySource<'_>,
    running: bool,
) -> FlowGraphLayout {
    let policy = effective_flow_layout_policy(flow);
    let has_dag = has_dag_topology(flow);
    let req_map: HashMap<&str, &Request> = requests.iter().map(|r| (r.id.as_str(), r)).collect();

    let mut ordered_steps: Vec<FlowStep> = flow.steps.clone();
    ordered_steps.sort_by_key(|s| s.position);

    let overlays: Vec<FlowStepRunOverlay> = ordered_steps
        .iter()
        .map(|s| resolve_step_overlay(&s.id, overlay_source, running))
        .collect();

    let mut node_specs = Vec::with_capacity(ordered_steps.len());
    let mut produces_by_step: Vec<Vec<(String, bool)>> = Vec::with_capacity(ordered_steps.len());
    let mut consumes_by_step: Vec<Vec<String>> = Vec::with_capacity(ordered_steps.len());
    let mut method_urls: Vec<(String, String, Option<String>)> =
        Vec::with_capacity(ordered_steps.len());

    for (idx, step) in ordered_steps.iter().enumerate() {
        let linked = (!step.request_id.trim().is_empty())
            .then(|| req_map.get(step.request_id.trim()).copied())
            .flatten();
        let method = match step.step_kind {
            FlowStepKind::Request => step
                .method_override
                .or_else(|| linked.map(|r| r.method))
                .map(|m| format!("{m:?}").to_uppercase())
                .unwrap_or_else(|| "GET".to_owned()),
            FlowStepKind::Condition => "IF".to_owned(),
            FlowStepKind::Join => "JOIN".to_owned(),
        };
        let target_url = step
            .url_override
            .as_deref()
            .filter(|u| !u.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| linked.map(|r| r.url.clone()))
            .unwrap_or_default();
        let linked_name = linked.map(|r| r.name.clone());
        method_urls.push((method, target_url.clone(), linked_name));

        let produces: Vec<(String, bool)> = step
            .extractions
            .iter()
            .filter(|e| !e.variable_name.trim().is_empty())
            .map(|e| (e.variable_name.trim().to_owned(), e.is_secret()))
            .collect();
        produces_by_step.push(produces);

        let consumes = collect_step_template_vars(step, linked);
        consumes_by_step.push(consumes);

        let ov = &overlays[idx];
        let has_alert = ov.error.is_some();
        let (archetype, size_hint, ports) = match step.step_kind {
            FlowStepKind::Request => {
                let h = if has_alert {
                    FLOW_CARD_ALERT_H
                } else {
                    FLOW_CARD_H
                };
                (
                    ProcessNodeArchetype::Action,
                    (FLOW_CARD_W, h),
                    vec![ProcessPortSpec::input("in"), ProcessPortSpec::output("out")],
                )
            }
            FlowStepKind::Condition => {
                let mut ports = vec![ProcessPortSpec::input("in")];
                if step.branches.is_empty() {
                    ports.push(ProcessPortSpec::branch(
                        "out",
                        "Else",
                        ProcessEdgeTone::Neutral,
                    ));
                } else {
                    for (bi, br) in step.branches.iter().enumerate() {
                        let tone = if ov.matched_branch_id.as_deref() == Some(br.id.as_str()) {
                            ProcessEdgeTone::Completed
                        } else if ov.matched_branch_id.is_some() {
                            ProcessEdgeTone::Skipped
                        } else if bi == 0 {
                            ProcessEdgeTone::Active
                        } else {
                            ProcessEdgeTone::Warning
                        };
                        ports.push(ProcessPortSpec::branch(
                            format!("br:{}", br.id),
                            br.label.clone(),
                            tone,
                        ));
                    }
                }
                let branch_rows = step.branches.len().max(2) as f32;
                let h = (68.0 + branch_rows * 16.0).clamp(88.0, 148.0)
                    + if has_alert { 20.0 } else { 0.0 };
                (ProcessNodeArchetype::Decision, (FLOW_COND_W, h), ports)
            }
            FlowStepKind::Join => (
                ProcessNodeArchetype::Join,
                (
                    FLOW_JOIN_W,
                    FLOW_JOIN_H + if has_alert { 20.0 } else { 0.0 },
                ),
                vec![ProcessPortSpec::input("in"), ProcessPortSpec::output("out")],
            ),
        };

        let pinned_pos = flow
            .options
            .pinned_positions
            .get(&step.id)
            .map(|&(x, y)| (x as f32, y as f32));

        node_specs.push(ProcessNodeSpec {
            id: step.id.clone(),
            archetype,
            status: ov.status,
            lane_id: step.lane.clone(),
            stage_hint: None,
            size_hint,
            primary_port_offset: None,
            ports,
            is_group_container: false,
            pinned_pos,
        });
    }

    let id_to_idx: HashMap<&str, usize> = ordered_steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();

    // 1. Control-flow edges (linear chain when `!has_dag`, otherwise `depends_on` + `branches`).
    let mut edge_specs: Vec<ProcessEdgeSpec> = Vec::new();
    let mut control_pairs: HashSet<(usize, usize)> = HashSet::new();

    if !has_dag {
        for pair in ordered_steps.windows(2) {
            let from = &pair[0];
            let to = &pair[1];
            let fi = id_to_idx[from.id.as_str()];
            let ti = id_to_idx[to.id.as_str()];
            control_pairs.insert((fi, ti));
            let (tone, dashed) = classify_flow_edge_tone(overlays[fi].status, overlays[ti].status);
            edge_specs.push(ProcessEdgeSpec {
                id: format!("ctrl:{}->{}", from.id, to.id),
                from_node: from.id.clone(),
                from_port: "out".to_owned(),
                to_node: to.id.clone(),
                to_port: "in".to_owned(),
                layer: ProcessEdgeLayer::Control,
                kind: ProcessEdgeKind::Sequence,
                label: None,
                tone,
                dashed,
                insertable: true,
            });
        }
    } else {
        // Explicit `branches` from Condition routers first so their labels & ports take precedence.
        for (fi, step) in ordered_steps.iter().enumerate() {
            if step.step_kind != FlowStepKind::Condition {
                continue;
            }
            let matched = overlays[fi].matched_branch_id.as_deref();
            for br in &step.branches {
                for target_id in &br.target_step_ids {
                    let Some(&ti) = id_to_idx.get(target_id.as_str()) else {
                        continue;
                    };
                    if fi == ti {
                        continue;
                    }
                    control_pairs.insert((fi, ti));
                    let (tone, dashed) = match matched {
                        Some(mid) if mid == br.id => (ProcessEdgeTone::Completed, false),
                        Some(_) => (ProcessEdgeTone::Skipped, true),
                        None => (ProcessEdgeTone::Active, false),
                    };
                    edge_specs.push(ProcessEdgeSpec {
                        id: format!("br:{}:{}->{}", br.id, step.id, target_id),
                        from_node: step.id.clone(),
                        from_port: format!("br:{}", br.id),
                        to_node: target_id.clone(),
                        to_port: "in".to_owned(),
                        layer: ProcessEdgeLayer::Control,
                        kind: ProcessEdgeKind::Conditional,
                        label: (!br.label.trim().is_empty()).then(|| br.label.clone()),
                        tone,
                        dashed,
                        insertable: true,
                    });
                }
            }
        }
        // Explicit `depends_on` edges.
        for (ti, step) in ordered_steps.iter().enumerate() {
            for dep_id in &step.depends_on {
                let Some(&fi) = id_to_idx.get(dep_id.as_str()) else {
                    continue;
                };
                if fi == ti || !control_pairs.insert((fi, ti)) {
                    continue;
                }
                let (tone, dashed) =
                    classify_flow_edge_tone(overlays[fi].status, overlays[ti].status);
                let kind = if step.step_kind == FlowStepKind::Join {
                    ProcessEdgeKind::ParallelJoin
                } else {
                    ProcessEdgeKind::Sequence
                };
                edge_specs.push(ProcessEdgeSpec {
                    id: format!("dep:{dep_id}->{}", step.id),
                    from_node: dep_id.clone(),
                    from_port: "out".to_owned(),
                    to_node: step.id.clone(),
                    to_port: "in".to_owned(),
                    layer: ProcessEdgeLayer::Control,
                    kind,
                    label: None,
                    tone,
                    dashed,
                    insertable: true,
                });
            }
        }
    }

    // 2. Static `{{var}}` DataFlow edges (producer -> consumer).
    let mut latest_producer: HashMap<String, (usize, bool)> = HashMap::new();
    for (idx, step) in ordered_steps.iter().enumerate() {
        let mut emitted_for_step: HashSet<(usize, String)> = HashSet::new();
        for var in &consumes_by_step[idx] {
            if let Some(&(prod_idx, is_secret)) = latest_producer.get(var)
                && prod_idx != idx
                && emitted_for_step.insert((prod_idx, var.clone()))
            {
                let lbl = if is_secret {
                    format!("{{{{{var}}}}} 🔒")
                } else {
                    format!("{{{{{var}}}}}")
                };
                edge_specs.push(ProcessEdgeSpec {
                    id: format!("df:{}:{}->{}", var, ordered_steps[prod_idx].id, step.id),
                    from_node: ordered_steps[prod_idx].id.clone(),
                    from_port: "out".to_owned(),
                    to_node: step.id.clone(),
                    to_port: "in".to_owned(),
                    layer: ProcessEdgeLayer::DataFlow,
                    kind: ProcessEdgeKind::DataFlow,
                    label: Some(lbl),
                    tone: ProcessEdgeTone::Data,
                    dashed: true,
                    insertable: false,
                });
            }
        }
        for (var, is_secret) in &produces_by_step[idx] {
            latest_producer.insert(var.clone(), (idx, *is_secret));
        }
    }

    let lanes: Vec<ProcessLaneSpec> = flow
        .options
        .lanes
        .iter()
        .map(|l| ProcessLaneSpec {
            id: l.id.clone(),
            title: l.title.clone(),
            accent_index: l.accent_index,
        })
        .collect();

    let geometry = layout_process_graph(&ProcessGraphInput {
        nodes: node_specs,
        edges: edge_specs,
        lanes,
        policy,
    });

    let mut step_by_id: HashMap<String, (usize, FlowStep)> = ordered_steps
        .into_iter()
        .enumerate()
        .map(|(i, s)| (s.id.clone(), (i, s)))
        .collect();

    let mut nodes = Vec::with_capacity(geometry.nodes.len());
    let mut passed_steps = 0usize;
    let mut failed_steps = 0usize;
    let mut skipped_steps = 0usize;
    let mut running_steps = 0usize;

    for pn in &geometry.nodes {
        let Some((order_index, step)) = step_by_id.remove(&pn.id) else {
            continue;
        };
        let overlay = overlays[order_index].clone();
        match overlay.status {
            ProcessNodeStatus::Completed => passed_steps += 1,
            ProcessNodeStatus::Failed | ProcessNodeStatus::Interrupted => failed_steps += 1,
            ProcessNodeStatus::Skipped => skipped_steps += 1,
            ProcessNodeStatus::Running | ProcessNodeStatus::Queued => running_steps += 1,
            _ => {}
        }
        let (method, target_url, linked_request_name) = method_urls[order_index].clone();
        nodes.push(FlowGraphNode {
            step,
            order_index,
            stage: pn.stage,
            lane_id: pn.lane_id.clone(),
            x: pn.x,
            y: pn.y,
            w: pn.w,
            h: pn.h,
            pinned: pn.pinned,
            ports: pn.ports.clone(),
            method,
            target_url,
            linked_request_name,
            overlay,
            produces_vars: produces_by_step[order_index].clone(),
            consumes_vars: consumes_by_step[order_index].clone(),
        });
    }

    let total_steps = nodes.len();
    let has_pinned = geometry.has_pinned_nodes;

    FlowGraphLayout {
        geometry,
        nodes,
        has_dag,
        has_pinned,
        total_steps,
        passed_steps,
        failed_steps,
        skipped_steps,
        running_steps,
    }
}

fn resolve_step_overlay(
    step_id: &str,
    source: FlowOverlaySource<'_>,
    running: bool,
) -> FlowStepRunOverlay {
    match source {
        FlowOverlaySource::Live { steps } => {
            if let Some(s) = steps.get(step_id) {
                overlay_from_summary(s)
            } else if running {
                FlowStepRunOverlay {
                    status: ProcessNodeStatus::Running,
                    ..Default::default()
                }
            } else {
                FlowStepRunOverlay::default()
            }
        }
        FlowOverlaySource::Result(res) => res
            .steps
            .iter()
            .find(|s| s.step_id == step_id)
            .map(overlay_from_result)
            .unwrap_or_default(),
        FlowOverlaySource::PastSummary(sum) => sum
            .steps
            .iter()
            .find(|s| s.step_id == step_id)
            .map(overlay_from_summary)
            .unwrap_or_default(),
        FlowOverlaySource::DesignOnly => FlowStepRunOverlay::default(),
    }
}

fn overlay_from_summary(s: &FlowStepSummary) -> FlowStepRunOverlay {
    let status = if s.skipped {
        ProcessNodeStatus::Skipped
    } else if s.passed {
        ProcessNodeStatus::Completed
    } else {
        ProcessNodeStatus::Failed
    };
    FlowStepRunOverlay {
        status,
        status_code: s.status_code,
        elapsed_ms: Some(s.elapsed_ms),
        started_offset_ms: s.started_offset_ms,
        attempts: s.attempts,
        passed: Some(s.passed),
        skipped: s.skipped,
        error: s.error.clone(),
        condition: s.condition.clone(),
        matched_branch_id: s.matched_branch_id.clone(),
        assertions_passed: s.assertions_passed,
        assertions_failed: s.assertions_failed,
        phases: Vec::new(),
    }
}

fn overlay_from_result(r: &FlowStepResult) -> FlowStepRunOverlay {
    let status = if r.skipped {
        ProcessNodeStatus::Skipped
    } else if r.passed {
        ProcessNodeStatus::Completed
    } else {
        ProcessNodeStatus::Failed
    };
    let failed = r.assertion_results.iter().filter(|a| !a.passed).count();
    let passed = r.assertion_results.len().saturating_sub(failed);
    FlowStepRunOverlay {
        status,
        status_code: r.status_code,
        elapsed_ms: Some(r.elapsed_ms),
        started_offset_ms: r.started_offset_ms,
        attempts: r.attempts,
        passed: Some(r.passed),
        skipped: r.skipped,
        error: r
            .explanation
            .as_ref()
            .map(|e| e.message.clone())
            .or_else(|| r.error.clone()),
        condition: r.condition.clone(),
        matched_branch_id: r.matched_branch_id.clone(),
        assertions_passed: passed,
        assertions_failed: failed,
        phases: r.timing.map(timing_to_phases).unwrap_or_default(),
    }
}

fn timing_to_phases(t: PhaseTiming) -> Vec<ProcessTimingPhase> {
    let mut out = Vec::with_capacity(4);
    let dns = t.dns_ms.ms().unwrap_or(0);
    if dns > 0 {
        out.push(ProcessTimingPhase {
            label: "DNS".to_owned(),
            duration_ms: dns,
            tone: ProcessEdgeTone::Neutral,
        });
    }
    let connect = t
        .connect_ms
        .ms()
        .unwrap_or(0)
        .saturating_add(if t.connect_includes_tls {
            0
        } else {
            t.tls_ms.ms().unwrap_or(0)
        });
    if connect > 0 {
        out.push(ProcessTimingPhase {
            label: "TLS/TCP".to_owned(),
            duration_ms: connect,
            tone: ProcessEdgeTone::Warning,
        });
    }
    let req_ms = u64::try_from(t.request_ms).unwrap_or(u64::MAX);
    let wait = t
        .wait_ms
        .unwrap_or_else(|| req_ms.saturating_sub(dns.saturating_add(connect)));
    if wait > 0 {
        out.push(ProcessTimingPhase {
            label: "TTFB".to_owned(),
            duration_ms: wait,
            tone: ProcessEdgeTone::Active,
        });
    }
    let body = u64::try_from(t.response_ms).unwrap_or(u64::MAX);
    if body > 0 {
        out.push(ProcessTimingPhase {
            label: "Body".to_owned(),
            duration_ms: body,
            tone: ProcessEdgeTone::Completed,
        });
    }
    out
}

fn classify_flow_edge_tone(
    from: ProcessNodeStatus,
    to: ProcessNodeStatus,
) -> (ProcessEdgeTone, bool) {
    if matches!(to, ProcessNodeStatus::Skipped) || matches!(from, ProcessNodeStatus::Cancelled) {
        return (ProcessEdgeTone::Skipped, true);
    }
    if from.is_terminal_error() || to.is_terminal_error() {
        return (ProcessEdgeTone::Danger, false);
    }
    if matches!(from, ProcessNodeStatus::Running) || matches!(to, ProcessNodeStatus::Running) {
        return (ProcessEdgeTone::Active, true);
    }
    if matches!(from, ProcessNodeStatus::Completed) && matches!(to, ProcessNodeStatus::Completed) {
        return (ProcessEdgeTone::Completed, false);
    }
    (ProcessEdgeTone::Neutral, false)
}

fn collect_step_template_vars(step: &FlowStep, linked: Option<&Request>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut scan = |text: &str| {
        extract_template_vars(text, &mut seen, &mut out);
    };

    if let Some(u) = &step.url_override {
        scan(u);
    } else if let Some(req) = linked {
        scan(&req.url);
    }
    if let Some(body) = &step.body_override {
        scan(body);
    }
    for kv in &step.headers_override {
        if kv.enabled {
            scan(&kv.value);
        }
    }
    for kv in &step.query_override {
        if kv.enabled {
            scan(&kv.value);
        }
    }
    if let Some(req) = linked {
        for kv in &req.headers {
            if kv.enabled {
                scan(&kv.value);
            }
        }
        for kv in &req.query_params {
            if kv.enabled {
                scan(&kv.value);
            }
        }
    }
    scan(&step.condition_expression);
    for br in &step.branches {
        scan(&br.expression);
    }
    out
}

fn extract_template_vars(text: &str, seen: &mut HashSet<String>, out: &mut Vec<String>) {
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let raw = after[..end].trim();
        if !raw.is_empty()
            && !raw.starts_with('$')
            && !raw.contains(char::is_whitespace)
            && seen.insert(raw.to_owned())
        {
            out.push(raw.to_owned());
        }
        rest = &after[end + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spanbay_core::model::WorkspaceId;
    use spanbay_core::model::items::{
        FlowBranch, FlowExtraction, FlowExtractionSource, FlowLaneDef, FlowOptions,
    };

    fn mk_step(id: &str, name: &str, pos: i64, url: &str) -> FlowStep {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": name,
            "delay_ms": 0,
            "continue_on_error": false,
            "position": pos,
            "url_override": url,
        }))
        .expect("step")
    }

    #[test]
    fn eight_step_ecommerce_flow_defaults_to_serpentine_and_derives_variable_dataflow_edges() {
        let mut s_login = mk_step("login", "登录", 1, "https://api.shop/auth/login");
        s_login.extractions.push(FlowExtraction {
            variable_name: "token".into(),
            source: FlowExtractionSource::BodyJsonPath,
            expression: "$.token".into(),
            secret: Some(true),
        });
        let mut s_order = mk_step("order", "下单", 3, "https://api.shop/orders");
        s_order
            .headers_override
            .push(spanbay_core::model::KeyValue {
                key: "Authorization".into(),
                value: "Bearer {{token}}".into(),
                enabled: true,
                description: None,
            });
        s_order.extractions.push(FlowExtraction {
            variable_name: "order_id".into(),
            source: FlowExtractionSource::BodyJsonPath,
            expression: "$.id".into(),
            secret: Some(false),
        });
        let s_pay = mk_step("pay", "支付", 4, "https://api.shop/orders/{{order_id}}/pay");

        let flow = Flow {
            id: "f_shop".into(),
            workspace_id: WorkspaceId::new("ws_1").expect("ws"),
            name: "电商下单回归".into(),
            description: String::new(),
            steps: vec![
                mk_step("reg", "注册", 0, "https://api.shop/register"),
                s_login,
                mk_step("browse", "浏览商品", 2, "https://api.shop/products"),
                s_order,
                s_pay,
                mk_step(
                    "done",
                    "订单完成",
                    5,
                    "https://api.shop/orders/{{order_id}}",
                ),
                mk_step("ship", "发货完成", 6, "https://api.shop/ship/{{order_id}}"),
                mk_step(
                    "finish",
                    "交易完成",
                    7,
                    "https://api.shop/finish/{{order_id}}",
                ),
            ],
            updated_at_ms: 0,
            options: FlowOptions::default(),
        };

        let layout = build_flow_graph_layout(&flow, &[], FlowOverlaySource::DesignOnly, false);
        assert!(layout.geometry.policy.direction.is_serpentine());
        assert_eq!(layout.nodes.len(), 8);
        assert!(
            layout
                .geometry
                .edges
                .iter()
                .any(|e| e.layer == ProcessEdgeLayer::DataFlow
                    && e.from_node == "login"
                    && e.to_node == "order"),
            "must infer {{token}} dataflow edge from login -> order"
        );
        assert!(
            layout
                .geometry
                .edges
                .iter()
                .any(|e| e.layer == ProcessEdgeLayer::DataFlow
                    && e.from_node == "order"
                    && e.to_node == "pay"),
            "must infer {{order_id}} dataflow edge from order -> pay"
        );
    }

    #[test]
    fn dag_condition_branches_and_swimlanes_produce_multi_port_edges() {
        let mut cond = mk_step("cond", "VIP 判断", 1, "");
        cond.step_kind = FlowStepKind::Condition;
        cond.depends_on = vec!["login".into()];
        cond.lane = Some("order".into());
        cond.branches = vec![
            FlowBranch {
                id: "b1".into(),
                label: "VIP".into(),
                expression: "{{vip}} == true".into(),
                target_step_ids: vec!["fast".into()],
            },
            FlowBranch {
                id: "b2".into(),
                label: "普通".into(),
                expression: String::new(),
                target_step_ids: vec!["std".into()],
            },
        ];

        let mut login = mk_step("login", "登录", 0, "https://api.shop/login");
        login.lane = Some("auth".into());
        let mut fast = mk_step("fast", "极速支付", 2, "https://api.shop/fast");
        fast.lane = Some("pay".into());
        let mut std_step = mk_step("std", "普通支付", 3, "https://api.shop/std");
        std_step.lane = Some("pay".into());

        let mut opts = FlowOptions::default();
        opts.layout.swimlanes = true;
        opts.lanes = vec![
            FlowLaneDef {
                id: "auth".into(),
                title: "用户中心".into(),
                accent_index: 0,
            },
            FlowLaneDef {
                id: "order".into(),
                title: "订单交易".into(),
                accent_index: 1,
            },
            FlowLaneDef {
                id: "pay".into(),
                title: "支付网关".into(),
                accent_index: 2,
            },
        ];

        let flow = Flow {
            id: "f_branch".into(),
            workspace_id: WorkspaceId::new("ws_1").expect("ws"),
            name: "分支测试".into(),
            description: String::new(),
            steps: vec![login, cond, fast, std_step],
            updated_at_ms: 0,
            options: opts,
        };

        let layout = build_flow_graph_layout(&flow, &[], FlowOverlaySource::DesignOnly, false);
        assert!(layout.has_dag);
        assert_eq!(layout.geometry.lanes.len(), 3);
        let cond_node = layout.node("cond").expect("cond");
        let out_ports = cond_node
            .ports
            .iter()
            .filter(|p| p.direction == super::super::process_graph::ProcessPortDirection::Output)
            .count();
        assert_eq!(out_ports, 2);
        assert!(
            layout.geometry.edges.iter().any(
                |e| e.kind == ProcessEdgeKind::Conditional && e.label.as_deref() == Some("VIP")
            )
        );
    }
}
