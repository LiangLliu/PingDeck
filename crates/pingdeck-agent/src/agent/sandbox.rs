//! Which OS-level sandbox a shell command can run under, and the profile it runs with.
//!
//! Single responsibility: detect the sandbox tier of this machine (`Seatbelt` on macOS, `Bubblewrap`
//! on Linux when `bwrap` is on `PATH`, otherwise none), prove that it really applies (the
//! self-test: [`sandbox_status`], [`test_sandbox`]), classify a failure to apply it
//! ([`sandbox_apply_failed`]) and build the Seatbelt profile and the `bwrap` arguments of one
//! command ([`wrap_command`]). Whether a shell call may run unsandboxed is decided by
//! `agent::permissions`; spawning is `agent::tools::shell`.
//!
//! Seatbelt rule order (a later rule wins): deny default; allow executing, forking and process
//! info about the sandbox's own processes, `sysctl-read` and signalling inside the sandbox; allow
//! `mach-lookup`, then deny the pasteboard, AppleEvents, Launch Services, keychain services and
//! the process-list daemon (`sysmond`, which `pgrep`/`top` use to list every process of the
//! machine); allow every read; deny reads of the read-deny block ([`read_deny`]: the data root,
//! credential folders, browser profiles); re-allow reading the per-session temp directory; allow
//! writes (the Folder, the temp directory, `/private/tmp`, `/dev/null`, `/dev/tty`; Full access
//! also the toolchain caches); deny writes to the protected workspace paths ([`protect`]: git
//! hooks and config at any depth, agent folders, shell startup files); network only when allowed.
//! The `bwrap` arguments are built from the same lists, so the two profiles cannot drift apart.
//! `bwrap` additionally hides the session bus, the keyring and the container sockets of the host,
//! and runs in new IPC, UTS and terminal-session namespaces without capabilities.
//!
//! Every path of a [`SandboxSpec`] is canonicalised before it is written: Seatbelt matches the
//! resolved path of each access, so a rule for `/tmp/x` would silently match nothing. The sandbox,
//! not the safety rules, is the security boundary.

#[cfg(test)]
mod tests;

pub(crate) mod protect;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, PoisonError};

pub use pingdeck_model::agent::sandbox::{SandboxKind, SandboxState, SandboxStatus, SandboxTag};

/// The tier whose tool is installed on this machine; it may still fail to apply (see
/// [`sandbox_status`]).
#[must_use]
pub fn detect_kind() -> SandboxKind {
    if cfg!(target_os = "macos") && Path::new(SANDBOX_EXEC).is_file() {
        SandboxKind::Seatbelt
    } else if cfg!(target_os = "linux") && on_path("bwrap") {
        SandboxKind::Bubblewrap
    } else {
        SandboxKind::None
    }
}

/// The tier that works on this machine right now: [`sandbox_status`]'s usable tier.
#[must_use]
pub fn detect_checked_kind() -> SandboxKind {
    sandbox_status().usable()
}

use pingdeck_base::error::AppError;

/// Where macOS keeps `sandbox-exec`.
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// The self-test result of this process; `None` until the first call to [`sandbox_status`].
static STATUS: Mutex<Option<SandboxStatus>> = Mutex::new(None);

/// The cached self-test result. The first call runs the probe (blocking, a few milliseconds).
#[must_use]
pub fn sandbox_status() -> SandboxStatus {
    let mut cached = STATUS.lock().unwrap_or_else(PoisonError::into_inner);
    *cached.get_or_insert_with(run_self_test)
}

/// Runs the self-test again ("Test sandbox" in Settings) and caches and returns the result.
#[must_use]
pub fn test_sandbox() -> SandboxStatus {
    let status = run_self_test();
    *STATUS.lock().unwrap_or_else(PoisonError::into_inner) = Some(status);
    status
}

/// The self-test of this machine's own sandbox tool.
fn run_self_test() -> SandboxStatus {
    let tier = detect_kind();
    let program = match tier {
        SandboxKind::Seatbelt => Path::new(SANDBOX_EXEC),
        SandboxKind::Bubblewrap => Path::new("bwrap"),
        SandboxKind::None => {
            return SandboxStatus {
                state: SandboxState::Missing,
                tier,
            };
        }
    };
    self_test(tier, program)
}

