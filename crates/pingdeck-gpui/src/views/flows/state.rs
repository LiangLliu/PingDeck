//! State container, view modes, modals, and input synchronization for the unified ProcessView
//! `FlowsView` workbench (`NavModule::Flows`).

use std::cell::Cell;
use std::collections::HashMap;

use gpui_kit::{Context, Entity, Subscription, Task, Window};
use pingdeck_desktop::core::app::{EnvironmentDto, FlowListEntryDto};
use pingdeck_desktop::core::model::Request;
use pingdeck_desktop::core::model::items::{
    Flow, FlowExtractionSource, FlowRunResult, FlowRunSummary, FlowStep, FlowStepResult,
    FlowStepSummary,
};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::state::{AlignmentGuide, FlowOverlaySource, FlowViewMode};

use crate::ui::{EditorState, InputState, Tone};

/// Inspector tabs for the selected flow step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowStepTab {
    Request,
    Extractions,
    Assertions,
    Control,
    Topology,
    Output,
}

impl FlowStepTab {
    pub const ALL: [Self; 6] = [
        Self::Request,
        Self::Extractions,
        Self::Assertions,
        Self::Control,
        Self::Topology,
        Self::Output,
    ];

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Request => 0,
            Self::Extractions => 1,
            Self::Assertions => 2,
            Self::Control => 3,
            Self::Topology => 4,
            Self::Output => 5,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Extractions,
            2 => Self::Assertions,
            3 => Self::Control,
            4 => Self::Topology,
            5 => Self::Output,
            _ => Self::Request,
        }
    }

    #[must_use]
    pub const fn label(self, lang: Language) -> &'static str {
        let f = crate::ui::Tr(lang).flows();
        match self {
            Self::Request => f.step_tab_request(),
            Self::Extractions => f.step_tab_extractions(),
            Self::Assertions => f.step_tab_assertions(),
            Self::Control => f.step_tab_control(),
            Self::Topology => f.step_tab_topology(),
            Self::Output => f.step_tab_output(),
        }
    }
}

/// Active mouse-drag session while repositioning a step card (or moving it across swimlanes).
#[derive(Debug, Clone)]
pub struct FlowDragState {
    pub step_id: String,
    pub start_mouse: (f32, f32),
    pub start_pos: (f32, f32),
    pub size: (f32, f32),
    pub scale: f32,
    pub dragging: bool,
    pub hover_lane_id: Option<String>,
}

/// Modal dialogs owned by `FlowsView`.
#[derive(Debug, Clone)]
pub enum FlowModal {
    CreateFlow,
    DeleteFlow {
        id: String,
        name: String,
    },
    ConfirmRun {
        reason: String,
        max_steps: Option<usize>,
        only_step: Option<usize>,
    },
    Options,
    Variables,
    AddSavedStep {
        insert_between: Option<(String, String)>,
    },
    PickStepRequest,
    InsertOnEdge {
        from_step_id: String,
        to_step_id: String,
    },
}

/// Cross-module handoff signals emitted by `FlowsView` and polled by `AppShell`.
#[derive(Debug, Clone)]
pub enum FlowsHandoff {
    OpenRequest(String),
}

/// Full stateful Flow orchestration workbench powered by the unified ProcessView Engine.
pub struct FlowsView {
    pub workspace_id: Option<String>,
    pub lang: Language,
    pub entries: Vec<FlowListEntryDto>,
    pub requests: Vec<Request>,
    pub environments: Vec<EnvironmentDto>,
    pub selected_flow_id: Option<String>,
    pub draft: Option<Flow>,
    pub dirty: bool,
    pub selected_step_id: Option<String>,
    pub selected_env_id: Option<String>,

    // Unified ProcessView state
    pub view_mode: FlowViewMode,
    pub step_tab: FlowStepTab,
    pub inspector_open: bool,
    pub graph_zoom: f32,
    pub drag_state: Option<FlowDragState>,
    pub alignment_guides: Vec<AlignmentGuide>,
    pub drag_suppress_click: bool,

    // Execution & history playback state
    pub running: bool,
    pub cancel_token: Option<String>,
    pub live_total: usize,
    pub live_steps: HashMap<String, FlowStepSummary>,
    pub last_result: Option<FlowRunResult>,
    pub past_runs: Vec<FlowRunSummary>,
    /// `None` = live / latest result (or design-only); `Some(run_id)` = replaying a past run.
    pub active_history_run_id: Option<String>,

