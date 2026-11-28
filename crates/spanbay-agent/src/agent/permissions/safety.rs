//! The safety rules (best effort) and the risk label of a shell command.
//!
//! Single responsibility: everything the core knows about a command line before anyone approves
//! it. The blocked-command list is one artifact: the pinned default rules ([`PINNED_RULES`], shown
//! read-only in Settings, never removable) plus the user's extra patterns (add-only, at most
//! [`MAX_EXTRA_RULES`], never lifting a pinned rule). [`blocked_by`] is the hard-block check;
//! [`assess`] computes the risk level (Low, Medium, High with a plain-words reason), the
//! "Chained command" operators with their positions and the safety-rules flag the approval card and
//! the stored-rule re-check read. [`stdin_targets_reason`] is the forced Ask for a recursive delete
//! or permission change whose targets `xargs` reads from standard input. It is a guard against
//! common destructive mistakes, not a wall: it cannot see `eval "$(… | base64 -d)"`, `python -c`,
//! an alias or a script already in the Folder. The OS sandbox is the boundary.

use std::path::{Component, Path, PathBuf};

pub use spanbay_model::agent::permissions::{OperatorHit, RiskLevel};

use serde::{Deserialize, Serialize};

use super::command::{self, Analysis, Sep};
use spanbay_base::error::AppError;

/// Most extra safety patterns a user may add.
pub const MAX_EXTRA_RULES: usize = 64;

/// One pinned default rule, as Settings lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PinnedRule {
    /// Stable id.
    pub id: &'static str,
    /// The rule in words.
    pub summary: &'static str,
}

/// The pinned default rules (version-pinned in code; the user can only add patterns next to
/// them).
pub const PINNED_RULES: &[PinnedRule] = &[
    PinnedRule {
        id: "privilege",
        summary: "sudo, su, doas",
    },
    PinnedRule {
        id: "recursive-delete",
        summary: "rm or rmdir with a recursive flag on /, ~, $HOME, /*, the folder, a parent of the folder or SpanBay's data folder",
    },
    PinnedRule {
        id: "disk",
        summary: "mkfs*, dd with of=/dev/*, diskutil erase*, partition*, apfs delete*",
    },
    PinnedRule {
        id: "power",
        summary: "shutdown, reboot, halt",
    },
    PinnedRule {
        id: "launchctl",
        summary: "launchctl, except print, list, version, help and blame",
    },
    PinnedRule {
        id: "security",
        summary: "the macOS security command",
    },
    PinnedRule {
        id: "desktop",
        summary: "osascript, open, defaults, crontab, at",
    },
    PinnedRule {
        id: "recursive-permissions",
        summary: "chmod or chown with a recursive flag on / or ~",
    },
    PinnedRule {
        id: "pipe-to-interpreter",
        summary: "curl or wget piped into sh, bash, zsh, python or node",
    },
    PinnedRule {
        id: "fork-bomb",
        summary: "the classic fork bomb",
    },
    PinnedRule {
        id: "data-folder",
        summary: "any argument that resolves into SpanBay's data folder",
    },
];

/// Where a command runs: what the rules compare paths with.
#[derive(Debug, Clone, Copy, Default)]
pub struct SafetyContext<'a> {
    /// The chat's canonical folder.
    pub folder: Option<&'a Path>,
    /// SpanBay's data directory.
    pub data_dir: Option<&'a Path>,
    /// The user's home directory.
    pub home: Option<&'a Path>,
}

/// The rule that blocked a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedBy {
    /// The pinned rule id, or the user's pattern.
    pub rule: String,
    /// A pinned default rule (true) or one of the user's extra patterns (false).
    pub pinned: bool,
}

/// Checks the extra patterns for the add-only list: at most [`MAX_EXTRA_RULES`], none empty.
///
/// # Errors
/// [`AppError::Validation`] naming the problem.
pub fn validate_extra_rules(extra: &[String]) -> Result<(), AppError> {
    if extra.len() > MAX_EXTRA_RULES {
        return Err(AppError::validation(format!(
            "at most {MAX_EXTRA_RULES} extra safety rules are allowed"
        )));
    }
    if extra.iter().any(|pattern| pattern.trim().is_empty()) {
        return Err(AppError::validation("an extra safety rule cannot be empty"));
    }
    Ok(())
}