/// Applies a minimal profile of `tier` with `program` to `/usr/bin/true` and classifies the
/// outcome.
fn self_test(tier: SandboxKind, program: &Path) -> SandboxStatus {
    let missing = SandboxStatus {
        state: SandboxState::Missing,
        tier: SandboxKind::None,
    };
    let Some(mut command) = probe_command(tier, program) else {
        return missing;
    };
    let result = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    match classify_probe(tier, result) {
        SandboxState::Missing => missing,
        state => SandboxStatus { state, tier },
    }
}

/// `program` running `/usr/bin/true` under the most minimal profile of `tier`.
fn probe_command(tier: SandboxKind, program: &Path) -> Option<Command> {
    let mut command = Command::new(program);
    match tier {
        SandboxKind::Seatbelt => command.args([
            "-p",
            "(version 1)(deny default)(allow process*)(allow file-read*)",
            "/usr/bin/true",
        ]),
        SandboxKind::Bubblewrap => command.args([
            "--ro-bind",
            "/",
            "/",
            "--unshare-pid",
            "--die-with-parent",
            "--",
            "/usr/bin/true",
        ]),
        SandboxKind::None => return None,
    };
    Some(command)
}

/// What a finished probe says: it ran (available), the sandbox could not be applied because the
/// process already sits in one (nested), or anything else (the tool is missing or unusable).
fn classify_probe(tier: SandboxKind, result: io::Result<Output>) -> SandboxState {
    match result {
        Ok(output) if output.status.success() => SandboxState::Available,
        Ok(output) if is_apply_error(tier, &String::from_utf8_lossy(&output.stderr)) => {
            SandboxState::Nested
        }
        _ => SandboxState::Missing,
    }
}

/// Whether `line` is the error `sandbox-exec` or `bwrap` prints when the sandbox cannot be
/// applied.
fn is_apply_error_line(tier: SandboxKind, line: &str) -> bool {
    match tier {
        // `sandbox-exec: sandbox_apply: Operation not permitted` (exit 71).
        SandboxKind::Seatbelt => line.contains("sandbox_apply"),
        SandboxKind::Bubblewrap => {
            line.starts_with("bwrap:")
                && [
                    "No permissions to create",
                    "Creating new namespace failed",
                    "setting up uid map",
                    "Operation not permitted",
                ]
                .iter()
                .any(|text| line.contains(text))
        }
        SandboxKind::None => false,
    }
}

/// Whether `stderr` holds only the apply error (at least one such line, no other line).
fn is_apply_error(tier: SandboxKind, stderr: &str) -> bool {
    let mut lines = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let first = lines.next();
    first.is_some_and(|line| is_apply_error_line(tier, line))
        && lines.all(|line| is_apply_error_line(tier, line))
}

/// Whether a finished command may have failed because the sandbox could not be applied: it exited
/// with the tool's own apply status (`sandbox-exec` 71, `bwrap` 1), its standard error holds the
/// tool's apply error and nothing else, and it printed nothing on stdout. A command controls its
/// own output and exit code, so this is only a trigger: the caller confirms it by running the
/// self-test probe again ([`test_sandbox`]) and trusts that, never the command's text.
#[must_use]
pub fn sandbox_apply_failed(
    tier: SandboxKind,
    exit_code: Option<i32>,
    stdout_empty: bool,
    stderr: &str,
) -> bool {
    let apply_status = match tier {
        SandboxKind::Seatbelt => 71,
        SandboxKind::Bubblewrap => 1,
        SandboxKind::None => return false,
    };
    exit_code == Some(apply_status) && stdout_empty && is_apply_error(tier, stderr)
}

/// Whether an executable called `name` is on `PATH`.
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
}

