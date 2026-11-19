//! The start routine: everything the AI layer does once the window is shown.
//!
//! Single responsibility: one testable entry point, [`llm_start_routine_impl`], that the shell
//! calls after the first paint. It pins the local time zone offset, seeds the model catalog,
//! applies the log retention once, marks interrupted probe runs, starts the gateway when
//! `gateway.enabled_on_start` is set, starts the balance scheduler and schedules the model
//! catalog's automatic sync (a background task, first check 90 s later; never an immediate
//! download). No step is fatal: a failure is a warning in the report and the rest still runs, and a
//! gateway that cannot start keeps its error in its status (`last_error`) for the page that
//! shows it. A test hook can postpone the gateway start; the call still returns at once.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::model_catalog::{AUTO_SYNC_FIRST_DELAY, AUTO_SYNC_RECHECK, spawn_auto_sync};
use super::{current_time_ms, llm_probe_recover_impl, start_balance_scheduler, with_db_blocking};
use crate::app::AppState;
use crate::error::AppError;
use crate::llm::catalog;
use crate::llm::gateway::GatewayError;
use crate::llm::store::{self, PruneLimits};
use crate::logging::{LogLevel, log_line};
use pingdeck_model::model_catalog::MODELS_DEV_URL;

/// How the shell (or a test) drives [`llm_start_routine_impl`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StartOptions {
    /// Start the gateway this long after the routine was called instead of now (test hook: the
    /// shell's first paint must not depend on the gateway). The call returns at once
    /// either way; the report says [`GatewayStart::Scheduled`].
    pub gateway_start_delay: Option<Duration>,
    /// Do not schedule the model catalog's automatic sync (tests never touch the network).
    pub skip_catalog_refresh: bool,
}

/// What became of the gateway autostart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GatewayStart {
    /// `gateway.enabled_on_start` is off.
    Off,
    /// The gateway is running.
    Started,
    /// The gateway did not start; the error is kept in its status too. The shell toasts
    /// "Gateway did not start: <reason>. Open Providers." once.
    Failed {
        /// Why (code, message, port).
        error: GatewayError,
    },
    /// The start was postponed by [`StartOptions::gateway_start_delay`]; its outcome arrives as
    /// a gateway status event.
    Scheduled {
        /// The delay in milliseconds.
        delay_ms: u64,
    },
}

/// What the routine did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartReport {
    /// The gateway autostart.
    pub gateway: GatewayStart,
    /// The bundled model catalog is in place (seeded now or earlier).
    pub catalog_ready: bool,
    /// The delayed automatic catalog sync task was scheduled. It only downloads while the setting
    /// `ai.providers.catalog_auto_refresh` is on and the next sync is due; it reads both live.
    pub catalog_refresh_started: bool,
    /// Probe runs that never ended and were marked Stopped.
    pub recovered_probe_runs: usize,
    /// The balance scheduler task runs.
    pub balance_scheduler_started: bool,
    /// The UTC offset in minutes east that usage days are cut with.
    pub tz_offset_minutes: i16,
    /// Log rows deleted by the first retention pass.
    pub log_rows_pruned: u64,
    /// Captures deleted by the first retention pass.
    pub captures_pruned: u64,
    /// Steps that failed, in words; the rest of the routine still ran.
    pub warnings: Vec<String>,
}

fn warn(report: &mut StartReport, step: &str, error: &AppError) {
    log_line(
        LogLevel::Warn,
        "llm.start",
        &format!("{step}: {}", error.message()),
    );
    report.warnings.push(format!("{step}: {}", error.message()));
}

/// Starts the gateway on the blocking pool; the failure (if any) is in the manager.
async fn autostart(state: &AppState) -> GatewayStart {
    if !state.settings().ai.gateway.enabled_on_start {
        return GatewayStart::Off;
    }
    let state = state.clone();
    match tokio::task::spawn_blocking(move || super::llm_gateway_autostart_impl(&state)).await {
        Ok(None) => GatewayStart::Started,
        Ok(Some(error)) => GatewayStart::Failed { error },
        Err(error) => GatewayStart::Failed {
            error: GatewayError {
                code: "start_failed".to_owned(),
                message: format!("the start task failed: {error}"),
                port: 0,
            },
        },
    }
}

/// Runs the start routine (see the module docs). Never fails: problems are in
/// [`StartReport::warnings`].
pub async fn llm_start_routine_impl(state: &AppState, options: StartOptions) -> StartReport {
    let mut report = StartReport {
        gateway: GatewayStart::Off,
        catalog_ready: false,
        catalog_refresh_started: false,
        recovered_probe_runs: 0,
        balance_scheduler_started: false,
        tz_offset_minutes: 0,
        log_rows_pruned: 0,
        captures_pruned: 0,
        warnings: Vec::new(),
    };

    // The OS reading of the zone spawns `date`: off the async workers, and before anything that
    // cuts a usage day.
    let runtime = std::sync::Arc::clone(state.llm());
    match tokio::task::spawn_blocking(move || runtime.tz_offset_minutes()).await {
        Ok(minutes) => report.tz_offset_minutes = minutes,
        Err(error) => warn(
            &mut report,
            "time zone",
            &AppError::internal(error.to_string()),
        ),
    }

    match with_db_blocking(state, |connection| {
        catalog::seed_bundled_if_empty(connection)?;
        Ok(catalog::status(connection)?.is_some())
    })
    .await
    {
        Ok(ready) => report.catalog_ready = ready,
        Err(error) => warn(&mut report, "model catalog", &error),
    }

    let retention_days = state.settings().ai.providers.usage_retention_days;
    match with_db_blocking(state, move |connection| {
        let pruned = store::prune_log(connection, &PruneLimits::default(), current_time_ms())?;
        store::prune_usage(connection, retention_days, current_time_ms())?;
        Ok(pruned)
    })
    .await
    {
        Ok(pruned) => {
            report.log_rows_pruned = pruned.rows_deleted;
            report.captures_pruned = pruned.captures_deleted;
        }
        Err(error) => warn(&mut report, "log retention", &error),
    }

    let probes = state.clone();
    match tokio::task::spawn_blocking(move || llm_probe_recover_impl(&probes)).await {
        Ok(Ok(runs)) => report.recovered_probe_runs = runs.len(),
        Ok(Err(error)) => warn(&mut report, "interrupted probe runs", &error),
        Err(error) => warn(
            &mut report,
            "interrupted probe runs",
            &AppError::internal(error.to_string()),
        ),
    }

    report.gateway = match options.gateway_start_delay {
        None => autostart(state).await,
        Some(delay) if state.settings().ai.gateway.enabled_on_start => {
            let later = state.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                // A failure is in the gateway's status, which announces itself.
                let _ = autostart(&later).await;
            });
            GatewayStart::Scheduled {
                delay_ms: u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            }
        }
        Some(_) => GatewayStart::Off,
    };

    match start_balance_scheduler(state) {
        Ok(()) => report.balance_scheduler_started = true,
        Err(error) => warn(&mut report, "balance refresh", &error),
    }

    if !options.skip_catalog_refresh {
        spawn_auto_sync(
            state,
            MODELS_DEV_URL,
            AUTO_SYNC_FIRST_DELAY,
            AUTO_SYNC_RECHECK,
        );
        report.catalog_refresh_started = true;
    }
    report
}
