//! Tests of the file tools: path confinement, protected paths, every tool's behaviour and caps.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use super::paths::{approval_reason, canonical_workspace};
use super::*;
use crate::agent::runtime::AgentRuntime;
use crate::agent::test_dir::TempDir;
use crate::agent::tools::{FileChangeKind, TestParts};
use spanbay_llm::llm::hub::{HubDocumentSource, HubImage, HubPart};
use spanbay_model::settings::Settings;

/// A workspace, an in-memory state and a cancel token.
struct Fixture {
    dir: TempDir,
    state: AgentRuntime,
    settings: Arc<Settings>,
    cancel: CancelToken,
    workspace: bool,
    parts: TestParts,
    /// Record the current content of a file as read before every write/edit call (the guard
    /// has its own tests that turn this off).
    auto_read: bool,
}

impl Fixture {
    fn new() -> Self {
        let state = AgentRuntime::for_test();
        Self {
            dir: TempDir::new("fs"),
            settings: std::sync::Arc::new(Settings::default()),
            state,
            cancel: CancelToken::new(),
            workspace: true,
            parts: TestParts::default(),
            auto_read: true,
        }
    }

    fn ctx(&self) -> ToolContext<'_> {
        ToolContext {
            workspace_dir: self.workspace.then(|| self.dir.path()),
            ..ToolContext::for_test(
                Some(&self.state as &(dyn std::any::Any + Send + Sync)),
                &self.settings,
                &self.cancel,
                &self.parts,
            )
        }
    }

    fn tool(name: &str) -> Arc<dyn AgentTool> {
        let mut registry = ToolRegistry::new();
        register(&mut registry);
        registry.get(name).unwrap().clone()
    }

    fn inspect(&self, name: &str, arguments: Value) -> Result<CallInspection, AppError> {
        Self::tool(name).inspect(&self.ctx(), &arguments)
    }

    async fn call(&self, name: &str, arguments: Value) -> ToolOutput {
        if self.auto_read
            && matches!(name, "write_file" | "edit_file")
            && let Some(rel) = arguments["path"].as_str()
            && let Ok(path) = fs::canonicalize(self.dir.path().join(rel))
            && let Ok(bytes) = fs::read(&path)
        {
            self.parts
                .read_hashes
                .record("s", &path, spanbay_base::crypto::sha256_hex(&bytes));
        }
        Self::tool(name).call(&self.ctx(), arguments).await
    }

    /// A successful call's text.
    async fn ok(&self, name: &str, arguments: Value) -> String {
        let output = self.call(name, arguments).await;
        assert!(!output.is_error, "{name} failed: {}", output.text_content());
        output.text_content()
    }

    /// A failed call's text.
    async fn err(&self, name: &str, arguments: Value) -> String {
        let output = self.call(name, arguments).await;
        assert!(
            output.is_error,
            "{name} succeeded: {}",
            output.text_content()
        );
        output.text_content()
    }
}

fn is_validation(result: &Result<PathBuf, AppError>) -> bool {
    matches!(result, Err(AppError::Validation { .. }))
}

#[test]
fn registers_the_eight_tools_with_hints() {
    let mut registry = ToolRegistry::new();
    register(&mut registry);
    let specs = registry.specs();
    let names: Vec<_> = specs.iter().map(|spec| spec.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "read_file",
            "write_file",
            "edit_file",
            "delete_file",
            "move_file",
            "list_dir",
            "glob",
            "grep"
        ]
    );
    for spec in &specs {
        let changes = matches!(
            spec.name.as_str(),
            "write_file" | "edit_file" | "delete_file" | "move_file"
        );
        assert_eq!(spec.read_only, !changes, "{}", spec.name);
        assert_eq!(spec.destructive, changes, "{}", spec.name);
        assert_eq!(spec.input_schema["type"], "object");
    }
}

// ---- path confinement -------------------------------------------------------------------------

#[test]
fn traversal_and_absolute_paths_outside_the_workspace_are_rejected() {
    let dir = TempDir::new("fs");
    let workspace = dir.path().join("ws");
    fs::create_dir(&workspace).unwrap();
    dir.file("secret.txt", "s");
    for candidate in [
        "../../etc/passwd",
        "../secret.txt",
        "sub/../../secret.txt",
        "/etc/passwd",
    ] {
        assert!(
            is_validation(&resolve_workspace_path(&workspace, candidate, false)),
            "{candidate}"
        );
        assert!(
            is_validation(&resolve_workspace_path(&workspace, candidate, true)),
            "{candidate}"
        );
    }
    let outside = dir.path().join("secret.txt");
    assert!(is_validation(&resolve_workspace_path(
        &workspace,
        outside.to_str().unwrap(),
        false
    )));
}

#[test]
fn relative_and_inside_absolute_paths_resolve_below_the_workspace() {
    let dir = TempDir::new("fs");
    let file = dir.file("a/b.txt", "x");
    let resolved = resolve_workspace_path(dir.path(), "a/../a/./b.txt", false).unwrap();
    assert_eq!(resolved, file);
    let absolute = resolve_workspace_path(dir.path(), file.to_str().unwrap(), false).unwrap();
    assert_eq!(absolute, file);
    // A file that does not exist yet resolves for a write, not for a read.
    let new = resolve_workspace_path(dir.path(), "new/dir/c.txt", true).unwrap();
    assert_eq!(new, dir.path().join("new/dir/c.txt"));
    assert!(is_validation(&resolve_workspace_path(
        dir.path(),
        "new/dir/c.txt",
        false
    )));
    assert!(is_validation(&resolve_workspace_path(
        dir.path(),
        "a\0b",
        true
    )));
}

#[cfg(unix)]
#[test]
fn symlinks_pointing_outside_are_rejected_for_reads_and_creates() {
    use std::os::unix::fs::symlink;
    let dir = TempDir::new("fs");
    let outside = dir.path().join("outside");
    let workspace = dir.path().join("ws");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    fs::write(outside.join("secret.txt"), "s").unwrap();
    symlink(&outside, workspace.join("link")).unwrap();
    symlink(outside.join("secret.txt"), workspace.join("file-link")).unwrap();
    symlink(outside.join("missing"), workspace.join("dangling")).unwrap();
    for (candidate, for_write) in [
        ("link/secret.txt", false),
        ("file-link", false),
        ("link/new.txt", true),
        ("link/new/deeper.txt", true),
        ("dangling", true),
        ("file-link", true),
    ] {
        assert!(
            is_validation(&resolve_workspace_path(&workspace, candidate, for_write)),
            "{candidate} write={for_write}"
        );
    }
    assert!(!outside.join("new.txt").exists());
}

