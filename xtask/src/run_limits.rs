//! Verifies that `docs/product/PRD.md`'s `#### Run limits (normative)` table mirrors
//! `crates/spanbay-agent/src/agent/limits.rs` constants and keeps its normative anchor sentence.

use std::collections::HashMap;

use regex::Regex;

use crate::docs::{FailFn, RepoView};

pub const PRD: &str = "docs/product/PRD.md";
pub const LIMITS_RS: &str = "crates/spanbay-agent/src/agent/limits.rs";
/// First sentence of the paragraph that must immediately follow the table.
pub const PRD_ANCHOR: &str =
    "A run that is waiting for an approval does not hold one of the concurrent slots.";
const HEADING: &str = "#### Run limits (normative)";

struct LimitRow {
    key: &'static str,
    value_re: &'static str,
    consts: &'static [&'static str],
    range: Option<(&'static str, &'static str)>,
}

const RUN_LIMITS: &[LimitRow] = &[
    LimitRow {
        key: "Concurrent top-level runs",
        value_re: r"^(\d+)$",
        consts: &["MAX_CONCURRENT_RUNS"],
        range: None,
    },
    LimitRow {
        key: "Sub-agents (`task` calls) per run",
        value_re: r"^(\d+)$",
        consts: &["MAX_SUBAGENTS"],
        range: None,
    },
    LimitRow {
        key: "Steps per run (each sub-agent has its own)",
        value_re: r"^(\d+)$",
        consts: &["MAX_STEPS"],
        range: None,
    },
    LimitRow {
        key: "Approval wait, foreground chat",
        value_re: r"^none$",
        consts: &[],
        range: None,
    },
    LimitRow {
        key: "Approval wait, run with no attached UI",
        value_re: r"^([\d,]+) s$",
        consts: &["APPROVAL_TIMEOUT_S"],
        range: None,
    },
    LimitRow {
        key: "Model stream stall",
        value_re: r"^(\d+) s without a byte$",
        consts: &["STALL_TIMEOUT"],
        range: Some((r"(\d+) s，`STALL_TIMEOUT`", "WAKE_STALL_TIMEOUT")),
    },
    LimitRow {
        key: "Auto-compact threshold",
        value_re: r"^(\d+) % of the model's maximum input$",
        consts: &["AUTO_COMPACT_PCT"],
        range: Some((r"保留最近 (\d+) 轮", "KEEP_RECENT_TURNS")),
    },
    LimitRow {
        key: "Replay buffer per run",
        value_re: r"^([\d,]+) events or (\d+) MiB, whichever is smaller$",
        consts: &["REPLAY_EVENTS", "REPLAY_BYTES:MiB"],
        range: None,
    },
    LimitRow {
        key: "Background processes",
        value_re: r"^(\d+) per chat, (\d+) app-wide, (\d+) hours each$",
        consts: &[
            "PROCESSES_PER_CHAT",
            "PROCESSES_TOTAL",
            "PROCESS_LIFETIME:h",
        ],
        range: None,
    },
    LimitRow {
        key: "Queue",
        value_re: r"^(\d+) messages per chat$",
        consts: &["QUEUE_MESSAGES"],
        range: None,
    },
    LimitRow {
        key: "Cost cap per run",
        value_re: r"^off$",
        consts: &[],
        range: None,
    },
    LimitRow {
        key: "One tool result in the model context",
        value_re: r"^min\(256 KiB, 8 % window × 4 B\)$",
        consts: &[],
        range: None,
    },
    LimitRow {
        key: "Transient retries of a model call before content",
        value_re: r"^(\d+)$",
        consts: &["TRANSIENT_RETRIES"],
        range: None,
    },
    LimitRow {
        key: "Identical tool calls before a notice to the model",
        value_re: r"^(\d+)$",
        consts: &["REPEAT_NOTICE_AFTER"],
        range: None,
    },
    LimitRow {
        key: "Full output kept per tool call",
        value_re: r"^(\d+) MiB$",
        consts: &["TOOL_OUTPUT_FILE_BYTES:MiB"],
        range: None,
    },
    LimitRow {
        key: "Full output kept per chat",
        value_re: r"^(\d+) MiB$",
        consts: &["TOOL_OUTPUT_CHAT_BYTES:MiB"],
        range: Some((r"全应用 (\d+) GiB", "TOOL_OUTPUT_TOTAL_BYTES:GiB")),
    },
    LimitRow {
        key: "Unknown context window",
        value_re: r"^([\d,]+) tokens$",
        consts: &["UNKNOWN_WINDOW_TOKENS"],
        range: None,
    },
    LimitRow {
        key: "`send_request` calls per run",
        value_re: r"^(\d+)$",
        consts: &["SENDS_PER_RUN"],
        range: None,
    },
    LimitRow {
        key: "Web fetches per run",
        value_re: r"^(\d+)$",
        consts: &["FETCHES_PER_RUN"],
        range: None,
    },
    LimitRow {
        key: "Tokens per run",
        value_re: r"^off$",
        consts: &[],
        range: None,
    },
    LimitRow {
        key: "Request body returned by `get_request`",
        value_re: r"^(\d+) KiB$",
        consts: &["GET_REQUEST_BODY_BYTES:KiB"],
        range: None,
    },
];

fn parse_spec(spec: &str) -> (&str, u64) {
    match spec.split_once(':') {
        Some((name, "KiB")) => (name, 1 << 10),
        Some((name, "MiB")) => (name, 1 << 20),
        Some((name, "GiB")) => (name, 1 << 30),
        Some((name, "h")) => (name, 3600),
        Some((name, _)) => (name, 1),
        None => (spec, 1),
    }
}

