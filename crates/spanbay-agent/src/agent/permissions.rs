//! Decides whether one tool call runs, asks the user first, or is refused.
//!
//! Single responsibility: the one permission decision of the PRD ("The decision order")
//! and what the approval card shows about it. [`evaluate_permission`] takes the call and its
//! context and returns [`Outcome::Allow`], [`Outcome::Ask`] or [`Outcome::Block`] with the deciding
//! [`DecisionLayer`] and a plain-language reason. The first layer that matches decides:
//!
//! 1. hard blocks: a command on the safety rules ([`safety`]), a block a tool reports
//!    (`CallInspection::hard_block`: a path outside the Folder, a write under `.git/`, a secret in
//!    a URL), Plan mode refusing anything that is not read-only;
//! 2. per-tool Block, and a stored Block rule: the user's Block holds on a forced-Ask path too;
//! 3. forced Ask: a protected path, a Production target, an untrusted MCP server, a URL with no
//!    provenance, a networked shell command that contacts a host with no provenance
//!    ([`shell_remote_reason`], set by the host) or any other reason a tool reports
//!    (`CallInspection::always_ask`), `xargs`
//!    feeding a recursive delete unseen targets, `shell` where the sandbox is unavailable and the
//!    chat has not acknowledged "Run without a sandbox", a per-tool "Always ask", a file write
//!    while a background process of the chat is alive: it holds in every mode, Full access
//!    included;
//! 4. stored scope rules (this chat, this folder), narrow and re-checked at match time ([`scope`]);
//! 5. per-tool Allow (ignored in Ask every time, and never offered for `shell`, `run_probe`,
//!    `send_request`, `create_request` or an MCP tool that changes something);
//! 6. the mode default.
//!
//! It does no IO and owns no state; the host loads the rules and settings and acts on the
//! [`PermissionDecision`]. [`approval_card`] derives from the same inputs everything the approval
//! card shows (risk level, tags, operators, why it asks, scopes). Whether a shell call gets network
//! is [`shell_network_allowed`].

mod command;
mod safety;
mod scope;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::Path;

pub use spanbay_model::agent::permissions::{
    ApprovalCard, PermissionAction, PermissionRule, RiskTag,
};

use serde::{Deserialize, Serialize};

use spanbay_model::settings::ToolApproval;

use super::model::ApprovalScope;
use super::sandbox::{SandboxKind, SandboxState, SandboxStatus};
use super::tools::shell_meta::SHELL;
use super::tools::{CallInspection, ToolApprovalPreview, ToolClass, ToolSource, ToolSpec};

pub use safety::{
    BlockedBy, CommandRisk, MAX_EXTRA_RULES, OperatorHit, PINNED_RULES, PinnedRule, RiskLevel,
    SafetyContext, assess as assess_command, blocked_by as blocked_by_safety_rules,
    chain_operators, remote_hosts, validate_extra_rules,
};
/// The rule pattern to offer for approving a shell command for good (`cargo clippy -p x` →
/// `cargo clippy *`): its leading words up to the first flag, at most three. An error naming the
/// reason (the approval menu item is disabled with it) for a compound or High-risk command, one
/// that starts with `rm`, `sudo`, `chmod`, `chown`, `dd`, `kill`, `docker`, `curl`, … or a shell
/// or interpreter alone.
///
/// # Errors
/// The reason no rule can be offered.
pub fn derive_command_rule(command: &str, ctx: &SafetyContext<'_>) -> Result<String, String> {
    scope::shell_rule(command, ctx)
}

/// What a call decided to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Run the call now.
    Allow,
    /// Ask the user first.
    Ask,
    /// Refuse the call; the result says why and no card is shown.
    Block,
}

/// Which layer of the decision order decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionLayer {
    /// A safety rule, a tool-reported hard block or Plan mode.
    HardBlock,
    /// A reason that asks in every mode.
    ForcedAsk,
    /// A per-tool Block (or a stored Block rule).
    ToolBlock,
    /// A stored scope rule.
    StoredRule,
    /// A per-tool Allow.
    ToolAllow,
    /// The mode's default.
    ModeDefault,
}