/// The first safety rule that forbids `command`: a pinned default, then the user's extra
/// patterns.
#[must_use]
pub fn blocked_by(extra: &[String], command: &str, ctx: &SafetyContext<'_>) -> Option<BlockedBy> {
    let analysis = command::analyze(command);
    if let Some(rule) = pinned_match(command, &analysis, ctx) {
        return Some(BlockedBy {
            rule: rule.to_owned(),
            pinned: true,
        });
    }
    command::blocked_pattern(extra, command).map(|pattern| BlockedBy {
        rule: pattern.to_owned(),
        pinned: false,
    })
}

/// Whether the extra patterns alone forbid `command` (used by the stored-rule re-check with the
/// same list the decision uses).
#[must_use]
pub fn on_safety_list(extra: &[String], command: &str, ctx: &SafetyContext<'_>) -> bool {
    blocked_by(extra, command, ctx).is_some()
}

/// Why `command` asks in every mode: `xargs` runs a recursive `rm`, `rmdir`, `chmod` or `chown`
/// whose targets come from standard input, so the target rules above cannot see them.
#[must_use]
pub fn stdin_targets_reason(command: &str) -> Option<&'static str> {
    if !command.contains("xargs") {
        return None;
    }
    command::analyze(command)
        .stdin_fed
        .iter()
        .any(|argv| {
            matches!(first(argv), "rm" | "rmdir" | "chmod" | "chown")
                && argv[1..].iter().any(|arg| is_recursive_flag(arg))
        })
        .then_some(
            "xargs passes targets from its input to a recursive delete or permission change, so the safety rules cannot check them",
        )
}

/// Programs whose purpose is to contact a remote host given on the command line.
const NETWORK_CLIENTS: [&str; 15] = [
    "curl", "wget", "http", "https", "xh", "nc", "ncat", "netcat", "socat", "telnet", "ftp",
    "sftp", "ssh", "scp", "rsync",
];
/// Clients that take a URL or a bare `host/path` operand.
const URL_CLIENTS: [&str; 5] = ["curl", "wget", "http", "https", "xh"];
/// Clients whose first operand is a host name.
const HOST_CLIENTS: [&str; 7] = ["nc", "ncat", "netcat", "telnet", "ftp", "ssh", "sftp"];
/// Options of the URL clients whose value is the next word (a file, a header, a body), never a
/// host.
const VALUE_OPTIONS: [&str; 30] = [
    "-o",
    "-O",
    "--output",
    "-d",
    "--data",
    "--data-raw",
    "--data-binary",
    "--data-urlencode",
    "-H",
    "--header",
    "-X",
    "--request",
    "-u",
    "--user",
    "-A",
    "--user-agent",
    "-e",
    "--referer",
    "-F",
    "--form",
    "-T",
    "--upload-file",
    "-w",
    "--write-out",
    "-b",
    "--cookie",
    "-c",
    "--cookie-jar",
    "-P",
    "--output-document",
];
/// `git` subcommands that talk to a remote.
const GIT_REMOTE: [&str; 5] = ["push", "fetch", "pull", "clone", "ls-remote"];

/// How an operand may name a host.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HostForm {
    /// A URL, `user@host[:path]` or `host:path` only.
    Remote,
    /// Also a bare `name.tld/path` or IP literal (curl, wget).
    Url,
    /// Any plausible host word (the first operand of `nc`, `ssh`, …).
    Bare,
}

