//! Approval scopes: which narrow rule an approval can leave behind and whether a stored rule may
//! still apply.
//!
//! Single responsibility: the scope table of the PRD. A grant is always
//! narrower than the tool: an argv prefix for `shell`, a folder path for file writes, a host for
//! `fetch_url`, method plus host for `send_request`, a provider for `run_probe`, the tool itself
//! (chat scope only) for a trusted-server MCP tool. [`derive_rule`] makes the pattern at approval
//! time (or says why none can be made); [`recheck`] re-checks a stored rule every time it is about
//! to apply and says why it is ignored (the card then shows "Saved rule not used: <reason>").
//! Stored rules never grant network: that is a per-call choice.

use std::path::Path;

use super::command;
use super::glob_match;
use super::safety::{self, RiskLevel, SafetyContext};
use crate::agent::model::ApprovalScope;
use crate::agent::tools::shell_meta::SHELL;
use crate::agent::tools::{CallInspection, ToolClass, ToolSource, ToolSpec};

/// Shells and interpreters that may never be a rule's whole prefix.
const INTERPRETERS: [&str; 9] = [
    "sh", "bash", "zsh", "dash", "ksh", "node", "ruby", "perl", "env",
];

/// Flags that stop an argv-prefix rule from matching a call that adds them.
const DENIED_FLAGS: [&str; 13] = [
    "--config",
    "-c",
    "--exec",
    "-x",
    "-e",
    "--eval",
    "--script",
    "--output",
    "--runner",
    "--pager",
    "--ext-diff",
    "--upload-pack",
    "--receive-pack",
];

/// First words whose approval is never offered as a stored rule.
const NO_RULE: [&str; 14] = [
    "rm", "rmdir", "mv", "chmod", "chown", "kill", "pkill", "dd", "docker", "kubectl", "sudo",
    "curl", "wget", "ssh",
];

/// Programs whose first words are not enough for a rule: `git *` would also cover `git push`, so
/// a rule needs a named subcommand (`git status *`), not `git -C . status` reduced to `git`.
const SUBCOMMAND_TOOLS: [&str; 6] = ["git", "cargo", "npm", "pnpm", "yarn", "make"];

/// The command and subcommand (`git push`, `npm publish`) of `argv` that is only ever allowed
/// once, wherever the subcommand stands among the options (`git -C . push`).
fn once_only_subcommand(argv: &[String]) -> Option<String> {
    let head = argv.first()?.as_str();
    let sub = argv.iter().skip(1).map(String::as_str).find(|word| {
        matches!(
            (head, *word),
            ("git", "push" | "reset" | "clean") | ("cargo" | "npm" | "pnpm" | "yarn", "publish")
        )
    })?;
    Some(format!("{head} {sub}"))
}

/// A bare shell, interpreter, `xargs` or `python*`.
fn is_interpreter(word: &str) -> bool {
    INTERPRETERS.contains(&word) || word == "xargs" || word.starts_with("python")
}

/// The tool is one of the kinds whose approval is only ever "once".
pub(super) fn once_only(spec: &ToolSpec, inspection: &CallInspection) -> bool {
    inspection.once_only || spec.name == "create_request"
}

/// Whether the tool is a trusted-server MCP tool that changes something (rule: that tool, chat
/// scope only).
fn mcp_tool(spec: &ToolSpec) -> bool {
    matches!(spec.source, ToolSource::Mcp { .. })
}

/// The shell rule pattern for `command` (`cargo clippy -p x` → `cargo clippy *`), or why none can
/// be made.
pub(super) fn shell_rule(command: &str, ctx: &SafetyContext<'_>) -> Result<String, String> {
    let analysis = command::analyze(command);
    if analysis.compound {
        return Err("the command chains several commands".to_owned());
    }
    let risk = safety::assess(command, ctx);
    if risk.level == RiskLevel::High {
        return Err(format!(
            "high risk: {}",
            risk.reason
                .as_deref()
                .unwrap_or("changes things outside the folder")
        ));
    }
    let (argv, _) = analysis
        .top
        .first()
        .ok_or_else(|| "the command is empty".to_owned())?;
    if analysis.explicit_path {
        return Err("a program given by its path is only ever allowed once".to_owned());
    }
    if let Some(what) = once_only_subcommand(argv) {
        return Err(format!("`{what}` is only ever allowed once"));
    }
    let words: Vec<&str> = argv
        .iter()
        .take(3)
        .take_while(|word| !word.starts_with('-') && !word.contains('*') && !word.contains('='))
        .map(String::as_str)
        .collect();
    let head = *words
        .first()
        .ok_or_else(|| "no safe prefix covers this command".to_owned())?;
    if NO_RULE.contains(&head) || head.starts_with("mkfs") {
        return Err(format!("`{head}` is only ever allowed once"));
    }
    if is_interpreter(head) && words.len() == 1 {
        return Err("a shell or interpreter alone is too broad a rule".to_owned());
    }
    if words.len() == 1 && SUBCOMMAND_TOOLS.contains(&head) {
        return Err(format!(
            "`{head}` alone is too broad a rule: name the subcommand (`{head} <subcommand> …`)"
        ));
    }
    Ok(format!("{} *", words.join(" ")))
}