/// Undo and the change diff run from user commands, which show the error as text: no tool-result code line.
#[cfg(unix)]
#[test]
fn user_driven_refusals_carry_no_tool_code_line() {
    use std::os::unix::fs::symlink;
    let dir = TempDir::new("fs");
    let outside = dir.path().join("outside");
    let workspace = dir.path().join("ws");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    symlink(&outside, workspace.join("link")).unwrap();
    let undo = undo_file_change(&workspace, "link/new.txt", None, Some(""), true).unwrap_err();
    assert!(matches!(undo, AppError::Validation { .. }));
    assert!(!undo.message().starts_with('{'), "{undo:?}");
    assert!(undo.message().contains("outside the workspace"), "{undo:?}");
    let diff = resolve_workspace_path(&workspace, "link/new.txt", true).unwrap_err();
    assert!(!diff.message().starts_with('{'), "{diff:?}");
    // the tool path keeps its code line
    let canonical = canonical_workspace(&workspace).unwrap();
    let tool = PathGuard::for_process(None)
        .resolve(&canonical, "link/new.txt", true)
        .unwrap_err();
    assert!(tool.message().starts_with('{'), "{tool:?}");
}

#[cfg(unix)]
#[test]
fn a_read_through_a_link_swapped_in_after_the_check_is_refused() {
    use std::os::unix::fs::symlink;
    let dir = TempDir::new("fs");
    fs::create_dir_all(dir.path().join("ws")).unwrap();
    let workspace = canonical_workspace(&dir.path().join("ws")).unwrap();
    let outside = dir.file("outside/secret.txt", "TOP SECRET");
    fs::write(workspace.join("x"), "fine").unwrap();
    let env = Env::open(Some(&workspace), None, "s", ReadHashes::new()).unwrap();
    // The path was checked while `x` was a plain file; the open then happens after a swap.
    let checked = env.resolve("x", false).unwrap();
    assert!(ops::open_read(&env, &checked, "x").is_ok());
    fs::remove_file(&checked).unwrap();
    symlink(&outside, &checked).unwrap();

    let denied = ops::open_read(&env, &checked, "x").unwrap_err();
    assert!(
        denied.message().contains("outside the workspace"),
        "{denied:?}"
    );
    // A link to another file of the workspace is followed by the checker too, so it passes.
    let inside = dir.file("ws/y", "inside");
    fs::remove_file(&checked).unwrap();
    symlink(&inside, &checked).unwrap();
    assert!(ops::open_read(&env, &checked, "x").is_ok());
}

#[cfg(unix)]
#[test]
fn a_symlink_inside_the_workspace_is_followed() {
    use std::os::unix::fs::symlink;
    let dir = TempDir::new("fs");
    let real = dir.file("real/x.txt", "x");
    symlink(dir.path().join("real"), dir.path().join("alias")).unwrap();
    assert_eq!(
        resolve_workspace_path(dir.path(), "alias/x.txt", false).unwrap(),
        real
    );
}

#[test]
fn the_data_root_and_credential_directories_are_always_denied() {
    let dir = TempDir::new("fs");
    let home = dir.path().to_path_buf();
    let data = dir.path().join("data");
    dir.file("data/spanbay.sqlite3", "db");
    dir.file(".ssh/id_rsa", "key");
    dir.file(".aws/credentials", "c");
    dir.file(".gnupg/pubring.kbx", "g");
    dir.file("ok.txt", "ok");
    let guard = PathGuard::new(Some(&data), Some(&home));
    let workspace = canonical_workspace(dir.path()).unwrap();
    for candidate in [
        "data/spanbay.sqlite3",
        "data",
        ".ssh/id_rsa",
        ".ssh",
        ".aws/credentials",
        ".gnupg/pubring.kbx",
        ".ssh/new_key",
    ] {
        for for_write in [false, true] {
            let result = guard.resolve(&workspace, candidate, for_write);
            assert!(
                is_validation(&result),
                "{candidate} write={for_write}: {result:?}"
            );
        }
    }
    let absolute = home.join(".ssh/id_rsa");
    assert!(is_validation(&guard.resolve(
        &workspace,
        absolute.to_str().unwrap(),
        false
    )));
    assert!(guard.resolve(&workspace, "ok.txt", false).is_ok());
}

#[test]
fn a_workspace_that_is_a_protected_directory_is_denied() {
    let dir = TempDir::new("fs");
    let ssh = dir.path().join(".ssh");
    fs::create_dir(&ssh).unwrap();
    fs::write(ssh.join("config"), "c").unwrap();
    let guard = PathGuard::new(None, Some(dir.path()));
    let workspace = canonical_workspace(&ssh).unwrap();
    assert!(is_validation(&guard.resolve(&workspace, "config", false)));
    assert!(is_validation(&guard.resolve(&workspace, "fresh", true)));
    let data = PathGuard::new(Some(&ssh), None);
    assert!(is_validation(&data.resolve(&workspace, "config", false)));
}

#[test]
fn protected_files_ask_even_under_auto_all() {
    let ws = Path::new("/w");
    let reason = |path: &str, write: bool| approval_reason(ws, &ws.join(path), write);
    for path in [".env", ".env.local", "sub/.env.production", ".ENV"] {
        assert!(reason(path, false).is_some(), "read {path}");
        assert!(reason(path, true).is_some(), "write {path}");
    }
    for path in [
        ".git/hooks/pre-commit",
        "pkg/.git/hooks/x",
        ".claude/settings.json",
        ".agents/skill.md",
        ".zshrc",
        ".bashrc",
        ".profile",
        ".zprofile",
        ".GIT/HOOKS/pre-push",
    ] {
        assert!(reason(path, true).is_some(), "write {path}");
        assert!(reason(path, false).is_none(), "read {path}");
    }
    for path in [
        "src/main.rs",
        ".gitignore",
        ".git/HEAD",
        "environment.md",
        "a/.envrc.d/x",
        "id_rsa.pub",
        "docs/identity.md",
    ] {
        assert!(reason(path, true).is_none(), "{path}");
        assert!(reason(path, false).is_none(), "read {path}");
    }
}