/// The hosts `command` contacts explicitly, when any of its simple commands is a network client
/// (`curl`, `wget`, `nc`, `ssh`, `scp`, `rsync` to a remote, …) or a `git` subcommand that talks to a
/// remote: `Some` with the hosts its arguments name (lower case; empty when they name none the
/// rules can read, as in `git push origin`), `None` for a command that names no remote contact.
/// Best effort like the other rules: a script or an interpreter is not looked into.
#[must_use]
pub fn remote_hosts(command: &str) -> Option<Vec<String>> {
    let analysis = command::analyze(command);
    let mut found = false;
    let mut hosts: Vec<String> = Vec::new();
    for argv in &analysis.elements {
        let argv = command::strip_wrappers(argv);
        let name = file_name(first(&argv));
        let args = &argv[argv.len().min(1)..];
        let contacts = match name {
            "git" => args
                .iter()
                .find(|arg| !arg.starts_with('-'))
                .is_some_and(|sub| GIT_REMOTE.contains(&sub.as_str())),
            // rsync between two local paths contacts nobody.
            "rsync" => args
                .iter()
                .any(|arg| arg_host(arg, HostForm::Remote).is_some()),
            _ => NETWORK_CLIENTS.contains(&name),
        };
        if !contacts {
            continue;
        }
        found = true;
        let mut operand = 0;
        let mut skip = false;
        for arg in args {
            if std::mem::take(&mut skip) {
                continue;
            }
            if arg.starts_with('-') {
                skip = URL_CLIENTS.contains(&name) && VALUE_OPTIONS.contains(&arg.as_str());
                continue;
            }
            let form = if URL_CLIENTS.contains(&name) {
                HostForm::Url
            } else if HOST_CLIENTS.contains(&name) && operand == 0 {
                HostForm::Bare
            } else {
                HostForm::Remote
            };
            operand += 1;
            if let Some(host) = arg_host(arg, form)
                && !hosts.contains(&host)
            {
                hosts.push(host);
            }
        }
    }
    found.then_some(hosts)
}

/// The last path component of a program word (`/usr/bin/curl` → `curl`).
fn file_name(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// The host an operand names in `form` (see [`HostForm`]), lower case.
fn arg_host(arg: &str, form: HostForm) -> Option<String> {
    if arg.contains("://") {
        let url = url::Url::parse(arg).ok()?;
        return url
            .host_str()
            .map(|host| host.trim_matches(['[', ']']).to_ascii_lowercase());
    }
    let (rest, user) = match arg.split_once('@') {
        Some((_, rest)) => (rest, true),
        None => (arg, false),
    };
    let (name, remote_path) = match rest.split_once(':') {
        Some((name, _)) if !name.contains('/') => (name, true),
        _ => (rest.split('/').next().unwrap_or(rest), false),
    };
    let word = !name.is_empty()
        && !name.starts_with(['.', '-'])
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_'));
    let named = match form {
        HostForm::Remote => user || remote_path,
        HostForm::Url => user || looks_like_domain(name),
        HostForm::Bare => true,
    };
    (word && named).then(|| name.to_ascii_lowercase())
}

/// `name.tld` with an alphabetic top-level label, `localhost`, or an IPv4 literal.
fn looks_like_domain(name: &str) -> bool {
    if name.parse::<std::net::Ipv4Addr>().is_ok() || name == "localhost" {
        return true;
    }
    name.rsplit_once('.').is_some_and(|(head, tld)| {
        !head.is_empty() && tld.len() >= 2 && tld.bytes().all(|byte| byte.is_ascii_alphabetic())
    })
}

/// Options of curl and wget that send the request elsewhere than the operand names (a proxy, a
/// forced address) or read more targets or settings from a file or a startup command.
const REROUTING_OPTIONS: [&str; 15] = [
    "-x",
    "--proxy",
    "--preproxy",
    "--socks4",
    "--socks4a",
    "--socks5",
    "--socks5-hostname",
    "--resolve",
    "--connect-to",
    "-K",
    "--config",
    "-i",
    "--input-file",
    "-e",
    "--execute",
];

/// Whether the arguments of a `curl` or `wget` call name only loopback hosts (`localhost`,
/// `127.0.0.0/8`, `::1`): every operand must read as one, and no option may reroute the request.
/// An operand the rules cannot read (a variable, an unknown option's value) keeps the call a
/// remote contact.
fn loopback_only(args: &[String]) -> bool {
    let mut skip = false;
    let mut named = false;
    for arg in args {
        if std::mem::take(&mut skip) {
            continue;
        }
        if arg.starts_with('-') {
            let rerouting = REROUTING_OPTIONS.iter().any(|option| {
                if option.starts_with("--") {
                    arg == option
                        || arg
                            .strip_prefix(option)
                            .is_some_and(|rest| rest.starts_with('='))
                } else {
                    // A short option may be glued to its value (`-xhost:8080`).
                    !arg.starts_with("--") && arg.starts_with(option)
                }
            });
            if rerouting {
                return false;
            }
            skip = VALUE_OPTIONS.contains(&arg.as_str());
            continue;
        }
        let loopback = arg_host(arg, HostForm::Url).is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !loopback {
            return false;
        }
        named = true;
    }
    named
}

/// The id of the first pinned rule that matches.
fn pinned_match(
    command: &str,
    analysis: &Analysis,
    ctx: &SafetyContext<'_>,
) -> Option<&'static str> {
    let compact: String = command.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains(":(){") {
        return Some("fork-bomb");
    }
    for argv in &analysis.elements {
        if let Some(id) = pinned_argv(argv, ctx) {
            return Some(id);
        }
    }
    if analysis.top.windows(2).any(|pair| {
        pair[0].1 == Sep::Pipe
            && matches!(first(&command::strip_wrappers(&pair[0].0)), "curl" | "wget")
            && is_interpreter(first(&command::strip_wrappers(&pair[1].0)))
    }) {
        return Some("pipe-to-interpreter");
    }
    None
}