/// The host of `url`, lower case.
fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()?
        .host_str()
        .map(str::to_ascii_lowercase)
}

/// The pattern a `Session` or `WorkspacePattern` answer stores for this call: `Ok(None)` when the
/// scope stores the tool itself (a trusted MCP tool), `Err(reason)` when no narrow rule exists.
pub(super) fn derive_rule(
    spec: &ToolSpec,
    inspection: &CallInspection,
    ctx: &SafetyContext<'_>,
) -> Result<Option<String>, String> {
    let subject = inspection.arg_subject.as_deref().unwrap_or_default();
    if once_only(spec, inspection) {
        return Err("this tool is only ever allowed once".to_owned());
    }
    match spec.name.as_str() {
        SHELL => shell_rule(subject, ctx).map(Some),
        "write_file" | "edit_file" => {
            if safety::is_protected_word(subject) {
                return Err("a protected path is never remembered".to_owned());
            }
            // The folder the file sits in (a file at the top keeps its own name).
            Ok(Some(
                Path::new(subject)
                    .parent()
                    .filter(|dir| !dir.as_os_str().is_empty())
                    .map_or_else(|| subject.to_owned(), |dir| format!("{}/*", dir.display())),
            ))
        }
        "fetch_url" => host_of(subject)
            .map(Some)
            .ok_or_else(|| "the URL has no host".to_owned()),
        "send_request" | "run_probe" => Ok(Some(subject.to_owned())),
        // A move names two paths and a delete removes a file: each is approved on its own.
        "delete_file" | "move_file" => {
            Err("a file delete or move is approved one call at a time".to_owned())
        }
        _ if mcp_tool(spec) => Ok(None),
        _ => Ok(inspection.arg_subject.clone()),
    }
}

/// The scopes the approval menu offers for this call: "once" always, then only what the scope
/// table allows.
pub(super) fn offered(
    spec: &ToolSpec,
    inspection: &CallInspection,
    ctx: &SafetyContext<'_>,
) -> Vec<ApprovalScope> {
    let mut scopes = vec![ApprovalScope::Once];
    if once_only(spec, inspection) {
        return scopes;
    }
    if inspection.read_only || (spec.read_only && !mcp_tool(spec)) {
        scopes.push(ApprovalScope::Tool);
        return scopes;
    }
    if spec.class == ToolClass::Internal || derive_rule(spec, inspection, ctx).is_err() {
        return scopes;
    }
    scopes.push(ApprovalScope::Session);
    // Folder scope: not for a provider probe, nor for an MCP tool.
    if spec.name != "run_probe" && !mcp_tool(spec) {
        scopes.push(ApprovalScope::WorkspacePattern);
    }
    scopes
}

/// Why a stored rule that names `pattern` cannot apply to this call, or `Ok` when it still may.
/// Called only for a rule that matched the call's tool and scope.
pub(super) fn recheck(
    spec: &ToolSpec,
    inspection: &CallInspection,
    pattern: Option<&str>,
    chat_scoped: bool,
    extra: &[String],
    ctx: &SafetyContext<'_>,
) -> Result<(), String> {
    if once_only(spec, inspection) {
        return Err("this tool is only ever allowed once".to_owned());
    }
    let subject = inspection.arg_subject.as_deref().unwrap_or_default();
    let narrow = pattern.filter(|pattern| !pattern.trim_matches('*').trim().is_empty());
    match (spec.name.as_str(), spec.class) {
        (SHELL, _) => {
            let pattern = narrow.ok_or_else(|| "the rule names no command".to_owned())?;
            recheck_shell(pattern, subject, extra, ctx)
        }
        ("write_file" | "edit_file", _) => {
            let pattern = narrow.ok_or_else(|| "the rule names no folder".to_owned())?;
            if pattern.starts_with('/') || pattern.split('/').any(|part| part == "..") {
                return Err("the rule's folder is outside the chat's folder".to_owned());
            }
            if safety::is_protected_word(subject) {
                return Err("the call touches a protected path".to_owned());
            }
            matches_or(
                glob_match(pattern, subject),
                "the file is outside the rule's folder",
            )
        }
        ("delete_file" | "move_file", _) => {
            Err("a file delete or move is approved one call at a time".to_owned())
        }
        ("fetch_url", _) => {
            let pattern = narrow.ok_or_else(|| "the rule names no host".to_owned())?;
            let same = if pattern.contains("://") || pattern.contains('*') {
                glob_match(pattern, subject)
            } else {
                host_of(subject).is_some_and(|host| host.eq_ignore_ascii_case(pattern))
            };
            matches_or(same, "the host differs from the rule's")
        }
        ("send_request" | "run_probe", _) => {
            let pattern = narrow.ok_or_else(|| "the rule names no target".to_owned())?;
            if spec.name == "run_probe" && !chat_scoped {
                return Err("a provider rule applies in one chat only".to_owned());
            }
            matches_or(
                glob_match(pattern, subject),
                "the target differs from the rule's",
            )
        }
        _ if mcp_tool(spec) && !spec.read_only => {
            if chat_scoped {
                Ok(())
            } else {
                Err("an MCP tool rule applies in one chat only".to_owned())
            }
        }
        _ => Ok(()),
    }
}

