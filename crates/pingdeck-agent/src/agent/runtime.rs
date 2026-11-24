//! Process-wide state of the agent harness: MCP connections, the run registry, pending approvals,
//! background processes, read hashes, the attention hub and the skill folder.
//!
//! Single responsibility: own the shared pieces and the few operations that span them (the
//! attention snapshot, the sandbox tier, the approval gate of one run). The pieces themselves
//! live in `registry`, `approvals`, `attention` and `tools`; the use-cases in `app::agent_service`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use tokio::sync::watch;

use pingdeck_model::events::EventHub;

use super::approvals::Approvals;
use super::attention::AgentAttention;
use super::gate::{GateKind, GateWatch, HumanGate};
use super::mcp::McpManager;
use super::registry::{ActiveRun, RunRegistry};
use super::tools::{ProcessRegistry, ReadHashes};

/// Subscribers the attention hub serves (one per window).
const ATTENTION_SUBSCRIBERS: usize = 16;
/// Attention snapshots the hub replays.
const ATTENTION_REPLAY: usize = 64;

/// The MCP tool names the Tools page has shown (what marks an MCP tool "new").
#[derive(Default)]
pub struct SeenTools(Mutex<Option<HashSet<String>>>);

impl SeenTools {
    /// Records `names` as shown and returns those that were not known before. The first call only
    /// records the baseline, so it returns nothing.
    pub fn observe<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> HashSet<String> {
        let mut seen = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let first_read = seen.is_none();
        let known = seen.get_or_insert_with(HashSet::new);
        let mut new = HashSet::new();
        for name in names {
            if known.insert(name.to_owned()) && !first_read {
                new.insert(name.to_owned());
            }
        }
        new
    }
}

/// Serializes journaled file mutations (`write_file`, `edit_file`, `delete_file`, `move_file`)
/// per canonical workspace folder so concurrent `General` sub-agents or parallel top-level runs
/// cannot race past read-hash checks and silently clobber each other or corrupt the undo journal.
#[derive(Default)]
pub struct FolderWriteLocks(Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>);

impl FolderWriteLocks {
    /// Returns the per-workspace lock for `workspace` (canonicalized when possible).
    #[must_use]
    pub fn lock_for(&self, workspace: &Path) -> Arc<Mutex<()>> {
        let key = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf());
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(map.entry(key).or_insert_with(|| Arc::new(Mutex::new(()))))
    }
}

/// Everything the agent use-cases share between calls.
pub struct AgentRuntime {
    /// Live MCP server connections.
    pub mcp: McpManager,
    /// Every active and recently finished run.
    pub runs: RunRegistry,
    /// Every wait for the user: approvals, plans, cost pauses.
    pub gates: Arc<HumanGate>,
    /// Approvals waiting for the user's answer (a facade over `gates`).
    pub approvals: Approvals,
    /// Background shell processes of all chats.
    pub processes: ProcessRegistry,
    /// What the file tools last read, per chat.
    pub read_hashes: ReadHashes,
    /// Per-workspace write serialization locks preventing concurrent file-tool TOCTOU races.
    pub write_locks: FolderWriteLocks,
    /// What needs the user across every chat; one snapshot per change.
    pub attention_hub: EventHub<AgentAttention>,
    last_attention: Mutex<AgentAttention>,
    /// Folder of imported skills; `None` for an in-memory state.
    pub managed_skills_dir: Option<PathBuf>,
    /// Chats in Plan (read-only) mode, mirrored to `agent_sessions.plan_mode`; never inherited
    /// by a fork or a sub-agent.
    plans: Mutex<HashSet<String>>,
    /// The unopened finished runs of earlier sessions were read from the store.
    unseen_seeded: AtomicBool,
    /// MCP tool names already shown on the Tools page.
    pub seen_tools: SeenTools,
    /// The main window has focus (someone is looking); the desktop shell reports changes.
    window_focused: AtomicBool,
}