/// The first word of an argv, or "".
fn first(argv: &[String]) -> &str {
    argv.first().map_or("", String::as_str)
}

/// Interpreters a download must not be piped into.
fn is_interpreter(word: &str) -> bool {
    matches!(word, "sh" | "bash" | "zsh" | "dash" | "ksh" | "node") || word.starts_with("python")
}

/// Whether `word` is a recursive flag of `rm`/`chmod`/`chown`: `-r`, `-R`, `--recursive` or a
/// short cluster containing `r` or `R`.
fn is_recursive_flag(word: &str) -> bool {
    word == "--recursive"
        || (word.starts_with('-')
            && !word.starts_with("--")
            && word[1..].chars().any(|c| c == 'r' || c == 'R'))
}

fn pinned_argv(argv: &[String], ctx: &SafetyContext<'_>) -> Option<&'static str> {
    let name = first(argv);
    let args = &argv[argv.len().min(1)..];
    match name {
        "sudo" | "su" | "doas" => return Some("privilege"),
        "rm" | "rmdir" => {
            if args.iter().any(|arg| is_recursive_flag(arg))
                && args
                    .iter()
                    .filter(|arg| !arg.starts_with('-'))
                    .any(|target| catastrophic_target(target, ctx))
            {
                return Some("recursive-delete");
            }
        }
        "dd" => {
            if args.iter().any(|arg| arg.starts_with("of=/dev/")) {
                return Some("disk");
            }
        }
        "diskutil" => {
            let lower: Vec<String> = args.iter().map(|arg| arg.to_ascii_lowercase()).collect();
            let apfs_delete = lower
                .windows(2)
                .any(|pair| pair[0] == "apfs" && pair[1].starts_with("delete"));
            if apfs_delete
                || lower
                    .iter()
                    .any(|arg| arg.starts_with("erase") || arg.starts_with("partition"))
            {
                return Some("disk");
            }
        }
        name if name.starts_with("mkfs") => return Some("disk"),
        "shutdown" | "reboot" | "halt" => return Some("power"),
        // Only the read-only subcommands pass: every other one (`load`, `submit`, `setenv`,
        // `asuser`, `kickstart`, …) changes what launchd runs, inside or outside the sandbox.
        "launchctl" => {
            let first = args.iter().find(|arg| !arg.starts_with('-'));
            if !first.is_some_and(|sub| {
                matches!(
                    sub.as_str(),
                    "print" | "list" | "version" | "help" | "blame"
                )
            }) {
                return Some("launchctl");
            }
        }
        "security" => return Some("security"),
        "osascript" | "open" | "defaults" | "crontab" | "at" => return Some("desktop"),
        "chmod" | "chown"
            if args.iter().any(|arg| is_recursive_flag(arg))
                && args
                    .iter()
                    .filter(|arg| !arg.starts_with('-'))
                    .any(|target| {
                        matches!(target.as_str(), "/" | "~" | "~/" | "$HOME" | "${HOME}")
                    }) =>
        {
            return Some("recursive-permissions");
        }
        _ => {}
    }
    // Any argument (also a redirect target) that resolves into the data folder.
    if let Some(data) = ctx.data_dir
        && argv
            .iter()
            .map(|word| word.trim_start_matches(['>', '<']))
            .any(|word| resolves_into(word, ctx, data))
    {
        return Some("data-folder");
    }
    None
}

/// `/`, `~`, `$HOME`, `/*`, the folder, a parent of the folder or the data folder.
fn catastrophic_target(target: &str, ctx: &SafetyContext<'_>) -> bool {
    if matches!(
        target.trim_end_matches('/'),
        "" | "~" | "$HOME" | "${HOME}" | "/*" | "~/*"
    ) && !target.is_empty()
    {
        return true;
    }
    let Some(path) = resolve(target, ctx) else {
        return false;
    };
    if path.parent().is_none() {
        return true;
    }
    let is_folder_or_parent = ctx.folder.is_some_and(|folder| folder.starts_with(&path));
    let is_home = ctx.home.is_some_and(|home| home == path);
    let in_data = ctx
        .data_dir
        .is_some_and(|data| path.starts_with(data) || data.starts_with(&path));
    is_folder_or_parent || is_home || in_data
}

/// The lexically normalised absolute path `word` names (`~` and `$HOME` expanded, relative paths
/// joined to the folder), or `None` when it does not look like a path.
fn resolve(word: &str, ctx: &SafetyContext<'_>) -> Option<PathBuf> {
    let word = word.split_once('=').map_or(
        word,
        |(name, value)| {
            if name.starts_with('-') { value } else { word }
        },
    );
    let expanded = if let Some(rest) = word
        .strip_prefix("~/")
        .or_else(|| word.strip_prefix("$HOME/"))
        .or_else(|| word.strip_prefix("${HOME}/"))
    {
        ctx.home?.join(rest)
    } else if matches!(word, "~" | "$HOME" | "${HOME}") {
        ctx.home?.to_path_buf()
    } else if word.starts_with('/') {
        PathBuf::from(word)
    } else if word.contains('/') || word == ".." || word == "." {
        if word.starts_with('-') || word.contains("://") {
            return None;
        }
        ctx.folder?.join(word)
    } else {
        return None;
    };
    Some(normalize(&expanded))
}

/// Resolves `.` and `..` without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn resolves_into(word: &str, ctx: &SafetyContext<'_>, dir: &Path) -> bool {
    resolve(word, ctx).is_some_and(|path| path.starts_with(dir))
}