#[test]
fn credential_files_ask_on_a_read_too() {
    let ws = Path::new("/w");
    let reason = |path: &str, write: bool| approval_reason(ws, &ws.join(path), write);
    for path in [
        ".git/config",
        "sub/.git/config",
        ".git-credentials",
        ".npmrc",
        "pkg/.PYPIRC",
        ".netrc",
        ".docker/config.json",
        "id_ed25519",
        "keys/id_rsa",
        "cert.pem",
        "KEY.P12",
    ] {
        assert!(reason(path, false).is_some(), "read {path}");
    }
    // The grep walk skips them like `.env`, so a content search never reads them silently.
    assert!(reason("config.json", false).is_none());
}

#[tokio::test]
async fn grep_skips_credential_files_but_an_explicit_path_reaches_them() {
    let fx = Fixture::new();
    fx.dir.file(".npmrc", "needle=token\n");
    fx.dir.file("keys/id_rsa", "needle\n");
    fx.dir.file("ok.txt", "needle\n");
    let text = fx.ok("grep", json!({"pattern": "needle"})).await;
    assert_eq!(text, "ok.txt:1:needle\n");
    let inspection = fx
        .inspect("grep", json!({"pattern": "n", "path": ".npmrc"}))
        .unwrap();
    assert!(inspection.always_ask.is_some());
}

#[test]
fn inspect_flags_protected_paths_and_denies_the_data_root() {
    let fx = Fixture::new();
    fx.dir.file(".env", "SECRET=1");
    fx.dir.file(".git/hooks/pre-commit", "#!/bin/sh\n");
    fx.dir.file("a.txt", "a\n");
    let read_env = fx.inspect("read_file", json!({"path": ".env"})).unwrap();
    assert!(read_env.read_only && read_env.always_ask.is_some());
    let hook = fx
        .inspect(
            "write_file",
            json!({"path": ".git/hooks/pre-commit", "content": "x"}),
        )
        .unwrap();
    assert!(hook.always_ask.is_some() && hook.destructive && !hook.read_only);
    let hook_read = fx
        .inspect("read_file", json!({"path": ".git/hooks/pre-commit"}))
        .unwrap();
    assert!(hook_read.always_ask.is_none());
    let plain = fx.inspect("read_file", json!({"path": "a.txt"})).unwrap();
    assert!(plain.always_ask.is_none() && plain.arg_subject.as_deref() == Some("a.txt"));
    // Outside the Folder: refused in every mode with no card, whichever tool and whether or not
    // the target exists.
    let calls = [
        ("read_file", json!({"path": "../x"})),
        ("list_dir", json!({"path": "/"})),
        ("glob", json!({"pattern": "*", "path": "../.."})),
        ("grep", json!({"pattern": "x", "path": "/etc"})),
        ("write_file", json!({"path": "../new.txt", "content": "x"})),
        (
            "write_file",
            json!({"path": "/tmp/new.txt", "content": "x"}),
        ),
        (
            "edit_file",
            json!({"path": "../x", "old_string": "a", "new_string": "b"}),
        ),
    ];
    for (tool, args) in calls {
        let blocked = fx.inspect(tool, args.clone()).unwrap();
        let reason = blocked.hard_block.as_deref().unwrap_or_default();
        assert!(
            reason.contains("outside the workspace"),
            "{tool} {args}: {blocked:?}"
        );
        assert_eq!(
            blocked.read_only,
            tool != "write_file" && tool != "edit_file"
        );
    }
    assert!(plain.hard_block.is_none());
}

#[tokio::test]
async fn the_state_data_root_is_denied_through_the_context() {
    let dir = TempDir::new("fs");
    let data = dir.path().join("data");
    fs::create_dir(&data).unwrap();
    let settings = spanbay_model::settings::Settings::default();
    let cancel = CancelToken::new();
    let parts = TestParts::default();
    let ctx = ToolContext {
        workspace_dir: Some(dir.path()),
        data_root: Some(&data),
        ..ToolContext::for_test(None, &settings, &cancel, &parts)
    };
    let tool = Fixture::tool("read_file");
    for (name, args) in [
        ("read_file", json!({"path": "data/spanbay.sqlite3"})),
        ("write_file", json!({"path": "data/x.txt", "content": "x"})),
    ] {
        let blocked = Fixture::tool(name).inspect(&ctx, &args).unwrap();
        assert!(
            blocked
                .hard_block
                .as_deref()
                .is_some_and(|reason| reason.contains("protected location")),
            "{name}: {blocked:?}"
        );
    }
    let output = tool
        .call(&ctx, json!({"path": "data/spanbay.sqlite3"}))
        .await;
    assert!(output.is_error, "{}", output.text_content());
}

#[tokio::test]
async fn a_session_without_workspace_gets_a_clear_error() {
    let mut fx = Fixture::new();
    fx.workspace = false;
    let text = fx.err("read_file", json!({"path": "a"})).await;
    assert!(text.contains("no workspace folder"), "{text}");
    assert!(fx.inspect("list_dir", json!({})).is_err());
}

// ---- read_file --------------------------------------------------------------------------------