impl DecisionLayer {
    /// The layers in the order they are checked (Settings draws this order).
    pub const ORDER: [Self; 6] = [
        Self::HardBlock,
        Self::ToolBlock,
        Self::ForcedAsk,
        Self::StoredRule,
        Self::ToolAllow,
        Self::ModeDefault,
    ];

    /// Stable snake_case name (the approval card's `layer`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HardBlock => "hard_block",
            Self::ForcedAsk => "forced_ask",
            Self::ToolBlock => "tool_block",
            Self::StoredRule => "stored_rule",
            Self::ToolAllow => "tool_allow",
            Self::ModeDefault => "mode_default",
        }
    }
}

/// The verdict for one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDecision {
    /// Allow, Ask or Block.
    pub outcome: Outcome,
    /// The layer that decided.
    pub layer: DecisionLayer,
    /// Why, in plain words ("Allowed by Approve reads", "Asks because: …").
    pub reason: String,
    /// A stored rule matched the call but could not be used: the card shows "Saved rule not used:
    /// <this>".
    pub rule_not_used: Option<String>,
    /// The audit's `decided_by` (`hard_block:<rule>`, `block:tool`, `block:rule:<id>`,
    /// `forced:<code>`, `rule:<id>`, `tool_allow`, `mode`).
    pub decided_by: String,
    /// The code of a forced Ask's reason (see [`CallInspection::ask_code`]).
    pub why_code: Option<&'static str>,
}

impl PermissionDecision {
    fn new(outcome: Outcome, layer: DecisionLayer, reason: impl Into<String>) -> Self {
        let decided_by = match layer {
            DecisionLayer::HardBlock => "hard_block:tool",
            DecisionLayer::ToolBlock => "block:tool",
            DecisionLayer::ForcedAsk => "forced:tool",
            DecisionLayer::StoredRule => "rule",
            DecisionLayer::ToolAllow => "tool_allow",
            DecisionLayer::ModeDefault => "mode",
        };
        Self {
            outcome,
            layer,
            reason: reason.into(),
            rule_not_used: None,
            decided_by: decided_by.to_owned(),
            why_code: None,
        }
    }

    fn by(mut self, decided_by: String) -> Self {
        self.decided_by = decided_by;
        self
    }

    /// Run now.
    #[must_use]
    pub fn is_allow(&self) -> bool {
        self.outcome == Outcome::Allow
    }

    /// Ask first.
    #[must_use]
    pub fn is_ask(&self) -> bool {
        self.outcome == Outcome::Ask
    }

    /// Refused.
    #[must_use]
    pub fn is_block(&self) -> bool {
        self.outcome == Outcome::Block
    }
}

/// What the decision needs to know about the world besides the call.
#[derive(Debug, Clone, Copy, Default)]
pub struct PermissionEnv<'a> {
    /// The chat acknowledged "Run without a sandbox" (lasts for the app process only).
    pub sandbox_acknowledged: bool,
    /// A background process of this chat, or another run on the same folder, is alive.
    pub folder_busy: bool,
    /// SpanBay's data directory (a command naming it is blocked).
    pub data_dir: Option<&'a Path>,
    /// The user's home directory.
    pub home: Option<&'a Path>,
    /// `ai.agent.sandbox_auto_run`: in Auto-approve reads a low-risk sandboxed shell
    /// command without network runs without asking.
    pub sandbox_auto_run: bool,
}

/// One tool call to decide on.
#[derive(Debug, Clone, Copy)]
pub struct PermissionQuery<'a> {
    /// The calling session.
    pub session_id: &'a str,
    /// The session's canonical workspace folder, if any.
    pub workspace_dir: Option<&'a Path>,
    /// The called tool's function name.
    pub tool_name: &'a str,
    /// The tool's static description; `class` takes part in the mode default.
    pub spec: &'a ToolSpec,
    /// The tool's classification of this concrete call.
    pub inspection: &'a CallInspection,
    /// The chat's permission mode.
    pub policy: ToolApproval,
    /// The sandbox tier a command can run under right now ([`SandboxKind::None`] when the
    /// self-test found none).
    pub sandbox: SandboxKind,
    /// The rest of the context.
    pub env: PermissionEnv<'a>,
}