/// What a sandboxed command may not read, below `$HOME`: `(path, is_directory)`. One list for
/// both profiles. Entries that do not exist are harmless on Seatbelt and skipped for `bwrap`.
const HOME_READ_DENY: &[(&str, bool)] = &[
    (".ssh", true),
    (".aws", true),
    (".gnupg", true),
    (".config/gh", true),
    (".config/gcloud", true),
    (".azure", true),
    (".netrc", false),
    (".npmrc", false),
    (".pypirc", false),
    (".docker", true),
    (".kube", true),
    (".cargo/credentials", false),
    (".cargo/credentials.toml", false),
    // Credential stores of other tools. Only git's credential file is hidden, not the whole
    // `~/.config/git`: that folder also holds the user's identity, which `git commit` needs.
    (".git-credentials", false),
    (".config/git/credentials", false),
    (".config/op", true),
    (".password-store", true),
    (".vault-token", false),
    (".terraform.d/credentials.tfrc.json", false),
    (".oci", true),
    (".config/doctl", true),
    (".gem/credentials", false),
    (".m2/settings.xml", false),
    (".local/share/keyrings", true),
    // Shell history often holds pasted tokens.
    (".bash_history", false),
    (".zsh_history", false),
    ("Library/Keychains", true),
    ("Library/Cookies", true),
    ("Library/Safari", true),
    ("Library/Messages", true),
    ("Library/Mail", true),
    ("Library/Group Containers/2BUA8C4S2C.com.1password", true),
    // Chat apps (sessions and message caches).
    ("Library/Application Support/Slack", true),
    ("Library/Application Support/discord", true),
    (".config/Slack", true),
    (".config/discord", true),
    // Browser profiles (cookies, saved logins).
    ("Library/Application Support/Google/Chrome", true),
    ("Library/Application Support/Chromium", true),
    ("Library/Application Support/Microsoft Edge", true),
    ("Library/Application Support/BraveSoftware", true),
    ("Library/Application Support/Arc", true),
    ("Library/Application Support/Vivaldi", true),
    ("Library/Application Support/Firefox", true),
    (".config/google-chrome", true),
    (".config/chromium", true),
    (".config/microsoft-edge", true),
    (".config/BraveSoftware", true),
    (".config/vivaldi", true),
    (".mozilla", true),
];

/// Toolchain caches below `$HOME` a Full access command may also write (package managers fail
/// without).
const HOME_CACHE_DIRS: &[&str] = &[
    ".cargo/registry",
    ".cargo/git",
    ".npm/_cacache",
    ".cache",
    "Library/Caches",
];

/// Mach services a sandboxed command cannot look up: the pasteboard, AppleEvents, Launch
/// Services and the keychain daemons.
const DENIED_SERVICES: &[&str] = &[
    "com.apple.pasteboard.1",
    "com.apple.coreservices.appleevents",
    "com.apple.coreservices.launchservicesd",
    "com.apple.lsd.mapdb",
    "com.apple.lsd.modifydb",
    "com.apple.lsd.open",
    "com.apple.SecurityServer",
    "com.apple.securityd",
    "com.apple.secd",
    "com.apple.sysmond",
];

/// `/usr/bin/git` is an `xcrun` shim that caches in the user's temp folder, ignoring `TMPDIR`;
/// without this single file pattern every `git` call prints an error.
const XCRUN_CACHE_REGEX: &str = r#"(regex #"^/private/var/folders/[^/]+/[^/]+/T/xcrun_db-[^/]+$")"#;

/// What a sandboxed shell command may touch.
#[derive(Debug, Clone, Copy)]
pub struct SandboxSpec<'a> {
    /// The working directory; the only project location the command may write.
    pub workspace: &'a Path,
    /// PingDeck's data root (database, secrets): hidden from the command.
    pub data_root: Option<&'a Path>,
    /// The per-session temp directory the command may read and write even when it lives below
    /// `data_root`.
    pub tmp_dir: Option<&'a Path>,
    /// The user's home directory: its credential folders are hidden, and (Full access) its
    /// toolchain caches are writable.
    pub home: Option<&'a Path>,
    /// Whether the command may open network connections.
    pub allow_network: bool,
    /// Full access: wider writes (toolchain caches at their usual places). Every read-deny still
    /// applies.
    pub full_access: bool,
    /// The shell binary that runs the command line with `-c`.
    pub shell: &'a str,
    /// The command line.
    pub command: &'a str,
}

/// The program and arguments that run a [`SandboxSpec`]'s command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxedCommand {
    /// The executable to spawn.
    pub program: String,
    /// Its arguments, the command line last.
    pub args: Vec<String>,
}

pub use pingdeck_base::platform::user_shell;

/// Builds the invocation that runs `spec.command` under `kind`.
///
/// # Errors
/// [`AppError::Validation`] when a path is not valid UTF-8 or contains a control character (it
/// could not be written into a Seatbelt profile or an argument list safely).
pub fn wrap_command(
    kind: SandboxKind,
    spec: &SandboxSpec<'_>,
) -> Result<SandboxedCommand, AppError> {
    let shell_args = || vec![spec.shell.to_owned(), "-c".to_owned()];
    let (program, mut args) = match kind {
        SandboxKind::Seatbelt => {
            let mut args = vec!["-p".to_owned(), seatbelt_profile(spec)?];
            args.extend(shell_args());
            (SANDBOX_EXEC.to_owned(), args)
        }
        SandboxKind::Bubblewrap => {
            let mut args = bubblewrap_args(spec)?;
            args.push("--".to_owned());
            args.extend(shell_args());
            ("bwrap".to_owned(), args)
        }
        SandboxKind::None if cfg!(windows) => ("cmd.exe".to_owned(), vec!["/C".to_owned()]),
        SandboxKind::None => (spec.shell.to_owned(), vec!["-c".to_owned()]),
    };
    args.push(spec.command.to_owned());
    Ok(SandboxedCommand { program, args })
}

