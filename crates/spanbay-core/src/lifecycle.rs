//! Cross-domain startup and shutdown orchestration for [`spanbay_app::app::AppState`].

use std::{future::Future, path::Path, sync::Arc, time::UNIX_EPOCH};

use spanbay_app::app::{AppState, current_time_ms};
use spanbay_app_agent::{
    agent_attachment_gc_impl, agent_work_stop_impl, remove_session_temp_dir,
    sweep_session_temp_dirs, warm_login_path,
};
use spanbay_app_llm::{
    GatewayStart, StartOptions, StartReport, llm_start_routine_impl, stop_balance_scheduler,
};
use spanbay_base::logging::{LogLevel, log_line};

/// What [`AppLifecycleExt::bootstrap_ai`] did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BootstrapReport {
    /// The LLM start routine's outcome (gateway, catalog, balance scheduler).
    pub llm: StartReport,
    /// Cleanup steps that failed or notices worth showing, in words; the rest still ran.
    pub warnings: Vec<String>,
}

/// Session scratch folders older than this are removed at start.
const SESSION_TEMP_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// How long a deleted chat stays restorable before startup purges it.
const DELETED_SESSION_GRACE_MS: i64 = 24 * 60 * 60 * 1000;

/// Spilled tool output older than this is removed at startup.
const TOOL_OUTPUT_MAX_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Lifecycle methods connecting `spanbay-app`, `spanbay-app-llm`, `spanbay-app-agent`, and
/// `spanbay-app-sync`.
pub trait AppLifecycleExt {
    /// Post-paint background startup: process ledger recovery, file cleanup, auto-sync wiring,
    /// shell probe, and the LLM start routine.
    fn bootstrap_ai(&self, options: StartOptions) -> impl Future<Output = BootstrapReport> + Send;

    /// Stops agent runs, background processes, mock servers, balance scheduler, gateway, and
    /// flushes queued mock history.
    fn shutdown(&self) -> impl Future<Output = ()> + Send;
}

impl AppLifecycleExt for AppState {
    async fn bootstrap_ai(&self, options: StartOptions) -> BootstrapReport {
        spanbay_app_sync::attach_auto_export(self);
        let mut warnings = Vec::new();
        spanbay_agent::agent::process_ledger::install(Arc::clone(self.store_arc()));
        let reaped = spanbay_agent::agent::process_ledger::reap_leftover_processes(Arc::clone(
            self.store_arc(),
        ))
        .await;
        if reaped.tool_calls > 0 {
            warnings.push(format!(
                "stopped {} leftover process(es) of a previous run",
                reaped.tool_calls
            ));
        }
        let state = self.clone();
        match tokio::task::spawn_blocking(move || bootstrap_ai_blocking(&state)).await {
            Ok(found) => warnings.extend(found),
            Err(error) => warnings.push(format!("start-up cleanup stopped: {error}")),
        }
        warm_login_path().await;
        for warning in &warnings {
            log_line(LogLevel::Warn, "startup", warning);
        }
        let llm = llm_start_routine_impl(self, options).await;
        if let GatewayStart::Failed { error } = &llm.gateway {
            log_line(
                LogLevel::Error,
                "startup",
                &format!("gateway did not start: {}", error.message),
            );
        }
        let report = BootstrapReport { llm, warnings };
        self.startup_notices.finish(startup_notices(&report));
        report
    }

    async fn shutdown(&self) {
        // Runs first (their tool calls are answered), then MCP children and process groups;
        // mock servers and the gateway follow.
        agent_work_stop_impl(self).await;
        self.mocks.stop_all();
        stop_balance_scheduler(self);
        let state = self.clone();
        // The gateway drains in-flight requests for a few seconds: keep that off the runtime.
        let _ = tokio::task::spawn_blocking(move || {
            state.llm.gateway.shutdown(&state.llm, true);
        })
        .await;
        self.mock_history.flush();
    }
}

/// What the page shows after the start routine (`app_startup_report_impl`): a gateway that did
/// not start, then the LLM routine's failed steps, then this routine's own notices (leftover
/// processes stopped, cleanup that failed). Each is already in the diagnostic log.
#[must_use]
pub fn startup_notices(report: &BootstrapReport) -> Vec<String> {
    let gateway = match &report.llm.gateway {
        GatewayStart::Failed { error } => Some(format!("gateway did not start: {}", error.message)),
        _ => None,
    };
    gateway
        .into_iter()
        .chain(report.llm.warnings.iter().cloned())
        .chain(report.warnings.iter().cloned())
        .collect()
}

fn bootstrap_ai_blocking(state: &AppState) -> Vec<String> {
    let now = current_time_ms();
    let mut warnings = Vec::new();
    let dirs = state.data_dirs();
    if let Some(dirs) = dirs {
        sweep_old_files(&dirs.root.join("tool-output"), TOOL_OUTPUT_MAX_AGE_MS, now);
    }
    sweep_session_temp_dirs(SESSION_TEMP_MAX_AGE);
    match state
        .store()
        .purge_deleted_sessions(now - DELETED_SESSION_GRACE_MS)
    {
        Ok(purged) => {
            for session_id in &purged {
                let _ = remove_session_temp_dir(session_id);
                if let Some(dirs) = dirs {
                    spanbay_agent::agent::tool_output_files::remove_session(
                        &dirs.root.join("tool-output"),
                        session_id,
                    );
                }
            }
        }
        Err(error) => warnings.push(format!("deleted chats were not purged: {error}")),
    }
    if dirs.is_some()
        && let Err(error) = agent_attachment_gc_impl(state)
    {
        warnings.push(format!("attachment cleanup failed: {}", error.message()));
    }
    let _ = spanbay_agent::agent::sandbox::sandbox_status();
    warnings
}

fn sweep_old_files(dir: &Path, max_age_ms: i64, now_ms: i64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .ok()
            .filter(|meta| meta.is_file())
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .and_then(|age| i64::try_from(age.as_millis()).ok())
            .is_some_and(|modified_ms| now_ms - modified_ms > max_age_ms);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_hears_the_gateway_first_then_the_failed_steps() {
        let llm = StartReport {
            gateway: GatewayStart::Failed {
                error: spanbay_llm::llm::gateway::GatewayError {
                    code: "port_in_use".into(),
                    message: "port 4000 is in use".into(),
                    port: 4000,
                },
            },
            catalog_ready: true,
            catalog_refresh_started: false,
            recovered_probe_runs: 0,
            balance_scheduler_started: true,
            tz_offset_minutes: 0,
            log_rows_pruned: 0,
            captures_pruned: 0,
            warnings: vec!["catalog seed: disk full".into()],
        };
        let report = BootstrapReport {
            llm,
            warnings: vec!["stopped 2 leftover process(es) of a previous run".into()],
        };
        assert_eq!(
            startup_notices(&report),
            [
                "gateway did not start: port 4000 is in use",
                "catalog seed: disk full",
                "stopped 2 leftover process(es) of a previous run",
            ]
        );
        let quiet = BootstrapReport {
            llm: StartReport {
                gateway: GatewayStart::Off,
                warnings: Vec::new(),
                ..report.llm
            },
            warnings: Vec::new(),
        };
        assert!(startup_notices(&quiet).is_empty());
    }
}
