//! UI-agnostic macOS menu-bar tray status snapshot and localized labels.

use pingdeck_core::{
    app::{AppState, RunningWork, app_running_work_impl, settings_get_impl},
    model::settings::Language,
};

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
    let zh = language == Language::ZhCn;
    let mut parts = Vec::new();
    match (gateway_port, zh) {
        (Some(port), true) => parts.push(format!("网关运行中（端口 {port}）")),
        (Some(port), false) => parts.push(format!("Gateway on port {port}")),
        (None, true) => parts.push("网关未运行".to_owned()),
        (None, false) => parts.push("Gateway stopped".to_owned()),
    }
    let counts = [
        (
            work.mock_servers,
            "个 Mock 服务",
            "Mock server",
            "Mock servers",
        ),
        (
            work.mcp_servers,
            "个 MCP 服务器",
            "MCP server",
            "MCP servers",
        ),
        (
            work.agent_runs + work.subagents,
            "个智能体运行",
            "agent run",
            "agent runs",
        ),
        (
            work.flow_runs + work.collection_runs + work.bench_runs,
            "个运行中的任务",
            "run",
            "runs",
        ),
    ];
    for (count, zh_word, one, many) in counts {
        if count == 0 {
            continue;
        }
        parts.push(match (zh, count) {
            (true, n) => format!("{n}{zh_word}"),
            (false, 1) => format!("1 {one}"),
            (false, n) => format!("{n} {many}"),
        });
    }
    parts.join(if zh { "，" } else { " · " })
}

/// Localized `(show, quit)` tray labels.
#[must_use]
pub fn labels(language: Language) -> (&'static str, &'static str) {
    match language {
        Language::ZhCn => ("显示 PingDeck", "退出 PingDeck"),
        Language::En => ("Show PingDeck", "Quit PingDeck"),
    }
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