/// Evaluates `pub const NAME: T = <int, a * b, Duration::from_secs(expr)>;` into u64 values.
pub fn rust_consts(text: &str) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    let Ok(re) = Regex::new(r"pub const (\w+): [\w:]+ = ([^;]+);") else {
        return out;
    };
    let Ok(num_expr) = Regex::new(r"^[\d\s*]+$") else {
        return out;
    };
    for caps in re.captures_iter(text) {
        let name = caps[1].to_owned();
        let raw = caps[2].trim();
        let stripped = raw
            .strip_prefix("Duration::from_secs(")
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or(raw)
            .replace('_', "");
        if !num_expr.is_match(&stripped) {
            continue;
        }
        let mut prod: u64 = 1;
        let mut valid = true;
        for part in stripped.split('*') {
            match part.trim().parse::<u64>() {
                Ok(n) => prod = prod.saturating_mul(n),
                Err(_) => {
                    valid = false;
                    break;
                }
            }
        }
        if valid {
            out.insert(name, prod);
        }
    }
    out
}

pub fn check_run_limits(repo: &dyn RepoView, fail: FailFn<'_>) {
    let text = repo.read(PRD).unwrap_or_default();
    let consts = rust_consts(&repo.read(LIMITS_RS).unwrap_or_default());
    let Ok(head_re) = Regex::new(r"^#+ Run limits \(normative\)\s*$") else {
        return;
    };
    let heads: Vec<&str> = text.split('\n').filter(|l| head_re.is_match(l)).collect();
    if heads.len() != 1 || heads[0] != HEADING {
        fail(
            "run-limits",
            PRD,
            format!("`{HEADING}` must appear exactly once"),
            None,
        );
        return;
    }
    let Some(pos) = text.find(HEADING) else {
        return;
    };
    let lines: Vec<&str> = text[pos..].split('\n').collect();
    let Some(start) = lines.iter().position(|l| l.starts_with('|')) else {
        fail(
            "run-limits",
            PRD,
            "run limits table not found".to_owned(),
            None,
        );
        return;
    };
    let len = lines[start..]
        .iter()
        .position(|l| !l.starts_with('|'))
        .unwrap_or(lines.len() - start);
    if len < 2 {
        fail(
            "run-limits",
            PRD,
            "run limits table is incomplete".to_owned(),
            None,
        );
        return;
    }
    let rows = &lines[start + 2..start + len];
    let tail = lines[start + len..].join("\n");
    let tail = tail.trim_start();
    let keys: Vec<&str> = rows
        .iter()
        .map(|r| r.split('|').nth(1).unwrap_or(""))
        .collect();

    for (i, spec) in RUN_LIMITS.iter().enumerate() {
        let want_key = format!(" {} ", spec.key);
        if keys.get(i).copied() != Some(want_key.as_str()) {
            fail(
                "run-limits",
                PRD,
                format!(
                    "row {} key must be \"| {} |\" (one space each side)",
                    i + 1,
                    spec.key
                ),
                None,
            );
            continue;
        }
        let cells: Vec<&str> = rows[i].split('|').map(str::trim).collect();
        let val_cell = cells.get(2).copied().unwrap_or("");
        let Ok(val_re) = Regex::new(spec.value_re) else {
            continue;
        };
        let Some(caps) = val_re.captures(val_cell) else {
            fail(
                "run-limits",
                PRD,
                format!("{}: value \"{val_cell}\" does not parse", spec.key),
                None,
            );
            continue;
        };
        for (j, const_spec) in spec.consts.iter().enumerate() {
            let (name, scale) = parse_spec(const_spec);
            let raw_num = caps.get(j + 1).map_or("", |m| m.as_str());
            let doc_val = raw_num
                .replace(',', "")
                .parse::<u64>()
                .ok()
                .map(|n| n * scale);
            if consts.get(name).copied() != doc_val {
                fail(
                    "run-limits",
                    PRD,
                    format!(
                        "{}: PRD says {raw_num}, {LIMITS_RS} {name} = {:?}",
                        spec.key,
                        consts.get(name)
                    ),
                    None,
                );
            }
        }
        if let Some((range_pat, const_spec)) = spec.range {
            let range_cell = cells.get(3).copied().unwrap_or("");
            let (name, scale) = parse_spec(const_spec);
            let matched = Regex::new(range_pat)
                .ok()
                .and_then(|re| re.captures(range_cell))
                .and_then(|c| c.get(1))
                .and_then(|m| m.as_str().parse::<u64>().ok())
                .map(|n| n * scale);
            if matched != consts.get(name).copied() {
                fail(
                    "run-limits",
                    PRD,
                    format!(
                        "{}: range cell disagrees with {name} = {:?}",
                        spec.key,
                        consts.get(name)
                    ),
                    None,
                );
            }
        }
    }

    if rows.len() != RUN_LIMITS.len() {
        fail(
            "run-limits",
            PRD,
            format!("table has {} rows, want {}", rows.len(), RUN_LIMITS.len()),
            None,
        );
    }
    if !tail.starts_with(PRD_ANCHOR) {
        fail(
            "run-limits",
            PRD,
            "the anchor sentence must follow the table".to_owned(),
            None,
        );
    }
    if !tail.contains("`cargo xtask check-docs`") && !tail.contains("`npm run check:docs`") {
        fail(
            "run-limits",
            PRD,
            "the paragraph after the table must name `cargo xtask check-docs` or `npm run check:docs`".to_owned(),
            None,
        );
    }
}