/// A path the command names that leaves the folder (the standard output sinks excepted).
fn outside_folder(word: &str, ctx: &SafetyContext<'_>) -> bool {
    let Some(folder) = ctx.folder else {
        return false;
    };
    if matches!(
        word,
        "/dev/null" | "/dev/stdout" | "/dev/stderr" | "/dev/tty" | "/dev/stdin"
    ) {
        return false;
    }
    resolve(word, ctx).is_some_and(|path| !path.starts_with(folder))
}

/// What the core knows about one shell command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRisk {
    /// Low, Medium or High.
    pub level: RiskLevel,
    /// Why the level is High ("deletes files"); `None` otherwise.
    pub reason: Option<String>,
    /// The command chains, substitutes or redirects (the "Chained command" tag).
    pub chained: bool,
    /// The operators that make it chained, in order.
    pub operators: Vec<OperatorHit>,
}

/// Scans `command` for chain, substitution and redirection operators (quote-aware: single quotes
/// hide everything, double quotes only hide the chain operators, not `$(` and backticks).
#[must_use]
pub fn chain_operators(command: &str) -> Vec<OperatorHit> {
    let chars: Vec<char> = command.chars().collect();
    let mut hits = Vec::new();
    let mut quote = '\0';
    // Whether a backtick substitution is open (only the opening backtick is an operator).
    let mut backtick = false;
    let mut i = 0;
    let mut push = |op: &str, start: usize| {
        hits.push(OperatorHit {
            op: op.to_owned(),
            start,
        });
    };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match quote {
            '\'' => {
                if c == '\'' {
                    quote = '\0';
                }
            }
            '"' => match c {
                '"' => quote = '\0',
                '\\' => i += 1,
                '`' => {
                    if !backtick {
                        push("`", i);
                    }
                    backtick = !backtick;
                }
                '$' if next == Some('(') => {
                    push("$(", i);
                    i += 1;
                }
                _ => {}
            },
            _ => match c {
                '\'' | '"' => quote = c,
                '\\' => i += 1,
                '&' if next == Some('&') => {
                    push("&&", i);
                    i += 1;
                }
                '&' if matches!(i.checked_sub(1).map(|p| chars[p]), Some('>' | '<')) => {}
                '&' if next == Some('>') => {
                    push("&>", i);
                    i += 1;
                }
                '&' => push("&", i),
                '|' if next == Some('|') => {
                    push("||", i);
                    i += 1;
                }
                '|' => push("|", i),
                ';' => push(";", i),
                '\n' => push("\\n", i),
                '`' => {
                    if !backtick {
                        push("`", i);
                    }
                    backtick = !backtick;
                }
                '$' if next == Some('(') => {
                    push("$(", i);
                    i += 1;
                }
                '>' if next == Some('>') => {
                    push(">>", i);
                    i += 1;
                }
                '>' => push(">", i),
                '<' if next == Some('<') => {
                    push("<<", i);
                    i += 1;
                }
                '<' => push("<", i),
                _ => {}
            },
        }
        i += 1;
    }
    hits
}