#[tokio::test]
async fn read_file_numbers_pages_bounds_and_rejects_invalid_inputs() {
    let fx = Fixture::new();
    fx.dir.file("a.txt", "alpha\nbeta\r\ngamma");
    assert_eq!(
        fx.ok("read_file", json!({"path": "a.txt"})).await,
        "     1\talpha\n     2\tbeta\n     3\tgamma\n"
    );
    let page = fx
        .ok(
            "read_file",
            json!({"path": "a.txt", "offset": 2, "limit": 1}),
        )
        .await;
    assert!(page.starts_with("     2\tbeta\n") && page.contains("offset=3"));
    assert!(
        fx.ok("read_file", json!({"path": "a.txt", "offset": 10}))
            .await
            .contains("3 lines")
    );
    fx.dir.file("empty.txt", "");
    assert!(
        fx.ok("read_file", json!({"path": "empty.txt"}))
            .await
            .contains("empty")
    );

    fx.dir.file(
        "many.txt",
        (1..=2500)
            .map(|n| format!("line {n}\n"))
            .collect::<String>(),
    );
    let many = fx.ok("read_file", json!({"path": "many.txt"})).await;
    assert!(many.contains("  2000\tline 2000\n") && many.contains("offset=2001"));
    assert!(!many.contains("line 2001"));
    let next = fx
        .ok("read_file", json!({"path": "many.txt", "offset": 2001}))
        .await;
    assert!(next.contains("  2500\tline 2500\n") && !next.contains("[Showing"));

    fx.dir.file(
        "wide.txt",
        (0..400)
            .map(|n| format!("{n:0>1000}\n"))
            .collect::<String>(),
    );
    let wide = fx.ok("read_file", json!({"path": "wide.txt"})).await;
    assert!(wide.len() < MAX_READ_BYTES + 3000 && wide.contains("offset="));
    fx.dir
        .file("long.txt", format!("{}\nnext\n", "x".repeat(200_000)));
    let long = fx.ok("read_file", json!({"path": "long.txt"})).await;
    assert!(long.contains("[line truncated]") && long.contains("     2\tnext"));
    assert!(long.len() < 4000);

    fx.dir.file("bin.dat", [1u8, 2, 0, 3]);
    for (args, message) in [
        (json!({"path": "bin.dat"}), "binary"),
        (json!({"path": "."}), "list_dir"),
        (json!({"path": "missing.txt"}), "does not exist"),
        (json!({"offset": 1}), "Invalid arguments"),
    ] {
        assert!(
            fx.err("read_file", args).await.contains(message),
            "{message}"
        );
    }
}

#[tokio::test]
async fn read_file_returns_images_and_pdfs_as_parts() {
    let fx = Fixture::new();
    fx.dir.file("pic.PNG", [0x89, b'P', b'N', b'G']);
    let output = fx.call("read_file", json!({"path": "pic.PNG"})).await;
    assert!(!output.is_error);
    assert!(matches!(
        output.parts.as_slice(),
        [HubPart::Image { image: HubImage::Base64 { media_type, data } }]
            if media_type == "image/png" && data == "iVBORw=="
    ));
    fx.dir.file("doc.pdf", b"%PDF-1.4");
    let output = fx.call("read_file", json!({"path": "doc.pdf"})).await;
    match output.parts.as_slice() {
        [HubPart::Document { document }] => {
            assert_eq!(document.media_type, "application/pdf");
            assert_eq!(document.name.as_deref(), Some("doc.pdf"));
            assert!(
                matches!(&document.source, HubDocumentSource::Base64 { data } if data == "JVBERi0xLjQ=")
            );
        }
        other => panic!("{other:?}"),
    }
    let big = fs::File::create(fx.dir.path().join("big.jpg")).unwrap();
    big.set_len(MAX_IMAGE_BYTES + 1).unwrap();
    let text = fx.err("read_file", json!({"path": "big.jpg"})).await;
    assert!(text.contains("limit"), "{text}");
}

// ---- write_file / edit_file -------------------------------------------------------------------

#[tokio::test]
async fn write_file_creates_parents_overwrites_and_previews() {
    let fx = Fixture::new();
    let args = json!({"path": "new/dir/a.txt", "content": "one\ntwo\n"});
    let inspection = fx.inspect("write_file", args.clone()).unwrap();
    assert!(!inspection.destructive && !inspection.read_only && inspection.always_ask.is_none());
    assert_eq!(inspection.arg_subject.as_deref(), Some("new/dir/a.txt"));
    match inspection.preview {
        Some(ToolApprovalPreview::Diff { path, unified_diff }) => {
            assert_eq!(path, "new/dir/a.txt");
            assert_eq!(
                unified_diff,
                "--- /dev/null\n+++ b/new/dir/a.txt\n@@ -0,0 +1,2 @@\n+one\n+two\n"
            );
        }
        other => panic!("{other:?}"),
    }
    let text = fx.ok("write_file", args).await;
    assert!(text.starts_with("Created new/dir/a.txt"), "{text}");
    assert_eq!(fx.dir.read("new/dir/a.txt"), "one\ntwo\n");

    let args = json!({"path": "new/dir/a.txt", "content": "one\nTWO\n"});
    let inspection = fx.inspect("write_file", args.clone()).unwrap();
    assert!(inspection.destructive);
    match inspection.preview {
        Some(ToolApprovalPreview::Diff { unified_diff, .. }) => assert_eq!(
            unified_diff,
            "--- a/new/dir/a.txt\n+++ b/new/dir/a.txt\n@@ -1,2 +1,2 @@\n one\n-two\n+TWO\n"
        ),
        other => panic!("{other:?}"),
    }
    assert!(fx.ok("write_file", args).await.starts_with("Overwrote"));
    assert_eq!(fx.dir.read("new/dir/a.txt"), "one\nTWO\n");
    let leftovers = fs::read_dir(fx.dir.path().join("new/dir")).unwrap().count();
    assert_eq!(leftovers, 1, "no temporary file stays behind");
}

#[tokio::test]
async fn write_file_rejects_invalid_inputs_and_previews_binary_overwrites() {
    let fx = Fixture::new();
    let huge = json!({"path": "big.txt", "content": "x".repeat(MAX_WRITE_BYTES + 1)});
    assert!(fx.inspect("write_file", huge.clone()).is_err());
    assert!(fx.err("write_file", huge).await.contains("at most"));
    fx.dir.file("d/x", "x");
    assert!(
        fx.err("write_file", json!({"path": "d", "content": "x"}))
            .await
            .contains("directory")
    );
    assert!(
        !fx.err("write_file", json!({"path": "../evil.txt", "content": "x"}))
            .await
            .is_empty()
    );
    assert!(!fx.dir.path().parent().unwrap().join("evil.txt").exists());
    fx.dir.file("b.bin", [0xff, 0xfe, 0x00]);
    let inspection = fx
        .inspect("write_file", json!({"path": "b.bin", "content": "text"}))
        .unwrap();
    assert!(inspection.destructive);
    assert!(matches!(
        inspection.preview,
        Some(ToolApprovalPreview::Diff { unified_diff, .. }) if unified_diff.contains("binary")
    ));
}