impl AgentRuntime {
    /// An empty runtime for tests (no connections, no runs, no skills folder).
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self::new(None)
    }

    /// A runtime with no connections and no runs; imported skills live in `managed_skills_dir`.
    pub fn new(managed_skills_dir: Option<PathBuf>) -> Self {
        let gates = Arc::new(HumanGate::default());
        Self {
            mcp: McpManager::new(),
            runs: RunRegistry::new(),
            approvals: Approvals::over(Arc::clone(&gates)),
            gates,
            processes: ProcessRegistry::new(),
            read_hashes: ReadHashes::new(),
            write_locks: FolderWriteLocks::default(),
            attention_hub: EventHub::new(ATTENTION_SUBSCRIBERS, ATTENTION_REPLAY),
            last_attention: Mutex::new(AgentAttention::default()),
            managed_skills_dir,
            plans: Mutex::new(HashSet::new()),
            unseen_seeded: AtomicBool::new(false),
            seen_tools: SeenTools::default(),
            window_focused: AtomicBool::new(true),
        }
    }

    /// Records whether the main window has focus; open gates read it for the timeout rule.
    pub fn set_window_focused(&self, focused: bool) {
        if self.window_focused.swap(focused, Ordering::Relaxed) != focused {
            self.runs.notify_watch_change();
        }
    }

    /// Whether the main window has focus; `true` until the shell reports otherwise.
    pub fn window_focused(&self) -> bool {
        self.window_focused.load(Ordering::Relaxed)
    }

    /// Cancels an active run for `session_id`, clears runtime state, kills session background
    /// processes and soft-deletes the session row.
    ///
    /// # Errors
    /// [`pingdeck_store::storage::StorageError`] when updating the session row fails.
    pub fn cancel_and_soft_delete(
        &self,
        store: &pingdeck_store::storage::WorkspaceStore,
        session_id: &str,
        now_ms: i64,
    ) -> Result<bool, pingdeck_store::storage::StorageError> {
        if let Some(run) = self.runs.active_for_session(session_id) {
            run.cancel.cancel();
        }
        self.read_hashes.forget_session(session_id);
        self.approvals.acknowledge_sandbox(session_id, false);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let (processes, id) = (self.processes.clone(), session_id.to_owned());
            runtime.spawn(async move { processes.kill_session(&id).await });
        }
        store.soft_delete_session(session_id, now_ms)
    }

    /// `true` for the first caller only: it reads the unopened finished runs from the store.
    pub fn claim_unseen_seed(&self) -> bool {
        !self.unseen_seeded.swap(true, Ordering::SeqCst)
    }

    /// Reads unfinished-opened runs from the store into the attention registry, once.
    pub fn seed_unseen_runs(&self, store: &pingdeck_store::storage::WorkspaceStore) {
        if !self.claim_unseen_seed() {
            return;
        }
        if let Ok(runs) = store.list_unseen_agent_runs() {
            self.runs
                .seed_unseen(runs.into_iter().map(|run| (run.run_id, run.session_id)));
            self.publish_attention();
        }
    }

    /// Reads the chats left in Plan mode at the last exit (`agent_sessions.plan_mode`).
    pub fn seed_plans(&self, store: &pingdeck_store::storage::WorkspaceStore) {
        if let Ok(ids) = store.plan_mode_sessions() {
            self.plans
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend(ids);
        }
    }

    /// Pushes the current attention snapshot to the hub when it differs from the last one.
    pub fn publish_attention(&self) {
        let snapshot = AgentAttention::assemble(
            self.approvals.list(),
            self.runs.session_states(),
            self.runs.active_runs().len(),
            self.runs.unseen_runs(),
            self.runs.finished_runs(),
        )
        .with_gates(
            self.gates.count(GateKind::Plan),
            self.gates.count(GateKind::Question),
            self.gates.count(GateKind::Cost),
        );
        let mut last = self
            .last_attention
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *last != snapshot {
            last.clone_from(&snapshot);
            self.attention_hub.emit(snapshot);
        }
    }

    /// Whether `session_id` is in Plan (read-only) mode.
    pub fn plan_active(&self, session_id: &str) -> bool {
        self.plans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(session_id)
    }

    /// Puts `session_id` in or out of Plan mode; returns whether it was in before.
    pub fn set_plan(&self, session_id: &str, on: bool) -> bool {
        let mut plans = self.plans.lock().unwrap_or_else(PoisonError::into_inner);
        if on {
            !plans.insert(session_id.to_owned())
        } else {
            plans.remove(session_id)
        }
    }

    /// The approval gate of `run`: its chat's subscribers, a real timer and its steer queue.
    pub fn gate<'a>(&'a self, run: &'a ActiveRun) -> RunGate<'a> {
        RunGate { runtime: self, run }
    }
}

/// [`GateWatch`] of one run: its chat's subscribers, the window focus, a real timer and its steer
/// queue.
pub struct RunGate<'a> {
    runtime: &'a AgentRuntime,
    run: &'a ActiveRun,
}

impl GateWatch for RunGate<'_> {
    fn watchers(&self) -> usize {
        self.runtime.runs.watchers_of_run(&self.run.run_id)
    }

    fn focused(&self) -> bool {
        self.runtime.window_focused()
    }

    fn changes(&self) -> watch::Receiver<u64> {
        self.runtime.runs.watchers_changed()
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        tokio::time::sleep(duration).boxed()
    }

    fn steered(&self) -> BoxFuture<'_, ()> {
        let parent = self
            .run
            .parent_run_id
            .as_deref()
            .and_then(|id| self.runtime.runs.get(id));
        async move {
            match parent {
                Some(parent) => {
                    tokio::select! {
                        () = self.run.steered() => {}
                        () = parent.steered() => {}
                    }
                }
                None => self.run.steered().await,
            }
        }
        .boxed()
    }
}
