//! Tests of the `shell` tool: pure helpers (truncation, throttling, environment, workspace
//! validation), real processes (unix) and the macOS Seatbelt sandbox.

use serde_json::json;

use std::path::PathBuf;
use std::sync::Mutex;

use super::*;
use crate::agent::tools::TestParts;
use crate::app::AppState;

const CAP: usize = 100 * 1024;
const HEAD_BYTES: usize = CAP / 2;
const TAIL_BYTES: usize = CAP - HEAD_BYTES;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "pingdeck-shell-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(std::fs::canonicalize(dir).unwrap())
    }

    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.0.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn env_vars() -> Vec<(OsString, OsString)> {
    scrub_env(std::env::vars_os(), None, &[], false)
}

async fn run(
    command: &str,
    workspace: &Path,
    kind: SandboxKind,
    limit: Duration,
    cancel: &CancelToken,
) -> (ToolOutput, Vec<(OutputStream, String)>) {
    run_with(
        command,
        workspace,
        kind,
        limit,
        false,
        &HostPaths::default(),
        &env_vars(),
        cancel,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_with(
    command: &str,
    workspace: &Path,
    kind: SandboxKind,
    limit: Duration,
    allow_network: bool,
    host: &HostPaths,
    env: &[(OsString, OsString)],
    cancel: &CancelToken,
) -> (ToolOutput, Vec<(OutputStream, String)>) {
    let events = Mutex::new(Vec::new());
    let sink =
        |stream: OutputStream, text: &str| events.lock().unwrap().push((stream, text.to_owned()));
    let request = RunRequest {
        command,
        workspace,
        kind,
        timeout: limit,
        allow_network,
        full_access: false,
        host,
        env,
        output_cap: CAP,
        ledger: None,
        label: "c",
    };
    let output = run_command(&request, cancel, Some(&sink), None).await;
    (output, events.into_inner().unwrap())
}

#[test]
fn pure_helpers_handle_utf8_capture_lane_workspace_and_args() {
    assert_eq!(incomplete_tail_len(b""), 0);
    assert_eq!(incomplete_tail_len(&"€".as_bytes()[..2]), 2);
    assert_eq!(incomplete_tail_len("€".as_bytes()), 0);
    assert_eq!(incomplete_tail_len(&[0xFF]), 0);

    let mut capture = Capture::new(CAP);
    let head_pad = "a".repeat(HEAD_BYTES - 1);
    let text = format!(
        "{head_pad}€{}€{}",
        "b".repeat(200_000),
        "c".repeat(TAIL_BYTES - 2)
    );
    capture.push(&text);
    let rendered = capture.render();
    assert!(!rendered.contains('\u{FFFD}') && rendered.contains("[output truncated:"));

    let start = Instant::now();
    let mut lane = Lane::new();
    assert_eq!(lane.push(b"one", start).as_deref(), Some("one"));
    assert_eq!(lane.push(b"two", start + Duration::from_millis(5)), None);
    assert_eq!(lane.tick(start + FLUSH_INTERVAL).as_deref(), Some("two"));
    let mut split_lane = Lane::new();
    let euro = "€".as_bytes();
    assert_eq!(split_lane.push(&euro[..2], start), None);
    assert_eq!(
        split_lane
            .push(&euro[2..], start + Duration::from_millis(1))
            .as_deref(),
        Some("€")
    );
    assert_eq!(
        lane.push(
            &vec![b'x'; FLUSH_BYTES],
            start + FLUSH_INTERVAL + Duration::from_millis(2)
        )
        .unwrap()
        .len(),
        FLUSH_BYTES
    );
    lane.push(
        &euro[..1],
        start + FLUSH_INTERVAL + Duration::from_millis(3),
    );
    assert_eq!(
        lane.finish(start + FLUSH_INTERVAL + Duration::from_millis(4))
            .as_deref(),
        Some("\u{FFFD}")
    );

    let scratch = Scratch::new("ws");
    let ws = scratch.dir("ws");
    let host = HostPaths::default();
    assert_eq!(validate_workspace(Some(&ws), &host).unwrap(), ws);
    assert!(validate_workspace(None, &host).is_err());
    assert!(validate_workspace(Some(Path::new("/")), &host).is_err());
    let home = HostPaths {
        home: Some(ws.clone()),
        ..HostPaths::default()
    };
    assert!(validate_workspace(Some(&ws), &home).is_err());
    #[cfg(unix)]
    {
        let link = scratch.0.join("link");
        std::os::unix::fs::symlink(&ws, &link).unwrap();
        assert!(validate_workspace(Some(&link), &home).is_err());
    }

    let max = Duration::from_secs(120);
    assert_eq!(effective_timeout(Some(0), max), Duration::from_secs(1));
    assert_eq!(effective_timeout(Some(9999), max), max);
    assert!(ShellArgs::parse(&json!({"command": "  "})).is_err());
    assert_eq!(
        ShellArgs::parse(&json!({"command": "ls", "timeout_s": 5}))
            .unwrap()
            .timeout_s,
        Some(5)
    );
}

struct Harness {
    state: AppState,
    settings: Settings,
    cancel: CancelToken,
    parts: TestParts,
}

impl Harness {
    fn new() -> Self {
        Self {
            state: AppState::open_in_memory().unwrap(),
            settings: Settings::default(),
            cancel: CancelToken::new(),
            parts: TestParts::default(),
        }
    }

    fn ctx<'a>(&'a self, workspace: Option<&'a Path>, sandbox: SandboxKind) -> ToolContext<'a> {
        ToolContext {
            workspace_dir: workspace,
            sandbox,
            unsandboxed_approved: true,
            ..ToolContext::for_test(
                Some(&self.state as &(dyn std::any::Any + Send + Sync)),
                &self.settings,
                &self.cancel,
                &self.parts,
            )
        }
    }
}

#[tokio::test]
async fn inspect_and_call_validate_workspace_and_run_commands() {
    let scratch = Scratch::new("inspect");
    let ws = scratch.dir("ws");
    let harness = Harness::new();
    let tool = ShellTool::new();
    let args = json!({"command": "rm -rf build"});

    let sandboxed = tool
        .inspect(&harness.ctx(Some(&ws), SandboxKind::Seatbelt), &args)
        .unwrap();
    assert!(!sandboxed.read_only && sandboxed.destructive);
    assert_eq!(
        sandboxed.preview,
        Some(ToolApprovalPreview::Command {
            cmd: "rm -rf build".to_owned(),
            cwd: ws.to_string_lossy().into_owned(),
        })
    );
    assert!(
        tool.inspect(&harness.ctx(None, SandboxKind::None), &args)
            .is_err()
    );

    let refused = tool
        .call(
            &harness.ctx(Some(Path::new("/")), SandboxKind::None),
            json!({"command": "echo no"}),
        )
        .await;
    assert!(refused.is_error);

    #[cfg(unix)]
    {
        let ok = tool
            .call(
                &harness.ctx(Some(&ws), SandboxKind::None),
                json!({"command": "pwd; echo hi"}),
            )
            .await;
        assert!(!ok.is_error && ok.text_content().ends_with("hi\n[exit code: 0]"));
        assert_eq!(ok.exit_code, Some(0));
    }
}

#[cfg(unix)]
fn group_alive(pgid: u32) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", "--", &format!("-{pgid}")])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
async fn assert_group_gone(pgid: u32) {
    for _ in 0..1500 {
        if !group_alive(pgid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("process group {pgid} survived");
}

#[cfg(unix)]
#[tokio::test]
async fn unix_execution_streams_truncates_scrubs_env_and_kills_process_groups() {
    let scratch = Scratch::new("unix");
    let ws = scratch.dir("ws");
    let cancel = CancelToken::new();
    let limit = Duration::from_secs(10);

    let (failed, _) = run("echo bad; exit 1", &ws, SandboxKind::None, limit, &cancel).await;
    assert!(failed.is_error && failed.text_content() == "bad\n[exit code: 1]");
    assert_eq!(failed.exit_code, Some(1));

    let (mixed, _) = run(
        "printf 'a\\377b'; echo err >&2",
        &ws,
        SandboxKind::None,
        limit,
        &cancel,
    )
    .await;
    assert!(mixed.text_content().contains("a\u{FFFD}b") && mixed.text_content().contains("err"));

    // Orphan background child holding pipe does not hang and is killed.
    let (orphan, _) = run(
        "echo $$; sleep 30 &",
        &ws,
        SandboxKind::None,
        limit,
        &cancel,
    )
    .await;
    assert!(!orphan.is_error);
    let orphan_pgid: u32 = orphan
        .text_content()
        .lines()
        .next()
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_group_gone(orphan_pgid).await;

    // Timeout kills the process group.
    let (timed_out, _) = run(
        "echo $$; sleep 30",
        &ws,
        SandboxKind::None,
        Duration::from_millis(400),
        &cancel,
    )
    .await;
    assert!(timed_out.is_error && timed_out.text_content().contains("timed out"));
    assert_eq!(timed_out.exit_code, None);
    let timeout_pgid: u32 = timed_out
        .text_content()
        .lines()
        .next()
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_group_gone(timeout_pgid).await;

    // Env scrubbing drops secrets and honours scratch TMPDIR.
    let tmp = scratch.dir("tmp");
    let planted = [("OPENAI_API_KEY", "leak-2"), ("GITHUB_TOKEN", "leak-3")]
        .map(|(n, v)| (OsString::from(n), OsString::from(v)));
    let env = scrub_env(std::env::vars_os().chain(planted), Some(&tmp), &[], false);
    let (env_out, _) = run_with(
        "env",
        &ws,
        SandboxKind::None,
        limit,
        false,
        &HostPaths::default(),
        &env,
        &cancel,
    )
    .await;
    assert!(
        !env_out.text_content().contains("leak-")
            && env_out.text_content().contains(tmp.to_str().unwrap())
    );
}

#[cfg(target_os = "macos")]
async fn seatbelt_available(ws: &Path) -> bool {
    let cancel = CancelToken::new();
    let (output, _) = run(
        "true",
        ws,
        SandboxKind::Seatbelt,
        Duration::from_secs(10),
        &cancel,
    )
    .await;
    !output.is_error
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn seatbelt_confines_writes_hides_credentials_and_explains_denials() {
    let scratch = Scratch::new("sb");
    let ws = scratch.dir("ws");
    std::fs::create_dir_all(ws.join(".git/hooks")).unwrap();
    if !seatbelt_available(&ws).await {
        return;
    }
    let data = scratch.dir("data");
    std::fs::write(data.join("secret.sqlite3"), "top secret").unwrap();
    let tmp = scratch.dir("data/tmp");
    let host = HostPaths {
        home: Some(scratch.dir("home")),
        data_root: Some(data.clone()),
        tmp_dir: Some(tmp.clone()),
    };
    std::fs::create_dir_all(host.home.as_ref().unwrap().join(".ssh")).unwrap();
    std::fs::write(host.home.as_ref().unwrap().join(".ssh/id"), "key").unwrap();
    let cancel = CancelToken::new();

    let command = format!(
        "echo inside > inside.txt; cat '{data}/secret.sqlite3' || echo no-secret; \
         cat '{home}/.ssh/id' || echo no-ssh; echo scratch > '{tmp}/x' && cat '{tmp}/x'",
        data = data.display(),
        home = host.home.as_ref().unwrap().display(),
        tmp = tmp.display()
    );
    let (output, _) = run_with(
        &command,
        &ws,
        SandboxKind::Seatbelt,
        Duration::from_secs(20),
        false,
        &host,
        &env_vars(),
        &cancel,
    )
    .await;
    let text = output.text_content();
    assert!(!text.contains("top secret") && text.contains("no-secret"));
    assert!(!text.contains("key\n") && text.contains("no-ssh") && text.contains("scratch"));
    assert_eq!(
        std::fs::read_to_string(ws.join("inside.txt")).unwrap(),
        "inside\n"
    );

    // Denial outside workspace gets SANDBOX_HINT; .git/hooks inside workspace does not; exit 71 is not a sandbox failure.
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let outside = home.join(format!(".pingdeck-hint-probe-{}", std::process::id()));
    let (out_deny, _) = run(
        &format!("touch '{}'", outside.display()),
        &ws,
        SandboxKind::Seatbelt,
        Duration::from_secs(10),
        &cancel,
    )
    .await;
    let _ = std::fs::remove_file(&outside);
    assert!(out_deny.text_content().contains(SANDBOX_HINT));

    let (hook_deny, _) = run(
        "echo x > .git/hooks/pre-commit",
        &ws,
        SandboxKind::Seatbelt,
        Duration::from_secs(10),
        &cancel,
    )
    .await;
    assert!(!hook_deny.text_content().contains(SANDBOX_HINT));

    let (exit71, _) = run(
        "echo about to fail; exit 71",
        &ws,
        SandboxKind::Seatbelt,
        Duration::from_secs(10),
        &cancel,
    )
    .await;
    assert!(
        exit71.text_content().contains("[exit code: 71]")
            && !exit71
                .text_content()
                .contains("sandbox could not be applied")
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn seatbelt_denies_the_network_unless_allowed() {
    let scratch = Scratch::new("sb-net");
    let ws = scratch.dir("ws");
    if !seatbelt_available(&ws).await || !Path::new("/usr/bin/nc").is_file() {
        return;
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let command = format!("/usr/bin/nc -z -w 2 127.0.0.1 {port} && echo connected || echo blocked");
    let cancel = CancelToken::new();
    let host = HostPaths::default();
    let env = env_vars();
    let limit = Duration::from_secs(20);

    let (denied, _) = run_with(
        &command,
        &ws,
        SandboxKind::Seatbelt,
        limit,
        false,
        &host,
        &env,
        &cancel,
    )
    .await;
    assert!(denied.text_content().contains("blocked"));
    let (allowed, _) = run_with(
        &command,
        &ws,
        SandboxKind::Seatbelt,
        limit,
        true,
        &host,
        &env,
        &cancel,
    )
    .await;
    assert!(allowed.text_content().contains("connected"));
    drop(listener);
}