/// The canonical form of `path`, or `path` itself when it cannot be resolved (it does not exist).
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The read-deny block of `spec` (see [`denied_locations`]).
#[must_use]
pub fn read_deny(spec: &SandboxSpec<'_>) -> Vec<(PathBuf, bool)> {
    denied_locations(spec.data_root, spec.home)
}

/// The locations a sandboxed command may not read: the data root and every [`HOME_READ_DENY`]
/// entry below the home directory, as `(path, is_directory)`. An entry that is a symlink (dotfile
/// managers, `~/.ssh -> ~/Dropbox/ssh`) is listed under its own name and under the canonical path
/// it leads to: Seatbelt matches the resolved path of an access, so only the target spelling
/// hides the real files. The single source of both profiles and of the workspace check.
#[must_use]
pub fn denied_locations(data_root: Option<&Path>, home: Option<&Path>) -> Vec<(PathBuf, bool)> {
    let mut hidden = Vec::new();
    let mut push = |path: PathBuf, directory: bool| {
        let real = canonical(&path);
        if real != path {
            hidden.push((real, directory));
        }
        hidden.push((path, directory));
    };
    if let Some(root) = data_root {
        push(canonical(root), true);
    }
    if let Some(home) = home {
        let home = canonical(home);
        for (relative, directory) in HOME_READ_DENY {
            push(home.join(relative), *directory);
        }
    }
    hidden
}

/// `(<form> "<path>")` (`subpath` or `literal`) of a [`read_deny`] entry.
fn deny_form(path: &Path, directory: bool) -> Result<String, AppError> {
    scheme_path(if directory { "subpath" } else { "literal" }, path)
}