#[tokio::test]
async fn edit_file_replaces_unique_text_and_previews_the_diff() {
    let fx = Fixture::new();
    fx.dir.file("src.txt", "fn a() {}\nfn b() {}\nfn c() {}\n");
    let args = json!({"path": "src.txt", "old_string": "fn b() {}", "new_string": "fn beta() {}"});
    let inspection = fx.inspect("edit_file", args.clone()).unwrap();
    assert!(inspection.destructive);
    match inspection.preview {
        Some(ToolApprovalPreview::Diff { path, unified_diff }) => {
            assert_eq!(path, "src.txt");
            assert_eq!(
                unified_diff,
                "--- a/src.txt\n+++ b/src.txt\n@@ -1,3 +1,3 @@\n fn a() {}\n-fn b() {}\n+fn beta() {}\n fn c() {}\n"
            );
        }
        other => panic!("{other:?}"),
    }
    let text = fx.ok("edit_file", args).await;
    assert!(text.contains("replaced 1 occurrence"), "{text}");
    assert_eq!(
        fx.dir.read("src.txt"),
        "fn a() {}\nfn beta() {}\nfn c() {}\n"
    );
}

#[tokio::test]
async fn edit_file_validates_matches_replace_all_and_preserves_crlf() {
    let fx = Fixture::new();
    fx.dir.file("dup.txt", "x = 1\ny = 2\nx = 1\n");
    let ambiguous = json!({"path": "dup.txt", "old_string": "x = 1", "new_string": "x = 2"});
    let text = fx.err("edit_file", ambiguous.clone()).await;
    assert!(text.contains("occurs 2 times") && text.contains("replace_all"));
    assert!(fx.inspect("edit_file", ambiguous).is_err());
    for (old, new, reason) in [
        ("z = 9", "z", "not found"),
        ("x = 1", "x = 1", "identical"),
        ("", "z", "empty"),
    ] {
        assert!(
            fx.err(
                "edit_file",
                json!({"path": "dup.txt", "old_string": old, "new_string": new})
            )
            .await
            .contains(reason)
        );
    }
    assert_eq!(fx.dir.read("dup.txt"), "x = 1\ny = 2\nx = 1\n");
    assert!(fx.ok("edit_file", json!({"path": "dup.txt", "old_string": "x = 1", "new_string": "x = 2", "replace_all": true})).await.contains("replaced 2 occurrences"));
    assert_eq!(fx.dir.read("dup.txt"), "x = 2\ny = 2\nx = 2\n");
    assert!(
        fx.err(
            "edit_file",
            json!({"path": "missing", "old_string": "a", "new_string": "b"})
        )
        .await
        .contains("does not exist")
    );

    fx.dir.file("w.txt", "one\r\ntwo\r\nthree\r\n");
    fx.ok(
        "edit_file",
        json!({"path": "w.txt", "old_string": "one\ntwo", "new_string": "uno\ndos\ndos2"}),
    )
    .await;
    assert_eq!(fx.dir.read("w.txt"), "uno\r\ndos\r\ndos2\r\nthree\r\n");
    fx.ok(
        "edit_file",
        json!({"path": "w.txt", "old_string": "three\r\n", "new_string": "3\r\n"}),
    )
    .await;
    assert_eq!(fx.dir.read("w.txt"), "uno\r\ndos\r\ndos2\r\n3\r\n");
}