fn matches_or(matches: bool, reason: &str) -> Result<(), String> {
    if matches {
        Ok(())
    } else {
        Err(reason.to_owned())
    }
}

/// The flag of `argv` (after the program name) that stops a prefix rule from matching, if any;
/// `--manifest-path` only when it points outside the folder.
fn denied_flag(argv: &[String], ctx: &SafetyContext<'_>) -> Option<String> {
    let mut args = argv.iter().skip(1);
    while let Some(arg) = args.next() {
        let (name, value) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(name, value)| (name, Some(value)));
        if DENIED_FLAGS.contains(&name) {
            return Some(name.to_owned());
        }
        if name == "--manifest-path" {
            let value = value
                .or_else(|| args.next().map(String::as_str))
                .unwrap_or_default();
            let inside = ctx.folder.is_some_and(|folder| {
                !value.is_empty()
                    && (value.starts_with(folder.to_string_lossy().as_ref())
                        || (!value.starts_with('/') && !value.contains("..")))
            });
            if !inside {
                return Some(name.to_owned());
            }
        }
    }
    None
}

/// Re-checks a stored shell rule against the command it is about to cover.
fn recheck_shell(
    pattern: &str,
    command_line: &str,
    extra: &[String],
    ctx: &SafetyContext<'_>,
) -> Result<(), String> {
    let words: Vec<&str> = pattern
        .split_whitespace()
        .filter(|word| *word != "*")
        .collect();
    if words.first().is_some_and(|word| is_interpreter(word)) && words.len() == 1 {
        return Err("a shell or interpreter alone is too broad a rule".to_owned());
    }
    if words.len() == 1
        && words
            .first()
            .is_some_and(|word| SUBCOMMAND_TOOLS.contains(word))
    {
        return Err("the rule names no subcommand".to_owned());
    }
    let analysis = command::analyze(command_line);
    if analysis.compound {
        return Err("the command chains several commands".to_owned());
    }
    if !command::rule_matches(pattern, command_line, true) {
        return Err("the command differs from the rule's".to_owned());
    }
    if safety::on_safety_list(extra, command_line, ctx) {
        return Err("a part of the command is on the safety rules".to_owned());
    }
    let risk = safety::assess(command_line, ctx);
    if risk.level == RiskLevel::High {
        return Err(format!(
            "the command is high risk ({})",
            risk.reason
                .as_deref()
                .unwrap_or("it changes things outside the folder")
        ));
    }
    if analysis
        .top
        .first()
        .is_some_and(|(argv, _)| argv.iter().any(|word| safety::is_protected_word(word)))
    {
        return Err("the command touches a protected path".to_owned());
    }
    if let Some((argv, _)) = analysis.top.first()
        && let Some(flag) = denied_flag(argv, ctx)
    {
        return Err(format!("the command adds `{flag}`"));
    }
    if let Some((argv, _)) = analysis.top.first()
        && let Some(what) = once_only_subcommand(argv)
    {
        return Err(format!("`{what}` is only ever allowed once"));
    }
    Ok(())
}

/// Whether a stored shell rule's pattern covers `command_line` as an argv prefix, ignoring every
/// re-check (used to tell "the rule does not apply" from "the rule is not used").
pub(super) fn shell_pattern_covers(pattern: &str, command_line: &str) -> bool {
    command::rule_matches(pattern, command_line, false)
}