impl PermissionQuery<'_> {
    /// Where this call runs, for the path rules.
    fn safety(&self) -> SafetyContext<'_> {
        SafetyContext {
            folder: self.workspace_dir,
            data_dir: self.env.data_dir,
            home: self.env.home,
        }
    }

    /// The command line of a `shell` call.
    fn command(&self) -> Option<&str> {
        (self.tool_name == SHELL)
            .then_some(self.inspection.arg_subject.as_deref())
            .flatten()
    }

    /// `shell` that would run without a sandbox the chat has not acknowledged.
    fn unsandboxed_shell(&self) -> bool {
        self.tool_name == SHELL
            && self.sandbox == SandboxKind::None
            && !self.env.sandbox_acknowledged
    }
}

/// Whether a shell call may use the network: off in Ask and Approve reads, on in Full access, or
/// when the user chose "Allow once with network" on the approval card for this one call.
#[must_use]
pub fn shell_network_allowed(mode: ToolApproval, allow_once_network: bool) -> bool {
    mode == ToolApproval::AutoAll || allow_once_network
}

/// Why a `shell` call that gets network ([`shell_network_allowed`]) asks in every mode: the
/// command contacts a remote host explicitly ([`remote_hosts`]) and a host it names did not come
/// from a trusted source (`seen`: the user's messages and attachments, web search results,
/// approved fetches), or it names none the rules can read (`git push origin`). The provenance
/// rule `fetch_url` follows, applied to the shell; `None` when the command names no remote
/// contact or every host it names was seen.
#[must_use]
pub fn shell_remote_reason(command: &str, seen: &super::fetch::SeenHosts) -> Option<String> {
    let hosts = remote_hosts(command)?;
    if hosts.is_empty() {
        return Some(
            "this command contacts a remote host, and which one is not on the command line"
                .to_owned(),
        );
    }
    let unseen: Vec<&str> = hosts
        .iter()
        .filter(|host| !seen.covers_host(host))
        .map(String::as_str)
        .collect();
    (!unseen.is_empty()).then(|| {
        format!(
            "this command contacts {}, which did not appear in your messages, attachments or this run's web search results",
            unseen.join(", ")
        )
    })
}

/// The first of the user's extra safety patterns (`ai.agent.blocked_commands`) that forbids
/// `command`: the whole line matches the pattern, or one of its simple commands starts with the
/// pattern's words. The pinned rules are checked by [`blocked_by_safety_rules`].
#[must_use]
pub fn blocked_command_pattern<'a>(extra: &'a [String], command: &str) -> Option<&'a str> {
    command::blocked_pattern(extra, command)
}

/// Tools whose per-tool "Allow" is never honoured.
fn allow_never_offered(query: &PermissionQuery<'_>) -> bool {
    matches!(
        query.tool_name,
        SHELL | "run_probe" | "send_request" | "create_request"
    ) || (matches!(query.spec.source, ToolSource::Mcp { .. }) && !query.inspection.read_only)
}