    // Inline builders
    pub new_ext_source: FlowExtractionSource,
    pub new_ext_secret: bool,
    pub new_assert_kind_idx: usize,

    // Shared hydrated inputs
    pub search_input: Entity<InputState>,
    pub flow_name_input: Entity<InputState>,
    pub flow_desc_input: Entity<InputState>,
    pub step_name_input: Entity<InputState>,
    pub step_url_input: Entity<InputState>,
    pub step_body_editor: Entity<EditorState>,
    pub step_delay_input: Entity<InputState>,
    pub step_timeout_input: Entity<InputState>,
    pub step_cond_input: Entity<InputState>,
    pub step_retries_input: Entity<InputState>,
    pub step_retry_wait_input: Entity<InputState>,
    pub kv_key_input: Entity<InputState>,
    pub kv_val_input: Entity<InputState>,
    pub ext_var_input: Entity<InputState>,
    pub ext_expr_input: Entity<InputState>,
    pub assert_val_input: Entity<InputState>,
    pub assert_path_input: Entity<InputState>,
    pub seed_key_input: Entity<InputState>,
    pub seed_val_input: Entity<InputState>,
    pub lane_name_input: Entity<InputState>,
    pub branch_label_input: Entity<InputState>,
    pub branch_expr_input: Entity<InputState>,
    pub req_picker_search: Entity<InputState>,
    pub new_flow_name_input: Entity<InputState>,

    // Modals, feedback, sync
    pub active_modal: Option<FlowModal>,
    pub status_msg: Option<(Tone, String)>,
    pub pending_sync: bool,
    pub syncing: bool,
    pub pending_handoff: Cell<Option<FlowsHandoff>>,
    pub(super) _subscriptions: Vec<Subscription>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl FlowsView {
    #[must_use]
    pub fn selected_step(&self) -> Option<&FlowStep> {
        let flow = self.draft.as_ref()?;
        let sid = self.selected_step_id.as_deref()?;
        flow.steps.iter().find(|s| s.id == sid)
    }

    pub fn selected_step_mut(&mut self) -> Option<&mut FlowStep> {
        let sid = self.selected_step_id.clone()?;
        self.draft.as_mut()?.steps.iter_mut().find(|s| s.id == sid)
    }

    /// Resolves which execution data source is currently projected onto the canvas/timeline.
    #[must_use]
    pub fn active_overlay_source(&self) -> FlowOverlaySource<'_> {
        if self.running {
            return FlowOverlaySource::Live {
                steps: &self.live_steps,
            };
        }
        if let Some(hid) = self.active_history_run_id.as_deref() {
            if let Some(res) = self.last_result.as_ref().filter(|r| r.run_id == hid) {
                return FlowOverlaySource::Result(res);
            }
            if let Some(sum) = self.past_runs.iter().find(|r| r.run_id == hid) {
                return FlowOverlaySource::PastSummary(sum);
            }
        }
        if let Some(res) = &self.last_result {
            return FlowOverlaySource::Result(res);
        }
        FlowOverlaySource::DesignOnly
    }

    #[must_use]
    pub fn active_past_summary(&self) -> Option<&FlowRunSummary> {
        let hid = self.active_history_run_id.as_deref()?;
        self.past_runs.iter().find(|r| r.run_id == hid)
    }

    #[must_use]
    pub fn step_result_of(&self, step_id: &str) -> Option<&FlowStepResult> {
        if let Some(hid) = self.active_history_run_id.as_deref() {
            let res = self.last_result.as_ref().filter(|r| r.run_id == hid)?;
            return res.steps.iter().find(|s| s.step_id == step_id);
        }
        self.last_result
            .as_ref()?
            .steps
            .iter()
            .find(|s| s.step_id == step_id)
    }

    #[must_use]
    pub fn step_summary_of(&self, step_id: &str) -> Option<&FlowStepSummary> {
        if self.running {
            return self.live_steps.get(step_id);
        }
        self.active_past_summary()?
            .steps
            .iter()
            .find(|s| s.step_id == step_id)
    }

    #[must_use]
    pub fn active_variables_count(&self) -> usize {
        if let Some(hid) = self.active_history_run_id.as_deref() {
            if let Some(res) = self.last_result.as_ref().filter(|r| r.run_id == hid) {
                return res.variables.len();
            }
            if let Some(sum) = self.past_runs.iter().find(|r| r.run_id == hid) {
                return sum.variables.as_ref().map_or(0, Vec::len);
            }
        }
        self.last_result.as_ref().map_or(0, |r| r.variables.len())
    }

