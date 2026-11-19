//! Tests of the Seatbelt profile and bubblewrap argument builders, the self-test classification
//! and, on macOS, the real sandbox: every probe has a negative twin run without the sandbox.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::agent::test_dir::TempDir as Scratch;

fn spec<'a>(workspace: &'a Path, command: &'a str) -> SandboxSpec<'a> {
    SandboxSpec {
        workspace,
        data_root: None,
        tmp_dir: None,
        home: None,
        allow_network: false,
        full_access: false,
        shell: "/bin/sh",
        command,
    }
}

#[test]
fn seatbelt_rules_come_in_the_documented_order() {
    let spec = SandboxSpec {
        data_root: Some(Path::new("/data/pingdeck")),
        tmp_dir: Some(Path::new("/data/pingdeck/tmp")),
        home: Some(Path::new("/Users/me")),
        allow_network: true,
        ..spec(Path::new("/work/proj"), "ls")
    };
    let profile = seatbelt_profile(&spec).unwrap();
    let at = |needle: &str| {
        profile
            .find(needle)
            .unwrap_or_else(|| panic!("missing {needle}"))
    };
    let order = [
        at("(deny default)"),
        at("(allow process-exec*)"),
        at("(allow sysctl-read)"),
        at("(allow mach-lookup)"),
        at("(deny mach-lookup"),
        at("(allow file-read*)\n"),
        at("(deny file-read*"),
        at(r#"(allow file-read* (subpath "/data/pingdeck/tmp"))"#),
        at("(allow file-write*"),
        at("(deny file-write*"),
        at("(allow network*)"),
    ];
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "{order:?}\n{profile}"
    );
    for service in [
        "com.apple.pasteboard.1",
        "com.apple.coreservices.appleevents",
        "com.apple.coreservices.launchservicesd",
        "com.apple.SecurityServer",
    ] {
        assert!(profile.contains(service), "{service}");
    }
}

#[test]
fn the_read_deny_block_has_every_listed_location_and_both_profiles_use_it() {
    let scratch = Scratch::new("sandbox-deny");
    let home = scratch.0.join("home");
    let data = scratch.0.join("data");
    let workspace = scratch.0.join("ws");
    for (relative, directory) in HOME_READ_DENY {
        let path = home.join(relative);
        if *directory {
            std::fs::create_dir_all(&path).unwrap();
        } else {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "x").unwrap();
        }
    }
    std::fs::create_dir_all(&data).unwrap();
    let spec = SandboxSpec {
        data_root: Some(&data),
        home: Some(&home),
        ..spec(&workspace, "ls")
    };
    let denied = read_deny(&spec);
    let names: Vec<String> = denied
        .iter()
        .map(|(path, _)| {
            path.strip_prefix(canonical(&home))
                .map_or_else(|_| "DATA".to_owned(), |p| p.display().to_string())
        })
        .collect();
    for required in [
        "DATA",
        ".ssh",
        ".aws",
        ".gnupg",
        ".config/gh",
        ".config/gcloud",
        ".azure",
        ".netrc",
        ".npmrc",
        ".pypirc",
        ".docker",
        ".kube",
        ".cargo/credentials",
        "Library/Keychains",
        "Library/Cookies",
        "Library/Safari",
        "Library/Application Support/Google/Chrome",
        "Library/Application Support/Firefox",
        ".git-credentials",
        ".config/git/credentials",
        ".bash_history",
        ".zsh_history",
        ".config/op",
        ".password-store",
        ".vault-token",
        ".terraform.d/credentials.tfrc.json",
        ".oci",
        ".config/doctl",
        ".gem/credentials",
        ".m2/settings.xml",
        ".local/share/keyrings",
        "Library/Messages",
        "Library/Mail",
        "Library/Application Support/Slack",
        "Library/Application Support/discord",
    ] {
        assert!(names.iter().any(|name| name == required), "{required}");
    }
    let profile = seatbelt_profile(&spec).unwrap();
    let args = bubblewrap_args(&spec).unwrap().join(" ");
    for (path, directory) in &denied {
        let text = path.to_str().unwrap();
        let form = if *directory { "subpath" } else { "literal" };
        assert!(
            profile.contains(&format!("({form} \"{text}\")")),
            "seatbelt {text}"
        );
        let bwrap = if *directory {
            format!("--tmpfs {text}")
        } else {
            format!("--ro-bind /dev/null {text}")
        };
        assert!(args.contains(&bwrap), "bwrap {bwrap}");
    }
}