/// Decides one tool call (see the module docs for the order).
///
/// `extra_safety_rules` is the user's add-only list (`ai.agent.blocked_commands`); the pinned
/// rules always apply. `tool_policies` is the per-tool setting by tool key (the function name, or
/// the call's [`CallInspection::policy_key`]).
#[must_use]
pub fn evaluate_permission(
    rules: &[PermissionRule],
    tool_policies: &HashMap<String, PermissionAction>,
    extra_safety_rules: &[String],
    query: &PermissionQuery<'_>,
) -> PermissionDecision {
    // 1. Hard blocks.
    if let Some(command) = query.command()
        && let Some(blocked) = safety::blocked_by(extra_safety_rules, command, &query.safety())
    {
        let what = PINNED_RULES
            .iter()
            .find(|rule| blocked.pinned && rule.id == blocked.rule)
            .map_or(blocked.rule.as_str(), |rule| rule.summary);
        return PermissionDecision::new(
            Outcome::Block,
            DecisionLayer::HardBlock,
            format!("blocked by the safety rule: {what}"),
        )
        .by(format!("hard_block:{}", blocked.rule));
    }
    if let Some(reason) = &query.inspection.hard_block {
        return PermissionDecision::new(Outcome::Block, DecisionLayer::HardBlock, reason.clone());
    }

    // 2. Per-tool Block, then a stored Block rule: a Block beats a forced Ask.
    let tool_policy = tool_policies
        .get(
            query
                .inspection
                .policy_key
                .as_deref()
                .unwrap_or(query.tool_name),
        )
        .copied();
    let matching: Vec<&PermissionRule> = rules
        .iter()
        .filter(|rule| matches_query(rule, query))
        .collect();
    if tool_policy == Some(PermissionAction::Block) {
        return PermissionDecision::new(
            Outcome::Block,
            DecisionLayer::ToolBlock,
            "this tool is blocked in Settings",
        );
    }
    if let Some(rule) = matching
        .iter()
        .find(|rule| rule.action == PermissionAction::Block)
    {
        return PermissionDecision::new(
            Outcome::Block,
            DecisionLayer::ToolBlock,
            "blocked by a permission rule",
        )
        .by(format!("block:rule:{}", rule.id));
    }

    // 3. Forced Ask: nothing below lowers it.
    if let Some((code, reason)) = forced_reason(query, tool_policy, &matching) {
        let mut decision = PermissionDecision::new(Outcome::Ask, DecisionLayer::ForcedAsk, reason)
            .by(format!("forced:{code}"));
        decision.why_code = Some(code);
        return decision;
    }

    // 4. Stored scope rules, narrowed and re-checked now.
    let mut not_used = None;
    if let Some(rule) = stored_allow(&matching, query, extra_safety_rules, &mut not_used) {
        let label = rule.arg_pattern.as_deref().unwrap_or(&rule.tool_pattern);
        let reason = if rule.session_id.is_some() {
            format!("Allowed for this chat (`{label}`)")
        } else {
            format!("Allowed by folder rule `{label}`")
        };
        return PermissionDecision::new(Outcome::Allow, DecisionLayer::StoredRule, reason)
            .by(format!("rule:{}", rule.id));
    }

    // 5. Per-tool Allow: never outranks a stricter chat mode.
    let mut decision = if tool_policy == Some(PermissionAction::Allow)
        && query.policy != ToolApproval::Ask
        && !allow_never_offered(query)
    {
        PermissionDecision::new(
            Outcome::Allow,
            DecisionLayer::ToolAllow,
            "Allowed by your setting",
        )
    } else {
        mode_decision(query)
    };
    decision.rule_not_used = not_used;
    decision
}

/// The forced-Ask reason of the call with its code, if any.
fn forced_reason(
    query: &PermissionQuery<'_>,
    tool_policy: Option<PermissionAction>,
    matching: &[&PermissionRule],
) -> Option<(&'static str, String)> {
    // Internal tools (todo_write, task, shell_output, shell_kill) never prompt: a stray
    // Always-ask setting or rule must not turn them into a card.
    if query.spec.class == ToolClass::Internal {
        return None;
    }
    if let Some(reason) = &query.inspection.always_ask {
        return Some((query.inspection.ask_code.unwrap_or("tool"), reason.clone()));
    }
    if let Some(reason) = query.command().and_then(safety::stdin_targets_reason) {
        return Some(("stdin_targets", reason.to_owned()));
    }
    if query.unsandboxed_shell() {
        return Some((
            "no_sandbox",
            "Sandbox unavailable: this command would run without an OS sandbox, so it can read and change anything your user account can. Acknowledge \"Run without a sandbox\" for this chat to continue."
                .to_owned(),
        ));
    }
    if tool_policy == Some(PermissionAction::Ask) {
        return Some((
            "tool_setting",
            "this tool is set to Always ask in Settings".to_owned(),
        ));
    }
    if matching
        .iter()
        .any(|rule| rule.action == PermissionAction::Ask)
    {
        return Some((
            "ask_rule",
            "a permission rule asks before this tool runs".to_owned(),
        ));
    }
    // `read_file` too: the host reads with privileges the shell sandbox lacks.
    let touches_files = (query.spec.class == ToolClass::Edit && !query.inspection.read_only)
        || query.tool_name == "read_file";
    (touches_files && query.env.folder_busy).then(|| {
        (
            "folder_busy",
            "a background process of this chat is running in this folder and could swap a path for a link between the check and the file access"
                .to_owned(),
        )
    })
}

