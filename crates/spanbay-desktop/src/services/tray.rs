//! UI-agnostic macOS menu-bar tray status snapshot and localized labels.

use spanbay_core::{
    app::{AppState, RunningWork, app_running_work_impl, settings_get_impl},
    model::settings::Language,
};

use crate::i18n::tr;

/// Snapshot rendered by the platform tray bar item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraySnapshot {
    pub status: String,
    pub language: Language,
    pub show_label: &'static str,
    pub quit_label: &'static str,
}

/// Formats the tray status line: gateway first, then active mock/MCP/agent/run counts.
#[must_use]
pub fn status_line(work: &RunningWork, gateway_port: Option<u16>, language: Language) -> String {
    let sh = tr(language).shell();
    let mut parts = Vec::new();
    match gateway_port {
        Some(port) => parts.push(sh.tray_gateway_running(port)),
        None => parts.push(sh.tray_gateway_stopped().to_owned()),
    }
    if work.mock_servers > 0 {
        parts.push(sh.tray_mock_servers(work.mock_servers as usize));
    }
    if work.mcp_servers > 0 {
        parts.push(sh.tray_mcp_servers(work.mcp_servers as usize));
    }
    let agent_total = work.agent_runs + work.subagents;
    if agent_total > 0 {
        parts.push(sh.tray_agent_runs(agent_total as usize));
    }
    let runs_total = work.flow_runs + work.collection_runs + work.bench_runs;
    if runs_total > 0 {
        parts.push(sh.tray_runs(runs_total as usize));
    }
    parts.join(sh.tray_sep())
}

/// Localized `(show, quit)` tray labels.
#[must_use]
pub fn labels(language: Language) -> (&'static str, &'static str) {
    let sh = tr(language).shell();
    (sh.tray_show(), sh.tray_quit())
}

/// Reads the current tray status snapshot from `AppState`.
#[must_use]
pub fn snapshot(state: &AppState) -> TraySnapshot {
    let language = settings_get_impl(state).general.language;
    let work = app_running_work_impl(state);
    let port = work
        .gateway_running
        .then(|| state.llm().gateway.status(state.llm()).port);
    let (show_label, quit_label) = labels(language);
    TraySnapshot {
        status: status_line(&work, port, language),
        language,
        show_label,
        quit_label,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bilingual_tray_status_lines() {
        let idle = RunningWork::default();
        assert_eq!(status_line(&idle, None, Language::En), "Gateway stopped");
        assert_eq!(status_line(&idle, None, Language::ZhCn), "网关未运行");

        let busy = RunningWork {
            mock_servers: 2,
            agent_runs: 1,
            subagents: 1,
            gateway_running: true,
            ..RunningWork::default()
        };
        assert_eq!(
            status_line(&busy, Some(4000), Language::En),
            "Gateway on port 4000 · 2 Mock servers · 2 agent runs"
        );
        assert_eq!(
            status_line(&busy, Some(4000), Language::ZhCn),
            "网关运行中（端口 4000），2个 Mock 服务，2个智能体运行"
        );
    }
}