#[test]
fn writes_are_narrow_and_full_access_widens_only_the_writes() {
    let home = Path::new("/Users/me");
    let narrow = seatbelt_profile(&SandboxSpec {
        home: Some(home),
        ..spec(Path::new("/w"), "ls")
    })
    .unwrap();
    let write_rule = narrow
        .lines()
        .find(|l| l.starts_with("(allow file-write*"))
        .unwrap();
    assert!(
        write_rule.contains(r#"(subpath "/w")"#)
            && write_rule.contains(r#"(subpath "/private/tmp")"#)
    );
    assert!(
        write_rule.contains(r#"(literal "/dev/null")"#)
            && write_rule.contains(r#"(literal "/dev/tty")"#)
    );
    assert!(!write_rule.contains("/private/var/folders/") || write_rule.contains("xcrun_db"));
    assert!(!write_rule.contains("Library/Caches") && !write_rule.contains(".cargo"));
    assert!(!narrow.contains("network"));

    let full = seatbelt_profile(&SandboxSpec {
        home: Some(home),
        full_access: true,
        allow_network: true,
        ..spec(Path::new("/w"), "ls")
    })
    .unwrap();
    for cache in [
        "/Users/me/.cargo/registry",
        "/Users/me/.cargo/git",
        "/Users/me/.npm/_cacache",
        "/Users/me/.cache",
        "/Users/me/Library/Caches",
    ] {
        assert!(full.contains(&format!("(subpath \"{cache}\")")), "{cache}");
    }
    assert!(full.contains("(allow network*)"));
    // The read-deny block still applies in Full access.
    assert!(full.contains(r#"(subpath "/Users/me/.ssh")"#));
}

#[test]
fn the_scratch_directory_is_reopened_after_the_deny() {
    let spec = SandboxSpec {
        data_root: Some(Path::new("/data")),
        tmp_dir: Some(Path::new("/data/tmp")),
        ..spec(Path::new("/w"), "ls")
    };
    let profile = seatbelt_profile(&spec).unwrap();
    let deny = profile.find("(deny file-read*").unwrap();
    let reopen = profile
        .find(r#"(allow file-read* (subpath "/data/tmp"))"#)
        .unwrap();
    assert!(deny < reopen, "later rules win in Seatbelt");
}

#[test]
fn profile_paths_are_canonical() {
    let scratch = Scratch::new("sandbox-canon");
    let real = scratch.0.join("real");
    std::fs::create_dir_all(&real).unwrap();
    let link = scratch.0.join("link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let profile = seatbelt_profile(&spec(&link, "ls")).unwrap();
    assert!(profile.contains(&format!("(subpath \"{}\")", real.display())));
    assert!(!profile.contains(&format!("\"{}\"", link.display())));
    // `/tmp` is `/private/tmp` on macOS: a rule written for the symlink matches nothing.
    if cfg!(target_os = "macos") {
        let profile = seatbelt_profile(&spec(Path::new("/tmp"), "ls")).unwrap();
        assert!(profile.contains(r#"(subpath "/private/tmp")"#));
    }
}

#[test]
fn git_hooks_agent_folders_and_startup_files_are_denied_after_the_allow_rule() {
    let profile = seatbelt_profile(&spec(Path::new("/work/proj"), "ls")).unwrap();
    let deny_at = profile.find("(deny file-write* (regex").unwrap();
    assert!(profile.find("(allow file-write*").unwrap() < deny_at);
    let deny = &profile[deny_at
        ..profile[deny_at..]
            .find('\n')
            .map_or(profile.len(), |n| deny_at + n)];
    // Four path patterns anchored at the folder: git hooks/config at any depth (submodules and
    // worktrees too), agent and editor folders, startup files and `.cargo/config.toml`. Letters, the folder's too, are
    // case-insensitive classes because the macOS volume is.
    let anchor = r#"(regex #"^/[wW][oO][rR][kK]/[pP][rR][oO][jJ]/"#;
    assert_eq!(deny.matches(anchor).count(), 4, "{deny}");
    for name in [
        "[hH][uU][sS][kK][yY]",
        "[vV][sS][cC][oO][dD][eE]",
        "[eE][nN][vV][rR][cC]",
    ] {
        assert!(deny.contains(name), "{name}: {deny}");
    }
    assert!(deny.contains(r"\.[cC][aA][rR][gG][oO]/"), "{deny}");
    assert!(deny.contains(r"\.[gG][iI][tT]/(("), "{deny}");
    assert!(deny.contains("[hH][oO][oO][kK][sS]") && deny.contains("[cC][lL][aA][uU][dD][eE]"));
    assert!(deny.contains("[aA][gG][eE][nN][tT][sS]") && deny.contains("[zZ][sS][hH][rR][cC]"));
}

#[test]
fn scheme_strings_are_escaped_and_bad_paths_rejected() {
    let profile = seatbelt_profile(&spec(Path::new(r#"/w/a"b\c) (allow default"#), "ls")).unwrap();
    assert!(profile.contains(r#"(subpath "/w/a\"b\\c) (allow default")"#));
    assert!(!profile.contains(r#"c) (allow default" "#));
    let bad = Path::new("/w/a\n(allow default)");
    assert!(seatbelt_profile(&spec(bad, "ls")).is_err());
    assert!(bubblewrap_args(&spec(bad, "ls")).is_err());
    let with_home = SandboxSpec {
        home: Some(Path::new("/h\u{0}")),
        ..spec(Path::new("/w"), "ls")
    };
    assert!(seatbelt_profile(&with_home).is_err());
}

#[test]
fn wrapping_runs_the_users_shell_non_login() {
    let seatbelt = wrap_command(SandboxKind::Seatbelt, &spec(Path::new("/w"), "echo hi")).unwrap();
    assert_eq!(seatbelt.program, "/usr/bin/sandbox-exec");
    assert_eq!(seatbelt.args[0], "-p");
    assert_eq!(seatbelt.args[2..], ["/bin/sh", "-c", "echo hi"]);
    let scratch = Scratch::new("sandbox-wrap");
    let bwrap = wrap_command(SandboxKind::Bubblewrap, &spec(&scratch.0, "echo 'a b'")).unwrap();
    assert_eq!(bwrap.program, "bwrap");
    assert_eq!(
        bwrap.args[bwrap.args.len() - 4..],
        ["--", "/bin/sh", "-c", "echo 'a b'"]
    );
    if !cfg!(windows) {
        let none = wrap_command(SandboxKind::None, &spec(Path::new("/w"), "dir")).unwrap();
        assert_eq!(
            (none.program.as_str(), none.args),
            ("/bin/sh", vec!["-c".to_owned(), "dir".to_owned()])
        );
    }
}

#[test]
fn bubblewrap_mounts_only_what_exists_and_binds_scratch_after_the_data_root() {
    let scratch = Scratch::new("sandbox-bwrap");
    let home = scratch.0.join("home");
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    let data = scratch.0.join("data");
    let tmp = data.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let workspace = scratch.0.join("ws");
    let ws = workspace.to_str().unwrap();
    let spec = SandboxSpec {
        data_root: Some(&data),
        tmp_dir: Some(&tmp),
        home: Some(&home),
        ..spec(&workspace, "ls")
    };
    let joined = bubblewrap_args(&spec).unwrap().join(" ");
    assert!(joined.starts_with(&format!("--ro-bind / / --tmpfs /tmp --bind {ws} {ws}")));
    assert!(joined.contains(&format!("--tmpfs {}/.ssh", home.display())));
    assert!(
        !joined.contains(".aws"),
        "missing credential dirs are skipped"
    );
    let tmpfs = joined.find(&format!("--tmpfs {}", data.display())).unwrap();
    let bind = joined
        .find(&format!("--bind {} {}", tmp.display(), tmp.display()))
        .unwrap();
    assert!(
        tmpfs < bind,
        "scratch must be mounted over the empty data root"
    );
    assert!(
        joined.ends_with("--dev /dev --proc /proc --unshare-pid --unshare-ipc --unshare-uts --new-session --cap-drop ALL --unshare-net --die-with-parent")
    );
    let shared = bubblewrap_args(&SandboxSpec {
        allow_network: true,
        ..spec
    })
    .unwrap();
    assert!(!shared.iter().any(|arg| arg == "--unshare-net"));
}

#[test]
fn bubblewrap_binds_caches_only_in_full_access_and_protects_git() {
    let scratch = Scratch::new("sandbox-bwrap-caches");
    let home = scratch.0.join("home");
    std::fs::create_dir_all(home.join(".cache")).unwrap();
    let workspace = scratch.0.join("ws");
    std::fs::create_dir_all(workspace.join(".git/hooks")).unwrap();
    std::fs::write(workspace.join(".git/config"), "").unwrap();
    let narrow = bubblewrap_args(&SandboxSpec {
        home: Some(&home),
        ..spec(&workspace, "ls")
    })
    .unwrap()
    .join(" ");
    let cache = home.join(".cache");
    let bind = format!("--bind {} {}", cache.display(), cache.display());
    assert!(!narrow.contains(&bind));
    let full = bubblewrap_args(&SandboxSpec {
        home: Some(&home),
        full_access: true,
        ..spec(&workspace, "ls")
    })
    .unwrap()
    .join(" ");
    assert!(full.contains(&bind));
    let ws = workspace.to_str().unwrap();
    for protected in [".git/hooks", ".git/config"] {
        let ro = format!("--ro-bind {ws}/{protected} {ws}/{protected}");
        assert!(full.contains(&ro), "{ro}");
        assert!(full.find(&format!("--bind {ws} {ws}")).unwrap() < full.find(&ro).unwrap());
    }
}

#[cfg(unix)]
fn output_of(code: i32, stderr: &str) -> io::Result<Output> {
    use std::os::unix::process::ExitStatusExt;
    Ok(Output {
        status: std::process::ExitStatus::from_raw(code << 8),
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    })
}

#[cfg(unix)]
#[test]
fn the_probe_classification_needs_the_apply_error_text() {
    let apply = "sandbox-exec: sandbox_apply: Operation not permitted\n";
    let tier = SandboxKind::Seatbelt;
    assert_eq!(
        classify_probe(tier, output_of(0, "")),
        SandboxState::Available
    );
    assert_eq!(
        classify_probe(tier, output_of(71, apply)),
        SandboxState::Nested
    );
    // Exit 71 alone is not a sandbox failure; neither is any other failure.
    assert_eq!(
        classify_probe(tier, output_of(71, "")),
        SandboxState::Missing
    );
    assert_eq!(
        classify_probe(tier, output_of(1, "boom")),
        SandboxState::Missing
    );
    assert_eq!(
        classify_probe(tier, Err(io::ErrorKind::NotFound.into())),
        SandboxState::Missing
    );
    assert_eq!(
        classify_probe(
            SandboxKind::Bubblewrap,
            output_of(1, "bwrap: No permissions to create new namespace\n")
        ),
        SandboxState::Nested
    );
}

#[test]
fn a_call_fails_because_of_the_sandbox_only_with_the_tools_status_text_and_no_other_output() {
    let apply = "sandbox-exec: sandbox_apply: Operation not permitted\n";
    let tier = SandboxKind::Seatbelt;
    assert!(sandbox_apply_failed(tier, Some(71), true, apply));
    assert!(
        !sandbox_apply_failed(tier, Some(1), true, apply),
        "a command that merely prints the text exits with its own status"
    );
    assert!(
        !sandbox_apply_failed(tier, None, true, apply),
        "killed by a signal"
    );
    assert!(
        !sandbox_apply_failed(tier, Some(71), false, apply),
        "the command printed on stdout"
    );
    assert!(
        !sandbox_apply_failed(tier, Some(71), true, ""),
        "exit 71 by itself, nothing on stderr"
    );
    assert!(!sandbox_apply_failed(
        tier,
        Some(71),
        true,
        &format!("{apply}something else\n")
    ));
    assert!(!sandbox_apply_failed(tier, Some(71), true, "usage: tool\n"));
    assert!(!sandbox_apply_failed(
        SandboxKind::None,
        Some(71),
        true,
        apply
    ));
    let bwrap = "bwrap: No permissions to create new namespace\n";
    assert!(sandbox_apply_failed(
        SandboxKind::Bubblewrap,
        Some(1),
        true,
        bwrap
    ));
    assert!(!sandbox_apply_failed(
        SandboxKind::Bubblewrap,
        Some(71),
        true,
        bwrap
    ));
}

#[test]
fn a_missing_tool_is_missing_and_the_status_maps_to_tier_and_label() {
    let status = self_test(
        SandboxKind::Seatbelt,
        Path::new("/nonexistent/sandbox-exec"),
    );
    assert_eq!(
        status,
        SandboxStatus {
            state: SandboxState::Missing,
            tier: SandboxKind::None
        }
    );
    let nested = SandboxStatus {
        state: SandboxState::Nested,
        tier: SandboxKind::Seatbelt,
    };
    assert_eq!(
        (nested.usable(), nested.folder_label()),
        (SandboxKind::None, "nested")
    );
    let available = SandboxStatus {
        state: SandboxState::Available,
        tier: SandboxKind::Bubblewrap,
    };
    assert_eq!(
        (available.usable(), available.folder_label()),
        (SandboxKind::Bubblewrap, "bubblewrap")
    );
    assert_eq!(SandboxState::Missing.recorded(), "unavailable");
    assert_eq!(SandboxState::Available.recorded(), "available");
    assert_eq!(SandboxTag::of_call(true), SandboxTag::SkippedByChoice);
    assert_eq!(SandboxTag::of_call(false), SandboxTag::Sandboxed);
    assert_eq!(
        serde_json::to_string(&nested).unwrap(),
        r#"{"state":"nested","tier":"seatbelt"}"#
    );
}

/// Whether this machine can run Seatbelt (otherwise the integration tests below skip).
fn seatbelt_works() -> bool {
    cfg!(target_os = "macos") && sandbox_status().usable() == SandboxKind::Seatbelt
}

#[cfg(target_os = "macos")]
#[test]
fn launched_inside_another_sandbox_the_state_is_nested() {
    if !seatbelt_works() {
        return;
    }
    let mut command = probe_command(SandboxKind::Seatbelt, Path::new(SANDBOX_EXEC)).unwrap();
    let args: Vec<_> = command.get_args().map(ToOwned::to_owned).collect();
    let mut parent = Command::new(SANDBOX_EXEC);
    parent
        .args(["-p", "(version 1)(allow default)", SANDBOX_EXEC])
        .args(args);
    let result = parent.stdin(Stdio::null()).output();
    assert_eq!(
        classify_probe(SandboxKind::Seatbelt, result),
        SandboxState::Nested
    );
    // And outside the parent the same probe applies.
    assert_eq!(
        classify_probe(SandboxKind::Seatbelt, command.output()),
        SandboxState::Available
    );
}

/// A world for the real-sandbox probes: a data folder with a database stand-in and a per-session
/// temp folder inside it, a fake home with credential folders, a git folder.
struct World {
    scratch: Scratch,
    data: PathBuf,
    tmp: PathBuf,
    home: PathBuf,
    ws: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        let scratch = Scratch::new(tag);
        let (data, home, ws) = (
            scratch.0.join("data"),
            scratch.0.join("home"),
            scratch.0.join("ws"),
        );
        let tmp = data.join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::create_dir_all(ws.join(".git/hooks")).unwrap();
        std::fs::write(data.join("pingdeck.sqlite3"), "stand-in").unwrap();
        std::fs::write(home.join(".ssh/id_ed25519"), "key").unwrap();
        Self {
            scratch,
            data,
            tmp,
            home,
            ws,
        }
    }

    fn spec<'a>(&'a self, command: &'a str) -> SandboxSpec<'a> {
        SandboxSpec {
            data_root: Some(&self.data),
            tmp_dir: Some(&self.tmp),
            home: Some(&self.home),
            ..spec(&self.ws, command)
        }
    }

    /// Runs `command` as the shell tool would and returns (success, stdout, stderr).
    fn run(&self, kind: SandboxKind, spec: &SandboxSpec<'_>) -> (bool, String, String) {
        let wrapped = wrap_command(kind, spec).unwrap();
        let output = Command::new(&wrapped.program)
            .args(&wrapped.args)
            .current_dir(&self.ws)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", &self.home)
            .env("TMPDIR", &self.tmp)
            .env("DATA", &self.data)
            .env("FOLDER", &self.ws)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn sandboxed(&self, command: &str) -> (bool, String, String) {
        self.run(SandboxKind::Seatbelt, &self.spec(command))
    }

    fn skipped(&self, command: &str) -> (bool, String, String) {
        self.run(SandboxKind::None, &self.spec(command))
    }
}

#[cfg(target_os = "macos")]
#[test]
fn reads_of_the_data_folder_and_credentials_fail_inside_and_succeed_outside() {
    if !seatbelt_works() {
        return;
    }
    let world = World::new("reads");
    for command in [r#"cat "$DATA/pingdeck.sqlite3""#, "ls ~/.ssh"] {
        let (ok, _, err) = world.sandboxed(command);
        assert!(
            !ok && err.contains("Operation not permitted"),
            "{command}: {err}"
        );
        let (ok, out, err) = world.skipped(command);
        assert!(ok, "{command} outside the sandbox: {err}");
        assert!(!out.is_empty());
    }
    // The per-session temp folder below the data folder is re-allowed and reads back.
    let (ok, out, err) = world.sandboxed(r#"echo scratch > "$TMPDIR/x" && cat "$TMPDIR/x""#);
    assert!(ok && out.trim() == "scratch", "{err}");
    let (ok, _, _) = world.skipped(r#"echo scratch > "$TMPDIR/y" && cat "$TMPDIR/y""#);
    assert!(ok);
    // Full access keeps every read-deny.
    let mut spec = world.spec(r#"cat "$DATA/pingdeck.sqlite3""#);
    spec.full_access = true;
    spec.allow_network = true;
    assert!(!world.run(SandboxKind::Seatbelt, &spec).0);
}

#[cfg(target_os = "macos")]
#[test]
fn writes_are_confined_to_the_folder_and_git_hooks_and_config_stay_closed() {
    if !seatbelt_works() {
        return;
    }
    let world = World::new("writes");
    let (ok, _, err) = world.sandboxed(r#"touch "$FOLDER/a""#);
    assert!(ok, "{err}");
    for command in [
        r#"touch "$HOME/pd-escape""#,
        r#"echo x > "$FOLDER/.git/hooks/pre-commit""#,
        r#"echo x > "$FOLDER/.git/config""#,
        r#"touch "$HOME/Library-Caches-x""#,
    ] {
        let (ok, _, err) = world.sandboxed(command);
        assert!(
            !ok && err.contains("Operation not permitted"),
            "{command}: {err}"
        );
        let (ok, _, err) = world.skipped(command);
        assert!(ok, "{command} outside the sandbox: {err}");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_folder_reached_through_a_symlink_is_writable() {
    if !seatbelt_works() {
        return;
    }
    let world = World::new("symlink");
    let link = world.scratch.0.join("linked");
    std::os::unix::fs::symlink(&world.ws, &link).unwrap();
    let spec = SandboxSpec {
        workspace: &link,
        ..world.spec(r#"touch "$FOLDER/b" && test -f b"#)
    };
    let (ok, _, err) = world.run(SandboxKind::Seatbelt, &spec);
    assert!(ok, "{err}");
}

#[cfg(target_os = "macos")]
#[test]
fn the_pasteboard_is_closed_inside_the_sandbox() {
    if !seatbelt_works() {
        return;
    }
    let world = World::new("pasteboard");
    // `pbpaste` succeeds outside whatever the pasteboard holds; the denied lookup fails it.
    let (outside, _, _) = world.skipped("pbpaste");
    let (inside, out, _) = world.sandboxed("pbpaste");
    if outside {
        assert!(
            !inside && out.is_empty(),
            "pbpaste printed {out:?} inside the sandbox"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn the_network_is_off_unless_allowed_and_on_in_full_access() {
    if !seatbelt_works() {
        return;
    }
    let world = World::new("network");
    let curl = "curl -sI --max-time 8 https://example.com";
    let (reachable, out, _) = world.skipped(curl);
    if !reachable || !out.starts_with("HTTP/") {
        return; // offline machine: nothing to compare against
    }
    assert!(
        !world.sandboxed(curl).0,
        "no network in the default profile"
    );
    let mut allowed_once = world.spec(curl);
    allowed_once.allow_network = true;
    let (ok, out, err) = world.run(SandboxKind::Seatbelt, &allowed_once);
    assert!(ok && out.starts_with("HTTP/"), "{err}");
    let mut full = world.spec(curl);
    full.allow_network = true;
    full.full_access = true;
    let (ok, out, err) = world.run(SandboxKind::Seatbelt, &full);
    assert!(ok && out.starts_with("HTTP/"), "{err}");
}

// ---- protected workspace paths, symlinked credentials, process list, bubblewrap isolation ----

/// The regex of one `(regex #"…")` rule.
fn regex_of(rule: &str) -> regex::Regex {
    let body = rule
        .strip_prefix("(regex #\"")
        .and_then(|rest| rest.strip_suffix("\")"))
        .unwrap();
    regex::Regex::new(body).unwrap()
}

#[test]
fn the_write_deny_patterns_cover_nested_repositories_agent_folders_and_startup_files() {
    let rules = protect::seatbelt_regexes("/w");
    let matches = |path: &str| rules.iter().any(|rule| regex_of(rule).is_match(path));
    for denied in [
        "/w/.git/hooks",
        "/w/.git/hooks/pre-commit",
        "/w/.git/config",
        "/w/.git/config.worktree",
        "/w/sub/deep/.git/hooks/post-merge",
        "/w/sub/.git/config",
        "/w/.git/modules/x/hooks/pre-push",
        "/w/.git/modules/x/modules/y/config",
        "/w/.git/worktrees/wt/config.worktree",
        "/w/.GIT/HOOKS/pre-commit",
        "/w/.claude/skills/x/SKILL.md",
        "/w/.CLAUDE",
        "/w/pkg/.agents/a.md",
        "/w/.zshrc",
        "/w/pkg/.bashrc",
        "/w/.ZSHRC",
        "/w/.profile",
    ] {
        assert!(matches(denied), "{denied} must be write-denied");
    }
    for allowed in [
        "/w/.git/HEAD",
        "/w/.git/refs/heads/main",
        "/w/.git/objects/ab/cdef",
        "/w/.gitignore",
        "/w/.gitmodules",
        "/w/hooks/x",
        "/w/src/config",
        "/w/config",
        "/w/.zshrc.bak",
        "/w/.claude-notes",
        "/w/notes/agents",
        "/w2/.zshrc",
        "/other/.git/hooks/x",
    ] {
        assert!(!matches(allowed), "{allowed} must stay writable");
    }
    // A workspace path cannot add regex syntax or leave the string literal.
    for rule in protect::seatbelt_regexes(r#"/w/a"b\c) (allow default.+"#) {
        let body = rule
            .strip_prefix("(regex #\"")
            .unwrap()
            .strip_suffix("\")")
            .unwrap();
        assert!(!body.contains('"') && !body.contains("(allow"), "{body}");
        regex::Regex::new(body).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn bubblewrap_binds_nested_repositories_agent_folders_and_startup_files_read_only() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new("sandbox-bwrap-protect");
    let ws = scratch.0.join("ws");
    for dir in [
        ".git/hooks",
        ".git/modules/m/hooks",
        ".git/modules/m/modules/n/hooks",
        "sub/deep/.git/hooks",
        ".claude/skills",
        ".AGENTS",
        "node_modules/x/.git/hooks",
    ] {
        std::fs::create_dir_all(ws.join(dir)).unwrap();
    }
    for file in [
        ".git/config",
        ".git/modules/m/config",
        "sub/deep/.git/config.worktree",
        "pkg/.zshrc",
    ] {
        std::fs::create_dir_all(ws.join(file).parent().unwrap()).unwrap();
        std::fs::write(ws.join(file), "").unwrap();
    }
    symlink(ws.join("sub"), ws.join("link-to-sub")).unwrap();
    let args = bubblewrap_args(&spec(&ws, "ls")).unwrap().join(" ");
    for protected in [
        ".git/hooks",
        ".git/config",
        ".git/modules/m/hooks",
        ".git/modules/m/config",
        ".git/modules/m/modules/n/hooks",
        "sub/deep/.git/hooks",
        "sub/deep/.git/config.worktree",
        ".claude",
        ".AGENTS",
        "pkg/.zshrc",
    ] {
        let path = ws.join(protected);
        let ro = format!("--ro-bind {} {}", path.display(), path.display());
        assert!(args.contains(&ro), "{ro}\n{args}");
    }
    assert!(!args.contains("node_modules"), "build output is not walked");
    assert!(!args.contains("link-to-sub"), "a symlink is never followed");
}

#[cfg(unix)]
#[test]
fn a_symlinked_credential_entry_is_hidden_under_both_spellings() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new("sandbox-deny-link");
    let home = scratch.0.join("home");
    let cloud = scratch.0.join("cloud/ssh");
    let real_rc = scratch.0.join("dotfiles/npmrc");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cloud).unwrap();
    std::fs::create_dir_all(real_rc.parent().unwrap()).unwrap();
    std::fs::write(&real_rc, "token").unwrap();
    symlink(&cloud, home.join(".ssh")).unwrap();
    symlink(&real_rc, home.join(".npmrc")).unwrap();
    let workspace = scratch.0.join("ws");
    let spec = SandboxSpec {
        home: Some(&home),
        ..spec(&workspace, "ls")
    };
    let denied = read_deny(&spec);
    for entry in [
        (home.join(".ssh"), true),
        (cloud.clone(), true),
        (home.join(".npmrc"), false),
        (real_rc.clone(), false),
    ] {
        assert!(denied.contains(&entry), "{entry:?} in {denied:?}");
    }
    let profile = seatbelt_profile(&spec).unwrap();
    assert!(profile.contains(&format!("(subpath \"{}\")", cloud.display())));
    assert!(profile.contains(&format!("(literal \"{}\")", real_rc.display())));
    // `--tmpfs` on a symlink is undefined: bwrap hides the targets and never the links.
    let args = bubblewrap_args(&spec).unwrap().join(" ");
    assert!(args.contains(&format!("--tmpfs {}", cloud.display())));
    assert!(args.contains(&format!("--ro-bind /dev/null {}", real_rc.display())));
    assert!(!args.contains(&format!("--tmpfs {}/.ssh", home.display())));
    assert!(!args.contains(&format!("--ro-bind /dev/null {}/.npmrc", home.display())));
}

#[test]
fn bubblewrap_cuts_host_ipc_the_terminal_session_and_capabilities() {
    let scratch = Scratch::new("sandbox-bwrap-ipc");
    let args = bubblewrap_args(&spec(&scratch.0, "ls")).unwrap();
    for flag in [
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--new-session",
        "--die-with-parent",
    ] {
        assert!(args.iter().any(|arg| arg == flag), "{flag}");
    }
    let drop = args.iter().position(|arg| arg == "--cap-drop").unwrap();
    assert_eq!(args[drop + 1], "ALL");
    // Where the user's runtime directory exists, an empty tmpfs replaces it (no session bus,
    // keyring or gpg-agent socket).
    if let Some(run_user) = run_user_dir().filter(|dir| dir.is_dir()) {
        let tmpfs = format!("--tmpfs {}", run_user.display());
        assert!(args.join(" ").contains(&tmpfs), "{tmpfs}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn the_session_bus_is_absent_inside_bubblewrap() {
    let Some(run_user) = run_user_dir().filter(|dir| dir.join("bus").exists()) else {
        return; // no session bus on this machine: nothing to compare
    };
    if sandbox_status().usable() != SandboxKind::Bubblewrap {
        return;
    }
    let scratch = Scratch::new("sandbox-bwrap-bus");
    let command = format!("test -e {}/bus", run_user.display());
    let wrapped = wrap_command(SandboxKind::Bubblewrap, &spec(&scratch.0, &command)).unwrap();
    let status = Command::new(&wrapped.program)
        .args(&wrapped.args)
        .stdin(Stdio::null())
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "the bus socket is reachable inside the sandbox"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn nested_repositories_agent_folders_and_startup_files_stay_closed() {
    if !seatbelt_works() {
        return;
    }
    let world = World::new("protect");
    for dir in [
        "sub/.git/hooks",
        ".git/modules/x/hooks",
        ".claude/skills/x",
        ".agents",
    ] {
        std::fs::create_dir_all(world.ws.join(dir)).unwrap();
    }
    std::fs::write(world.ws.join("sub/.git/config"), "").unwrap();
    // What the sandbox must still allow.
    for command in [
        r#"echo x > "$FOLDER/sub/src.txt""#,
        r#"echo x > "$FOLDER/.git/HEAD""#,
        r#"echo x > "$FOLDER/.gitignore""#,
        r#"mkdir -p "$FOLDER/new/src" && echo x > "$FOLDER/new/src/a.rs""#,
    ] {
        let (ok, _, err) = world.sandboxed(command);
        assert!(ok, "{command}: {err}");
    }
    for command in [
        r#"echo x > "$FOLDER/sub/.git/hooks/pre-commit""#,
        r#"echo x > "$FOLDER/sub/.git/config""#,
        r#"echo x > "$FOLDER/.git/modules/x/hooks/post-merge""#,
        r#"mkdir -p "$FOLDER/fresh/.git/hooks""#,
        r#"echo x > "$FOLDER/.claude/skills/x/SKILL.md""#,
        r#"echo x > "$FOLDER/.agents/agent.md""#,
        r#"echo x > "$FOLDER/.zshrc""#,
        r#"echo x > "$FOLDER/sub/.bashrc""#,
        r#"echo x > "$FOLDER/.ZSHRC""#,
    ] {
        let (ok, _, err) = world.sandboxed(command);
        assert!(
            !ok && err.contains("Operation not permitted"),
            "{command}: {err}"
        );
        let (ok, _, err) = world.skipped(command);
        assert!(ok, "{command} outside the sandbox: {err}");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_symlinked_credential_folder_cannot_be_read_inside_the_sandbox() {
    use std::os::unix::fs::symlink;
    if !seatbelt_works() {
        return;
    }
    let world = World::new("symlinked-ssh");
    let cloud = world.scratch.0.join("cloud-ssh");
    std::fs::create_dir_all(&cloud).unwrap();
    std::fs::write(cloud.join("id_ed25519"), "key").unwrap();
    std::fs::remove_dir_all(world.home.join(".ssh")).unwrap();
    symlink(&cloud, world.home.join(".ssh")).unwrap();
    let command = "cat ~/.ssh/id_ed25519";
    let (ok, _, err) = world.sandboxed(command);
    assert!(!ok && err.contains("Operation not permitted"), "{err}");
    let (ok, out, err) = world.skipped(command);
    assert!(ok && out == "key", "{err}");
}

#[cfg(target_os = "macos")]
#[test]
fn the_process_list_of_the_machine_is_closed_inside_the_sandbox() {
    if !seatbelt_works() {
        return;
    }
    let world = World::new("procs");
    let count = "/usr/bin/pgrep -f . | wc -l";
    let (_, outside, _) = world.skipped(count);
    if outside.trim().parse::<usize>().unwrap_or(0) < 5 {
        return; // `pgrep` cannot list processes here (a restricted host): nothing to compare
    }
    let (_, inside, _) = world.sandboxed(count);
    assert!(
        inside.trim().parse::<usize>().unwrap() <= 2,
        "the sandbox listed {} processes",
        inside.trim()
    );
    // Its own children are still visible to it, and `kill` of its own process works.
    let (ok, _, err) = world.sandboxed("sleep 5 & kill $! && wait; true");
    assert!(ok, "{err}");
}