/// The stored Allow rule that applies to the call, if one survives the re-check. A matching rule
/// that does not survive leaves its reason in `not_used`.
fn stored_allow<'a>(
    matching: &[&'a PermissionRule],
    query: &PermissionQuery<'_>,
    extra: &[String],
    not_used: &mut Option<String>,
) -> Option<&'a PermissionRule> {
    let ctx = query.safety();
    let mut best: Option<&PermissionRule> = None;
    for rule in matching
        .iter()
        .filter(|rule| rule.action == PermissionAction::Allow)
    {
        // Global rules do not exist any more: a rule needs a chat or a folder.
        if rule.session_id.is_none() && rule.workspace_dir.is_none() {
            continue;
        }
        match scope::recheck(
            query.spec,
            query.inspection,
            rule.arg_pattern.as_deref(),
            rule.session_id.is_some(),
            extra,
            &ctx,
        ) {
            Ok(()) => {
                // The newest of the rules that still apply.
                if best.is_none_or(|current| rule.created_at_ms >= current.created_at_ms) {
                    best = Some(rule);
                }
            }
            Err(reason) => {
                not_used.get_or_insert_with(|| reason);
            }
        }
    }
    best
}

/// The mode's default for this call (the mode x class table of the PRD).
fn mode_decision(query: &PermissionQuery<'_>) -> PermissionDecision {
    let reads = query.spec.class == ToolClass::Internal
        || (query.inspection.read_only && query.spec.class != ToolClass::Network);
    let layer = DecisionLayer::ModeDefault;
    match query.policy {
        ToolApproval::AutoAll => {
            PermissionDecision::new(Outcome::Allow, layer, "Allowed by Full access")
        }
        ToolApproval::AutoReadOnly if reads => {
            PermissionDecision::new(Outcome::Allow, layer, "Allowed by Approve reads")
        }
        ToolApproval::AutoReadOnly if sandbox_auto_runs(query) => PermissionDecision::new(
            Outcome::Allow,
            layer,
            "Allowed by sandbox auto-run: a low-risk command inside the sandbox without network",
        ),
        ToolApproval::AutoReadOnly => PermissionDecision::new(
            Outcome::Ask,
            layer,
            "this call changes something or sends data out, so it needs approval",
        ),
        ToolApproval::Ask if query.spec.class == ToolClass::Internal => {
            PermissionDecision::new(Outcome::Allow, layer, "an internal tool never asks")
        }
        ToolApproval::Ask => {
            PermissionDecision::new(Outcome::Ask, layer, "the chat asks before every tool call")
        }
    }
}

/// In Auto-approve reads with `ai.agent.sandbox_auto_run` on, a `shell` call runs without
/// a card when the sandbox self-test passed (and the chat did not turn it off), the command gets
/// no network and its risk is not High. A forced Ask and Ask or Block rules were decided above.
fn sandbox_auto_runs(query: &PermissionQuery<'_>) -> bool {
    query.env.sandbox_auto_run
        && query.tool_name == SHELL
        && query.sandbox != SandboxKind::None
        && !query.env.sandbox_acknowledged
        && !shell_network_allowed(query.policy, false)
        && query.command().is_some_and(|command| {
            safety::assess(command, &query.safety()).level != RiskLevel::High
        })
}

/// Whether `rule` applies to `query`: tool, chat, folder and (for an `arg_pattern`) the call's
/// subject. A shell prefix matches any segment here so a compound command that contains the
/// prefix reaches the re-check and gets its "not used" reason.
fn matches_query(rule: &PermissionRule, query: &PermissionQuery<'_>) -> bool {
    rule.session_id
        .as_deref()
        .is_none_or(|session| session == query.session_id)
        && rule.workspace_dir.as_deref().is_none_or(|dir| {
            query
                .workspace_dir
                .is_some_and(|workspace| Path::new(dir) == workspace)
        })
        && glob_match(&rule.tool_pattern, query.tool_name)
        && rule.arg_pattern.as_deref().is_none_or(|pattern| {
            query
                .inspection
                .arg_subject
                .as_deref()
                .is_some_and(|subject| {
                    if query.tool_name == SHELL {
                        scope::shell_pattern_covers(pattern, subject)
                    } else if query.tool_name == "fetch_url" && !pattern.contains(['*', ':']) {
                        // A host rule: compared by host in the re-check.
                        true
                    } else {
                        glob_match(pattern, subject) || pattern.trim_matches('*').is_empty()
                    }
                })
        })
}