/// Commands that only read (the Low set).
const READ_ONLY: &[&str] = &["ls", "cat", "head", "tail", "wc", "pwd", "which"];
/// `git` subcommands that only read.
const READ_ONLY_GIT: &[&str] = &["status", "diff", "log", "show"];

/// Computes the risk of `command`.
#[must_use]
pub fn assess(command: &str, ctx: &SafetyContext<'_>) -> CommandRisk {
    let analysis = command::analyze(command);
    let operators = chain_operators(command);
    let chained = !operators.is_empty() || analysis.compound;
    let reason = high_reason(&analysis, &operators, ctx);
    let level = if reason.is_some() {
        RiskLevel::High
    } else if !chained
        && analysis
            .top
            .first()
            .is_some_and(|(argv, _)| read_only(argv))
    {
        RiskLevel::Low
    } else {
        RiskLevel::Medium
    };
    CommandRisk {
        level,
        reason,
        chained,
        operators,
    }
}

fn read_only(argv: &[String]) -> bool {
    let name = first(argv);
    if READ_ONLY.contains(&name) {
        return true;
    }
    if name != "git" {
        return false;
    }
    match argv.get(1).map(String::as_str) {
        Some(sub) if READ_ONLY_GIT.contains(&sub) => true,
        // `git branch` lists; any name or delete/move flag changes things.
        Some("branch") => argv[2..].iter().all(|arg| {
            matches!(
                arg.as_str(),
                "-a" | "-r" | "-v" | "-vv" | "--list" | "--show-current"
            )
        }),
        _ => false,
    }
}

/// Why any segment of the command is High risk, in plain words.
fn high_reason(
    analysis: &Analysis,
    operators: &[OperatorHit],
    ctx: &SafetyContext<'_>,
) -> Option<String> {
    for argv in &analysis.elements {
        if let Some(reason) = high_segment(argv, ctx) {
            return Some(reason.to_owned());
        }
    }
    // A redirect to a path outside the folder.
    if operators
        .iter()
        .any(|hit| matches!(hit.op.as_str(), ">" | ">>" | "&>"))
    {
        for (argv, _) in &analysis.top {
            for (i, word) in argv.iter().enumerate() {
                let redirect = word.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
                let Some(target) = redirect.strip_prefix('>') else {
                    continue;
                };
                let target = target.trim_start_matches('>');
                let target = if target.is_empty() {
                    argv.get(i + 1).map_or("", String::as_str)
                } else {
                    target
                };
                if outside_folder(target, ctx) {
                    return Some("writes outside the folder".to_owned());
                }
            }
        }
    }
    None
}