#[cfg(unix)]
#[tokio::test]
async fn editing_keeps_the_file_mode() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let path = fx.dir.file("run.sh", "echo a\n");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let args = json!({"path": "run.sh", "old_string": "a", "new_string": "b"});
    fx.ok("edit_file", args).await;
    fx.ok(
        "write_file",
        json!({"path": "run.sh", "content": "echo c\n"}),
    )
    .await;
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[cfg(unix)]
#[tokio::test]
async fn writing_through_a_symlink_to_the_outside_fails_without_side_effects() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    let outside = TempDir::new("fs");
    symlink(outside.path(), fx.dir.path().join("link")).unwrap();
    let text = fx
        .err(
            "write_file",
            json!({"path": "link/new.txt", "content": "x"}),
        )
        .await;
    assert!(text.contains("outside the workspace"), "{text}");
    assert!(!outside.path().join("new.txt").exists());
    outside.file("secret.txt", "s");
    assert!(
        fx.err("read_file", json!({"path": "link/secret.txt"}))
            .await
            .contains("outside")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_path_that_leaves_the_folder_fails_with_the_outside_folder_code_first() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    let outside = TempDir::new("fs");
    symlink(outside.path(), fx.dir.path().join("link")).unwrap();
    let text = fx
        .err(
            "write_file",
            json!({"path": "link/new.txt", "content": "x"}),
        )
        .await;
    let (first, rest) = text.split_once('\n').unwrap();
    assert_eq!(first, r#"{"error":{"code":"outside_folder"}}"#);
    // The model still reads the original English sentence after the code line.
    assert!(rest.contains("outside the workspace"), "{text}");
    // A protected location is not an escape from the folder: no `outside_folder` code.
    let guard = PathGuard::new(Some(&fx.dir.path().join("secrets")), None);
    fs::create_dir_all(fx.dir.path().join("secrets")).unwrap();
    let workspace = canonical_workspace(fx.dir.path()).unwrap();
    let denied = guard.resolve(&workspace, "secrets/key", true).unwrap_err();
    assert!(!denied.message().contains("outside_folder"), "{denied:?}");
}

// ---- diff -------------------------------------------------------------------------------------

#[test]
fn unified_diff_covers_hunks_context_and_endings() {
    let old: String = (1..=20).map(|n| format!("l{n}\n")).collect();
    let new = old
        .replace("l3\n", "L3\n")
        .replace("l18\n", "L18\n")
        .replace("l19\n", "");
    let diff = diff::unified_diff("f", Some(&old), &new, 10_000);
    assert_eq!(
        diff,
        "--- a/f\n+++ b/f\n@@ -1,6 +1,6 @@\n l1\n l2\n-l3\n+L3\n l4\n l5\n l6\n@@ -15,6 +15,5 @@\n l15\n l16\n l17\n-l18\n-l19\n+L18\n l20\n"
    );
    assert_eq!(diff::unified_diff("f", Some("a\n"), "a\n", 100), "");
    let missing_newline = diff::unified_diff("f", Some("a\nb"), "a\nb\n", 1000);
    assert!(
        missing_newline.contains("-b\n\\ No newline at end of file\n+b\n"),
        "{missing_newline}"
    );
    let deleted = diff::unified_diff("f", Some("a\nb\n"), "", 1000);
    assert!(deleted.contains("@@ -1,2 +0,0 @@\n-a\n-b\n"), "{deleted}");
}

#[test]
fn unified_diff_is_capped_on_a_char_boundary() {
    let new: String = (0..5000).map(|n| format!("é{n}\n")).collect();
    let diff = diff::unified_diff("f", None, &new, 1001);
    assert!(diff.ends_with("… diff truncated …\n"));
    assert!(diff.len() < 1100);
    // A middle too large for the alignment table still produces a diff.
    let old: String = (0..3000).map(|n| format!("old{n}\n")).collect();
    let new: String = (0..3000).map(|n| format!("new{n}\n")).collect();
    let diff = diff::unified_diff("f", Some(&old), &new, 1 << 20);
    assert!(diff.contains("-old0\n") && diff.contains("+new2999\n"));
}

// ---- list_dir ---------------------------------------------------------------------------------

#[tokio::test]
async fn list_dir_shows_directories_first_with_sizes() {
    let fx = Fixture::new();
    fx.dir.file("zeta/inner.txt", "x");
    fx.dir.file("b.txt", "12345");
    fx.dir.file("a.bin", vec![0u8; 2048]);
    let text = fx.ok("list_dir", json!({})).await;
    assert_eq!(
        text,
        "./ (3 entries)\nzeta/\na.bin  (2.0 KiB)\nb.txt  (5 B)\n"
    );
    let text = fx.ok("list_dir", json!({"path": "zeta"})).await;
    assert!(
        text.starts_with("zeta/ (1 entries)") && text.contains("inner.txt  (1 B)"),
        "{text}"
    );
    assert!(
        fx.err("list_dir", json!({"path": "b.txt"}))
            .await
            .contains("not a directory")
    );
    fs::create_dir(fx.dir.path().join("empty")).unwrap();
    assert!(
        fx.ok("list_dir", json!({"path": "empty"}))
            .await
            .contains("is empty")
    );
}

#[tokio::test]
async fn directory_and_glob_results_are_capped_and_ties_sort_by_path() {
    let fx = Fixture::new();
    let stamp = SystemTime::now() - Duration::from_secs(60);
    for index in 0..1005 {
        let path = fx.dir.file(&format!("d/f{index:04}.txt"), "");
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(stamp)
            .unwrap();
    }
    let listed = fx.ok("list_dir", json!({"path": "d"})).await;
    assert_eq!(listed.lines().count(), 1 + MAX_LIST_ENTRIES + 1);
    assert!(listed.contains("first 500 of 1005"));
    assert!(listed.contains("f0499.txt") && !listed.contains("f0500.txt"));

    let globbed = fx.ok("glob", json!({"pattern": "d/*.txt"})).await;
    let lines: Vec<_> = globbed.lines().collect();
    assert_eq!(lines.len(), MAX_GLOB_RESULTS + 1);
    assert_eq!((lines[0], lines[999]), ("d/f0000.txt", "d/f0999.txt"));
    assert!(lines[1000].contains("1000 newest of 1005"));
}

// ---- glob_files -------------------------------------------------------------------------------

#[test]
fn glob_syntax_translates_to_path_matching() {
    let matches = |pattern: &str, path: &str| search::Glob::new(pattern).unwrap().is_match(path);
    assert!(matches("*.rs", "main.rs") && matches("*.rs", "src/deep/main.rs"));
    assert!(!matches("*.rs", "main.rsx"));
    assert!(matches("src/*.rs", "src/a.rs") && !matches("src/*.rs", "src/x/a.rs"));
    assert!(matches("src/**/*.rs", "src/a.rs") && matches("src/**/*.rs", "src/x/y/a.rs"));
    assert!(!matches("src/**/*.rs", "lib/a.rs"));
    assert!(matches("**/a.rs", "a.rs") && matches("**/a.rs", "x/a.rs"));
    assert!(matches("src/**", "src/x/y.txt"));
    assert!(matches("file?.txt", "file1.txt") && !matches("file?.txt", "file12.txt"));
    assert!(matches("[a-c]x", "bx") && !matches("[a-c]x", "dx"));
    assert!(matches("[!a-c]x", "dx") && !matches("[!a-c]x", "ax"));
    assert!(matches("*.{ts,tsx}", "a.tsx") && !matches("*.{ts,tsx}", "a.js"));
    assert!(matches("a.b", "a.b") && !matches("a.b", "axb"));
    assert!(matches("a[", "a[") && matches("a(b)+", "a(b)+"));
    assert!(matches("./src/a.rs", "src/a.rs"));
    assert!(search::Glob::new("").is_err());
    assert!(search::Glob::new("{a,b").is_err());
    assert!(matches("file?.txt", "fileé.txt"));
    assert!(matches(r"file\?.txt", "file?.txt"));
    assert!(matches("{,a}.txt", ".txt"));
}

#[tokio::test]
async fn glob_files_finds_files_newest_first_and_skips_build_dirs() {
    let fx = Fixture::new();
    let old = fx.dir.file("src/old.rs", "");
    fx.dir.file("src/new.rs", "");
    fx.dir.file("src/sub/other.rs", "");
    fx.dir.file("src/readme.md", "");
    fx.dir.file("node_modules/pkg/x.rs", "");
    fx.dir.file("target/debug/y.rs", "");
    fx.dir.file(".git/z.rs", "");
    fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
    let text = fx.ok("glob", json!({"pattern": "*.rs"})).await;
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.last(), Some(&"src/old.rs"), "{text}");
    assert_eq!(lines.len(), 3, "{text}");
    assert!(lines.contains(&"src/new.rs") && lines.contains(&"src/sub/other.rs"));
    let scoped = fx
        .ok("glob", json!({"pattern": "*.rs", "path": "src/sub"}))
        .await;
    assert_eq!(scoped, "src/sub/other.rs\n");
    let none = fx.ok("glob", json!({"pattern": "*.py"})).await;
    assert!(none.starts_with("No files match"), "{none}");
    assert!(
        fx.err("glob", json!({"pattern": "src", "path": "src/readme.md"}))
            .await
            .contains("not a directory")
    );
    assert!(
        fx.err("glob", json!({"pattern": ""}))
            .await
            .contains("empty")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn walks_never_follow_symlinks() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    let outside = TempDir::new("fs");
    outside.file("leak.txt", "needle");
    fx.dir.file("in.txt", "needle");
    symlink(outside.path(), fx.dir.path().join("linkdir")).unwrap();
    symlink(
        outside.path().join("leak.txt"),
        fx.dir.path().join("linkfile.txt"),
    )
    .unwrap();
    let glob = fx.ok("glob", json!({"pattern": "*.txt"})).await;
    assert_eq!(glob, "in.txt\n");
    let grep = fx.ok("grep", json!({"pattern": "needle"})).await;
    assert_eq!(grep, "in.txt:1:needle\n");
    let list = fx.ok("list_dir", json!({})).await;
    assert!(list.contains("linkdir -> "), "{list}");
}

// ---- grep_files -------------------------------------------------------------------------------

#[tokio::test]
async fn grep_files_reports_path_line_text() {
    let fx = Fixture::new();
    fx.dir
        .file("a.rs", "fn main() {}\nlet Needle = 1;\nlet needle = 2;\n");
    fx.dir.file("sub/b.txt", "no match here\nneedle again\n");
    fx.dir.file("node_modules/n.js", "needle\n");
    fx.dir.file("target/t.txt", "needle\n");
    fx.dir.file(".git/g.txt", "needle\n");
    let text = fx.ok("grep", json!({"pattern": "needle"})).await;
    assert_eq!(text, "a.rs:3:let needle = 2;\nsub/b.txt:2:needle again\n");
    let ci = fx
        .ok(
            "grep",
            json!({"pattern": "needle", "case_insensitive": true, "glob": "*.rs"}),
        )
        .await;
    assert_eq!(ci, "a.rs:2:let Needle = 1;\na.rs:3:let needle = 2;\n");
    let regex = fx
        .ok(
            "grep",
            json!({"pattern": r"^let \w+ = (\d)", "path": "a.rs"}),
        )
        .await;
    assert_eq!(regex, "a.rs:2:let Needle = 1;\na.rs:3:let needle = 2;\n");
    let nested = fx
        .ok("grep", json!({"pattern": "needle", "glob": "sub/**"}))
        .await;
    assert_eq!(nested, "sub/b.txt:2:needle again\n");
    let none = fx.ok("grep", json!({"pattern": "absent"})).await;
    assert!(none.starts_with("No matches"), "{none}");
}

#[tokio::test]
async fn grep_files_validates_the_regex_and_bounds_its_size() {
    let fx = Fixture::new();
    fx.dir.file("a.txt", "x\n");
    assert!(
        fx.err("grep", json!({"pattern": "("}))
            .await
            .contains("Invalid regex")
    );
    assert!(
        fx.err("grep", json!({"pattern": ""}))
            .await
            .contains("empty")
    );
    let huge = "(a{1000}){1000}";
    assert!(
        fx.err("grep", json!({"pattern": huge}))
            .await
            .contains("Invalid regex")
    );
    assert!(fx.inspect("grep", json!({"pattern": "("})).is_err());
}

#[tokio::test]
async fn grep_files_caps_results_and_line_length() {
    let fx = Fixture::new();
    let lines: String = (0..1500).map(|n| format!("hit {n}\n")).collect();
    fx.dir.file("many.txt", lines);
    let default = fx.ok("grep", json!({"pattern": "hit"})).await;
    assert_eq!(default.lines().count(), 101, "{default}");
    assert!(default.contains("Stopped after 100 matches"));
    let few = fx
        .ok("grep", json!({"pattern": "hit", "max_results": 3}))
        .await;
    assert_eq!(few.lines().count(), 4);
    let most = fx
        .ok("grep", json!({"pattern": "hit", "max_results": 99_999}))
        .await;
    assert_eq!(most.lines().count(), 1001);
    fx.dir
        .file("wide.txt", format!("needle {}\n", "y".repeat(2000)));
    let wide = fx
        .ok("grep", json!({"pattern": "needle", "path": "wide.txt"}))
        .await;
    assert!(
        wide.contains("… [truncated]") && wide.len() < 700,
        "{}",
        wide.len()
    );
}

#[tokio::test]
async fn grep_files_skips_binary_big_and_env_files() {
    let fx = Fixture::new();
    fx.dir.file("bin.dat", b"needle\0binary");
    fx.dir.file(
        "big.txt",
        format!("needle\n{}", "z".repeat(2 * 1024 * 1024 + 1)),
    );
    fx.dir.file(".env", "needle=secret\n");
    fx.dir.file("ok.txt", "needle\n");
    let text = fx.ok("grep", json!({"pattern": "needle"})).await;
    assert_eq!(text, "ok.txt:1:needle\n");
    // Naming the file explicitly is the approved route to it.
    let env = fx
        .ok("grep", json!({"pattern": "needle", "path": ".env"}))
        .await;
    assert_eq!(env, ".env:1:needle=secret\n");
    let inspection = fx
        .inspect("grep", json!({"pattern": "n", "path": ".env"}))
        .unwrap();
    assert!(inspection.always_ask.is_some());
}

// ---- cancellation -----------------------------------------------------------------------------

#[tokio::test]
async fn every_tool_stops_when_the_run_is_cancelled() {
    let fx = Fixture::new();
    fx.dir.file("a.txt", "needle\n");
    fx.cancel.cancel();
    for (tool, arguments) in [
        ("read_file", json!({"path": "a.txt"})),
        ("write_file", json!({"path": "b.txt", "content": "x"})),
        (
            "edit_file",
            json!({"path": "a.txt", "old_string": "needle", "new_string": "n"}),
        ),
        ("delete_file", json!({"path": "a.txt"})),
        ("move_file", json!({"from": "a.txt", "to": "c.txt"})),
        ("list_dir", json!({})),
        ("glob", json!({"pattern": "*.txt"})),
        ("grep", json!({"pattern": "needle"})),
    ] {
        let text = fx.err(tool, arguments).await;
        assert!(text.to_lowercase().contains("cancel"), "{tool}: {text}");
    }
    assert!(!fx.dir.path().join("c.txt").exists());
    assert_eq!(fx.dir.read("a.txt"), "needle\n");
    assert!(!fx.dir.path().join("b.txt").exists());
}

// ---- delete, move, batch edits ----------------------------------------------------------------

/// Runs a changing tool through `apply_write`, collecting the journal records it hands over.
fn apply(
    fx: &Fixture,
    tool: &str,
    arguments: Value,
) -> (Result<AppliedWrite, AppError>, Vec<FileChangeRecord>) {
    let records = std::sync::Mutex::new(Vec::new());
    let result = apply_write(
        tool,
        fx.dir.path(),
        None,
        "s",
        &fx.parts.read_hashes,
        &arguments,
        &fx.cancel,
        &|record| {
            records.lock().unwrap().push(record.clone());
            Ok(())
        },
    );
    (result, records.into_inner().unwrap())
}

#[tokio::test]
async fn a_deleted_file_is_journaled_and_comes_back_on_undo() {
    let fx = Fixture::new();
    fx.dir.file("src/old.rs", "fn a() {}\nfn b() {}\n");
    let inspection = fx
        .inspect("delete_file", json!({"path": "src/old.rs"}))
        .unwrap();
    assert!(inspection.destructive && !inspection.read_only);
    let (applied, records) = apply(&fx, "delete_file", json!({"path": "src/old.rs"}));
    let applied = applied.unwrap();
    assert!(!fx.dir.path().join("src/old.rs").exists());
    assert_eq!(records, applied.changes);
    let record = &records[0];
    assert_eq!(record.kind, FileChangeKind::Edit);
    assert_eq!(record.after_sha256, None);
    assert_eq!((record.added, record.removed), (0, 2));
    // Undo while the file is still gone restores it; once restored, a second undo conflicts.
    let workspace = canonical_workspace(fx.dir.path()).unwrap();
    let before = record.before_bytes.as_deref();
    assert_eq!(
        undo_file_change(&workspace, "src/old.rs", before, None, false).unwrap(),
        UndoOutcome::Restored
    );
    assert_eq!(fx.dir.read("src/old.rs"), "fn a() {}\nfn b() {}\n");
    assert_eq!(
        undo_file_change(&workspace, "src/old.rs", before, None, false).unwrap(),
        UndoOutcome::Conflict
    );
    // Folders and missing files are refused.
    fs::create_dir_all(fx.dir.path().join("dir")).unwrap();
    assert!(
        fx.err("delete_file", json!({"path": "dir"}))
            .await
            .contains("folder")
    );
    assert!(
        fx.inspect("delete_file", json!({"path": ".git/config"}))
            .is_err()
            || fx
                .inspect("delete_file", json!({"path": ".git/config"}))
                .unwrap()
                .hard_block
                .is_some()
    );
}

#[tokio::test]
async fn a_move_is_a_create_and_a_delete_and_never_overwrites() {
    let fx = Fixture::new();
    fx.dir.file("a.txt", "alpha\n");
    fx.dir.file("taken.txt", "keep\n");
    let refused = fx
        .err("move_file", json!({"from": "a.txt", "to": "taken.txt"}))
        .await;
    assert!(refused.contains("already exists"), "{refused}");
    assert_eq!(fx.dir.read("taken.txt"), "keep\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = fx.dir.path().join("a.txt");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (applied, records) = apply(
        &fx,
        "move_file",
        json!({"from": "a.txt", "to": "deep/b.txt"}),
    );
    applied.unwrap();
    assert!(!fx.dir.path().join("a.txt").exists());
    assert_eq!(fx.dir.read("deep/b.txt"), "alpha\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let moved = fs::metadata(fx.dir.path().join("deep/b.txt")).unwrap();
        assert_eq!(
            moved.permissions().mode() & 0o777,
            0o755,
            "the mode moves too"
        );
    }
    let kinds: Vec<(&str, FileChangeKind, bool)> = records
        .iter()
        .map(|r| (r.path.as_str(), r.kind, r.after_sha256.is_some()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("deep/b.txt", FileChangeKind::Create, true),
            ("a.txt", FileChangeKind::Edit, false)
        ]
    );
    // A destination under .git/ is refused before anything moves.
    fx.dir.file("c.txt", "c\n");
    let blocked = fx.inspect("move_file", json!({"from": "c.txt", "to": ".git/c.txt"}));
    assert!(blocked.is_err() || blocked.unwrap().hard_block.is_some());
}

#[tokio::test]
async fn batch_edits_apply_in_order_or_not_at_all() {
    let fx = Fixture::new();
    fx.dir.file("m.rs", "let a = 1;\nlet b = 2;\n");
    let edits = json!({"path": "m.rs", "edits": [
        {"old_string": "a = 1", "new_string": "a = 10"},
        {"old_string": "a = 10;\nlet b", "new_string": "a = 10;\nlet c"}
    ]});
    let text = fx.ok("edit_file", edits).await;
    assert!(text.contains("replaced 2 occurrences"), "{text}");
    assert_eq!(fx.dir.read("m.rs"), "let a = 10;\nlet c = 2;\n");
    // The second edit fails: the first is not applied either.
    let failing = json!({"path": "m.rs", "edits": [
        {"old_string": "a = 10", "new_string": "a = 11"},
        {"old_string": "missing", "new_string": "x"}
    ]});
    let error = fx.err("edit_file", failing).await;
    assert!(error.contains("edit 2 of 2"), "{error}");
    assert_eq!(fx.dir.read("m.rs"), "let a = 10;\nlet c = 2;\n");
    let both = json!({"path": "m.rs", "old_string": "a", "new_string": "b", "edits": [{"old_string": "a", "new_string": "b"}]});
    assert!(fx.err("edit_file", both).await.contains("not both"));
}