/// Whether `text` matches `pattern`, where `*` matches any run of characters (including none)
/// and everything else matches itself; the match is anchored and case-sensitive.
#[must_use]
pub fn glob_match(pattern: &str, text: &str) -> bool {
    // `*` is ASCII, so byte-wise matching never splits a multi-byte character wrongly.
    let (pattern, text) = (pattern.as_bytes(), text.as_bytes());
    let (mut p, mut t) = (0, 0);
    // Where the last `*` sits in the pattern and how much text it has absorbed so far.
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if pattern.get(p) == Some(&b'*') {
            star = Some((p, t));
            p += 1;
        } else if pattern.get(p) == Some(&text[t]) {
            p += 1;
            t += 1;
        } else if let Some((star_p, star_t)) = star {
            p = star_p + 1;
            t = star_t + 1;
            star = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&byte| byte == b'*')
}
/// Builds the approval card of an asked call.
#[must_use]
pub fn approval_card(
    query: &PermissionQuery<'_>,
    decision: &PermissionDecision,
    status: SandboxStatus,
) -> ApprovalCard {
    let ctx = query.safety();
    let command_risk = query.command().map(|command| safety::assess(command, &ctx));
    let (risk, risk_reason, operators, chained) = command_risk.map_or_else(
        || {
            let level = if query.inspection.destructive {
                RiskLevel::High
            } else if query.inspection.read_only {
                RiskLevel::Low
            } else {
                RiskLevel::Medium
            };
            (level, None, Vec::new(), false)
        },
        |risk| (risk.level, risk.reason, risk.operators, risk.chained),
    );
    let mut tags = Vec::new();
    if chained {
        tags.push(RiskTag::ChainedCommand);
    }
    if query.tool_name == SHELL {
        if status.state != SandboxState::Available {
            tags.push(RiskTag::NoSandbox);
        } else if query.env.sandbox_acknowledged {
            tags.push(RiskTag::SandboxOff);
        }
    }
    // Nothing lowers a forced Ask, so only "once" is worth offering then.
    let forced = decision.layer == DecisionLayer::ForcedAsk;
    let derived = scope::derive_rule(query.spec, query.inspection, &ctx);
    let offered_scopes = if forced {
        vec![ApprovalScope::Once]
    } else {
        scope::offered(query.spec, query.inspection, &ctx)
    };
    ApprovalCard {
        risk,
        risk_reason,
        tags,
        operators,
        why: forced.then(|| decision.reason.clone()),
        rule_not_used: decision.rule_not_used.clone(),
        sandbox: status,
        summary: summary_line(query),
        rule_label: derived.as_ref().ok().cloned().flatten().filter(|_| !forced),
        scope_disabled_reason: derived.err().filter(|_| {
            !forced
                && !query.inspection.read_only
                && !scope::once_only(query.spec, query.inspection)
        }),
        offered_scopes,
        layer: Some(decision.layer.as_str().to_owned()),
        why_code: decision.why_code.map(str::to_owned),
    }
}

/// The one line the board shows for a call.
fn summary_line(query: &PermissionQuery<'_>) -> String {
    const MAX: usize = 120;
    let text = match &query.inspection.preview {
        Some(ToolApprovalPreview::Command { cmd, .. }) => {
            cmd.lines().next().unwrap_or("").to_owned()
        }
        Some(ToolApprovalPreview::Diff { path, .. }) => format!("{}: {path}", query.tool_name),
        Some(ToolApprovalPreview::Url { url }) => format!("{}: {url}", query.tool_name),
        Some(ToolApprovalPreview::Summary { text }) => text.clone(),
        None => query.inspection.arg_subject.as_deref().map_or_else(
            || query.tool_name.to_owned(),
            |subject| {
                format!(
                    "{}: {}",
                    query.tool_name,
                    subject.lines().next().unwrap_or("")
                )
            },
        ),
    };
    if text.chars().count() > MAX {
        let mut cut: String = text.chars().take(MAX).collect();
        cut.push('…');
        cut
    } else {
        text
    }
}