    #[must_use]
    pub fn request_of(&self, req_id: &str) -> Option<&Request> {
        if req_id.trim().is_empty() {
            return None;
        }
        self.requests.iter().find(|r| r.id.as_str() == req_id)
    }

    pub fn normalize_step_selection(&mut self) {
        let Some(flow) = &self.draft else {
            self.selected_step_id = None;
            return;
        };
        if let Some(id) = self.selected_step_id.as_deref()
            && !flow.steps.iter().any(|s| s.id == id)
        {
            self.selected_step_id = flow.steps.first().map(|s| s.id.clone());
        }
    }

    pub fn select_step(&mut self, step_id: Option<String>, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        match step_id {
            Some(id) => {
                self.selected_step_id = Some(id);
                self.inspector_open = true;
                self.pending_sync = true;
            }
            None => {
                self.selected_step_id = None;
                self.inspector_open = false;
            }
        }
        cx.notify();
    }

    pub fn snapshot_inputs(&mut self, cx: &Context<Self>) {
        if self.syncing {
            return;
        }
        let flow_name = self.flow_name_input.read(cx).value().to_string();
        let flow_desc = self.flow_desc_input.read(cx).value().to_string();
        let step_name = self.step_name_input.read(cx).value().to_string();
        let step_url = self.step_url_input.read(cx).value().to_string();
        let step_body = self.step_body_editor.read(cx).value().to_string();
        let step_delay = self.step_delay_input.read(cx).value().to_string();
        let step_timeout = self.step_timeout_input.read(cx).value().to_string();
        let step_cond = self.step_cond_input.read(cx).value().to_string();
        let step_retries = self.step_retries_input.read(cx).value().to_string();
        let step_retry_wait = self.step_retry_wait_input.read(cx).value().to_string();
        let sel_step = self.selected_step_id.clone();

        let Some(flow) = self.draft.as_mut() else {
            return;
        };
        if !flow_name.trim().is_empty() {
            flow.name = flow_name;
        }
        flow.description = flow_desc;

        if let Some(sid) = sel_step
            && let Some(step) = flow.steps.iter_mut().find(|s| s.id == sid)
        {
            if !step_name.trim().is_empty() {
                step.name = step_name;
            }
            step.url_override = (!step_url.trim().is_empty()).then_some(step_url);
            step.body_override = (!step_body.is_empty()).then_some(step_body);
            if let Ok(d) = step_delay.trim().parse::<u64>() {
                step.delay_ms = d;
            }
            step.timeout_ms = step_timeout.trim().parse::<u64>().ok().filter(|v| *v > 0);
            step.condition_expression = step_cond;
            if let Ok(r) = step_retries.trim().parse::<u8>() {
                step.retries = r.min(5);
            }
            if let Ok(w) = step_retry_wait.trim().parse::<u64>() {
                step.retry_wait_ms = w.min(60_000);
            }
        }
    }

    pub fn sync_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_sync = false;
        self.normalize_step_selection();
        let Some(flow) = self.draft.clone() else {
            return;
        };
        let step = self
            .selected_step_id
            .as_deref()
            .and_then(|id| flow.steps.iter().find(|s| s.id == id))
            .cloned();

        self.syncing = true;
        self.flow_name_input
            .update(cx, |s, cx| s.set_value(flow.name, window, cx));
        self.flow_desc_input
            .update(cx, |s, cx| s.set_value(flow.description, window, cx));
        if let Some(st) = step {
            self.step_name_input
                .update(cx, |s, cx| s.set_value(st.name, window, cx));
            self.step_url_input.update(cx, |s, cx| {
                s.set_value(st.url_override.unwrap_or_default(), window, cx)
            });
            self.step_body_editor.update(cx, |s, cx| {
                s.set_value(st.body_override.unwrap_or_default(), window, cx)
            });
            self.step_delay_input
                .update(cx, |s, cx| s.set_value(st.delay_ms.to_string(), window, cx));
            self.step_timeout_input.update(cx, |s, cx| {
                s.set_value(
                    st.timeout_ms.map_or_else(String::new, |v| v.to_string()),
                    window,
                    cx,
                )
            });
            self.step_cond_input
                .update(cx, |s, cx| s.set_value(st.condition_expression, window, cx));
            self.step_retries_input
                .update(cx, |s, cx| s.set_value(st.retries.to_string(), window, cx));
            self.step_retry_wait_input.update(cx, |s, cx| {
                s.set_value(st.retry_wait_ms.to_string(), window, cx)
            });
        }
        self.syncing = false;
    }
}