fn high_segment(argv: &[String], ctx: &SafetyContext<'_>) -> Option<&'static str> {
    let name = first(argv);
    let args = &argv[argv.len().min(1)..];
    let has = |flag: &str| args.iter().any(|arg| arg == flag);
    let sub = args.first().map(String::as_str);
    let reason = match name {
        "rm" | "rmdir" => Some("deletes files"),
        "chmod" | "chown" => Some("changes permissions or owner"),
        "kill" | "pkill" => Some("stops processes"),
        "docker" | "kubectl" => Some("controls containers or clusters"),
        "curl" | "wget" if loopback_only(args) => None,
        "curl" | "wget" | "nc" | "ncat" | "netcat" | "socat" | "telnet" | "ftp" | "ssh" | "scp"
        | "sftp" => Some("contacts a remote host"),
        "mv" if args
            .iter()
            .any(|arg| !arg.starts_with('-') && outside_folder(arg, ctx)) =>
        {
            Some("moves files outside the folder")
        }
        "find" if has("-delete") => Some("deletes files"),
        "find"
            if args
                .windows(2)
                .any(|pair| pair[0] == "-exec" && pair[1] == "rm") =>
        {
            Some("deletes files")
        }
        "cargo" | "npm" if sub == Some("publish") => Some("publishes a package"),
        "git" => git_reason(args),
        _ => None,
    };
    if reason.is_some() {
        return reason;
    }
    if matches!(name, "rm" | "git") && (has("--force") || has("-f")) {
        return Some("forces the operation");
    }
    if args
        .iter()
        .any(|arg| !arg.starts_with('>') && !arg.starts_with('<') && outside_folder(arg, ctx))
    {
        return Some("touches paths outside the folder");
    }
    None
}

fn git_reason(args: &[String]) -> Option<&'static str> {
    let has = |flag: &str| args.iter().any(|arg| arg == flag);
    let sub = args.iter().find(|arg| !arg.starts_with('-'))?.as_str();
    let rest: Vec<&str> = args
        .iter()
        .skip_while(|arg| arg.as_str() != sub)
        .skip(1)
        .map(String::as_str)
        .collect();
    let rest_has = |flag: &str| rest.contains(&flag);
    match sub {
        "push" if has("--force-with-lease") || rest_has("--force-with-lease") => {
            Some("force-pushes to a remote")
        }
        "push" => Some("pushes to a remote"),
        "reset" if rest_has("--hard") => Some("discards local changes"),
        "clean" => Some("deletes untracked files"),
        "checkout" if rest_has("--") || rest_has(".") => Some("discards local changes"),
        "restore" if rest_has(".") => Some("discards local changes"),
        "stash" if rest.first() == Some(&"drop") => Some("drops a stash"),
        "branch" if rest_has("-D") => Some("deletes a branch"),
        _ => None,
    }
}

/// Whether `word` names a protected path: `.env*` anywhere, `.claude/` or `.agents/` content or a
/// shell start-up file.
#[must_use]
pub fn is_protected_word(word: &str) -> bool {
    let word = word.trim_start_matches(['>', '<']);
    let name = word.rsplit('/').next().unwrap_or(word);
    name.starts_with(".env")
        || matches!(
            name,
            ".zshrc" | ".bashrc" | ".profile" | ".zshenv" | ".bash_profile" | ".zprofile"
        )
        || word.contains(".claude/")
        || word.contains(".agents/")
        || word.starts_with(".claude")
        || word.starts_with(".agents")
}

#[cfg(test)]
mod tests {
    use super::chain_operators;

    #[test]
    fn chain_operators_with_cjk_and_consecutive_newlines_have_exact_byte_spans() {
        let cmd = "echo 你好世界\n\ncargo test -- --nocapture &&\necho 完成\n";
        let hits = chain_operators(cmd);
        assert_eq!(
            hits.iter().map(|h| h.op.as_str()).collect::<Vec<_>>(),
            ["\\n", "\\n", "&&", "\\n", "\\n"]
        );
        let mut prev_end = 0;
        for hit in &hits {
            let start = cmd
                .char_indices()
                .nth(hit.start)
                .expect("valid char index")
                .0;
            let end = start + hit.source_len();
            assert!(start >= prev_end, "operator ranges must not overlap");
            assert!(cmd.is_char_boundary(start));
            assert!(cmd.is_char_boundary(end));
            let expected = if hit.op == "\\n" {
                "\n"
            } else {
                hit.op.as_str()
            };
            assert_eq!(&cmd[start..end], expected);
            prev_end = end;
        }
    }
}