/// The Seatbelt profile for `spec` (see the module docs for the order of its rules).
///
/// # Errors
/// See [`wrap_command`].
pub fn seatbelt_profile(spec: &SandboxSpec<'_>) -> Result<String, AppError> {
    let workspace = canonical(spec.workspace);
    let mut profile = String::from(
        "(version 1)\n(deny default)\n(allow process-exec*)\n(allow process-fork)\n\
         (allow process-info* (target same-sandbox))\n(allow sysctl-read)\n\
         (allow signal (target same-sandbox))\n(allow mach-lookup)\n",
    );
    let services: Vec<String> = DENIED_SERVICES
        .iter()
        .map(|name| format!("(global-name \"{name}\")"))
        .collect();
    profile.push_str(&format!("(deny mach-lookup {})\n", services.join(" ")));
    profile.push_str("(allow file-read*)\n");
    let hidden = read_deny(spec)
        .iter()
        .map(|(path, directory)| deny_form(path, *directory))
        .collect::<Result<Vec<_>, _>>()?;
    if !hidden.is_empty() {
        profile.push_str(&format!("(deny file-read* {})\n", hidden.join(" ")));
    }
    let mut writable = vec![scheme_path("subpath", &workspace)?];
    if let Some(tmp) = spec.tmp_dir {
        let tmp = scheme_path("subpath", &canonical(tmp))?;
        // Later rules win: re-open the scratch directory after the data root was hidden.
        profile.push_str(&format!("(allow file-read* {tmp})\n"));
        writable.push(tmp);
    }
    writable.extend([
        r#"(subpath "/private/tmp")"#.to_owned(),
        r#"(literal "/dev/null")"#.to_owned(),
        r#"(literal "/dev/tty")"#.to_owned(),
        XCRUN_CACHE_REGEX.to_owned(),
    ]);
    if spec.full_access {
        writable.push(r#"(subpath "/private/var/folders")"#.to_owned());
        if let Some(home) = spec.home {
            let home = canonical(home);
            for dir in HOME_CACHE_DIRS {
                writable.push(scheme_path("subpath", &home.join(dir))?);
            }
        }
    }
    profile.push_str(&format!("(allow file-write* {})\n", writable.join(" ")));
    // After the allow rule: hooks and config of any repository, agent folders and startup files
    // change what runs outside the sandbox later.
    let protected = protect::seatbelt_regexes(path_arg(&workspace)?.as_str());
    profile.push_str(&format!("(deny file-write* {})\n", protected.join(" ")));
    if spec.allow_network {
        profile.push_str("(allow network*)\n");
    }
    Ok(profile)
}

/// The `bwrap` options for `spec` (everything before `-- <shell> -c <cmd>`): the root is
/// read-only, the workspace, the scratch directory and (Full access) the toolchain caches that
/// exist are writable (the protected paths of [`protect::existing_paths`] are bound read-only on
/// top), every existing [`read_deny`] directory is replaced by an empty tmpfs and every existing
/// file by `/dev/null`, the host's session bus and container sockets are hidden, and the PID, IPC,
/// UTS (and unless allowed, network) namespaces are private in a new terminal session without
/// capabilities.
///
/// # Errors
/// See [`wrap_command`].
pub fn bubblewrap_args(spec: &SandboxSpec<'_>) -> Result<Vec<String>, AppError> {
    let workspace_path = canonical(spec.workspace);
    let workspace = path_arg(&workspace_path)?;
    let mut args = strings([
        "--ro-bind",
        "/",
        "/",
        "--tmpfs",
        "/tmp",
        "--bind",
        &workspace,
        &workspace,
    ]);
    // Filesystem sockets ignore `--unshare-net`: the session bus can start units outside the
    // sandbox and the keyring and container sockets hand out secrets or root.
    if let Some(run_user) = run_user_dir().filter(|dir| dir.is_dir()) {
        args.extend(["--tmpfs".to_owned(), path_arg(&run_user)?]);
    }
    for socket in HOST_SOCKETS {
        if Path::new(socket).exists() {
            args.extend(strings(["--ro-bind", "/dev/null", socket]));
        }
    }
    // `--ro-bind` of a missing path would make bwrap fail.
    for path in protect::existing_paths(&workspace_path) {
        let path = path_arg(&path)?;
        args.extend(strings(["--ro-bind", &path, &path]));
    }
    // `--tmpfs` on a missing directory would make bwrap fail, and on a read-only root cannot
    // create it; on a symlink it is undefined (the canonical spelling is listed as well).
    for (path, directory) in read_deny(spec) {
        if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            continue;
        }
        if directory && path.is_dir() {
            args.extend(["--tmpfs".to_owned(), path_arg(&path)?]);
        } else if !directory && path.is_file() {
            args.extend(strings(["--ro-bind", "/dev/null", &path_arg(&path)?]));
        }
    }
    if let Some(tmp) = spec.tmp_dir.map(canonical).filter(|tmp| tmp.is_dir()) {
        let tmp = path_arg(&tmp)?;
        args.extend(strings(["--bind", &tmp, &tmp]));
    }
    if let Some(home) = spec.home.filter(|_| spec.full_access).map(canonical) {
        for dir in HOME_CACHE_DIRS {
            let dir = home.join(dir);
            if dir.is_dir() {
                let dir = path_arg(&dir)?;
                args.extend(strings(["--bind", &dir, &dir]));
            }
        }
    }
    args.extend(strings([
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--new-session",
        "--cap-drop",
        "ALL",
    ]));
    if !spec.allow_network {
        args.push("--unshare-net".to_owned());
    }
    args.push("--die-with-parent".to_owned());
    Ok(args)
}

/// Filesystem sockets of the host that give a sandboxed command more than it has itself.
const HOST_SOCKETS: [&str; 3] = [
    "/run/docker.sock",
    "/run/podman/podman.sock",
    "/run/dbus/system_bus_socket",
];

/// `/run/user/<uid>` of this process (session bus, keyring, gpg-agent sockets); `None` where the
/// uid cannot be read without `unsafe`.
fn run_user_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = fs::metadata("/proc/self").ok()?.uid();
        Some(PathBuf::from(format!("/run/user/{uid}")))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// `(<form> "<path>")` (`subpath` or `literal`) with the path escaped for Scheme string syntax.
fn scheme_path(form: &str, path: &Path) -> Result<String, AppError> {
    let text = path_arg(path)?;
    Ok(format!(
        "({form} \"{}\")",
        text.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

/// The path as UTF-8 text free of control characters.
fn path_arg(path: &Path) -> Result<String, AppError> {
    match path.to_str() {
        Some(text) if !text.chars().any(char::is_control) => Ok(text.to_owned()),
        _ => Err(AppError::validation(format!(
            "the path {} cannot be passed to the shell sandbox (not UTF-8 or contains control characters)",
            path.display()
        ))),
    }
}

/// Owned copies of string literals.
fn strings<const N: usize>(items: [&str; N]) -> Vec<String> {
    items.into_iter().map(str::to_owned).collect()
}
