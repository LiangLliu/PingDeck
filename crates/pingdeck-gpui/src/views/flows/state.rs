//! State container, tabs, modals, and input synchronization for `FlowsView`.

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

use crate::ui::{EditorState, InputState, Tone};

/// Inspector tabs for the selected flow step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowStepTab {
    Request,
    Extractions,
    Assertions,
    Control,
    Output,
}

impl FlowStepTab {
    pub const ALL: [Self; 5] = [
        Self::Request,
        Self::Extractions,
        Self::Assertions,
        Self::Control,
        Self::Output,
    ];

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Request => 0,
            Self::Extractions => 1,
            Self::Assertions => 2,
            Self::Control => 3,
            Self::Output => 4,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Extractions,
            2 => Self::Assertions,
            3 => Self::Control,
            4 => Self::Output,
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
            Self::Output => f.step_tab_output(),
        }
    }
}

/// Bottom execution results panel tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowRunTab {
    LiveRun,
    Variables,
    History,
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
    AddSavedStep,
    PickStepRequest,
}

/// Cross-module handoff signals emitted by `FlowsView` and polled by `AppShell`.
#[derive(Debug, Clone)]
pub enum FlowsHandoff {
    OpenRequest(String),
}

/// Full stateful Flow orchestration workbench (`NavModule::Flows`).
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

    // Tabs & layout
    pub step_tab: FlowStepTab,
    pub run_tab: FlowRunTab,
    pub inspector_open: bool,

    // Execution state
    pub running: bool,
    pub cancel_token: Option<String>,
    pub live_total: usize,
    pub live_steps: HashMap<String, FlowStepSummary>,
    pub last_result: Option<FlowRunResult>,
    pub past_runs: Vec<FlowRunSummary>,
    pub selected_past_run_id: Option<String>,

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

    #[must_use]
    pub fn step_result_of(&self, step_id: &str) -> Option<&FlowStepResult> {
        self.last_result
            .as_ref()?
            .steps
            .iter()
            .find(|s| s.step_id == step_id)
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
        if self
            .selected_step_id
            .as_deref()
            .is_none_or(|id| !flow.steps.iter().any(|s| s.id == id))
        {
            self.selected_step_id = flow.steps.first().map(|s| s.id.clone());
        }
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
