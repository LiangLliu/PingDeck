//! Files the chat UI hands to the operating system: opening one file of a chat's folder with its
//! default application (`open_path`) and saving a text the user exports
//! (`save_text_file`).
//!
//! Single responsibility for opening: decide which files may be handed to the operating system and start it.
//! Opening is riskier than showing (`reveal_path`), so the rule is strict: only a regular file that
//! lies inside a folder some chat is bound to (links resolved first, so a link inside the folder
//! cannot lead out) is considered, and only a document type on an allow-list ([`OPENABLE`]) is
//! opened. Any other file (scripts, macro documents, web pages, programs, files without an
//! extension) is shown in the file manager instead, never started.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::agent::attachments::sniff;
use crate::agent::model::AttachmentKind;
use crate::crypto::sha256_hex;
use crate::error::AppError;
use crate::platform::{HostOs, create_private_dir_all, write_atomic};

use super::reveal_service::{is_allowed, launch, resolve_absolute, reveal_command};
use super::{AppState, command_error};

/// Extensions (lowercase) of plain documents and images that are opened with the default
/// application. Everything else is revealed instead: an operating system may run, interpret or
/// script any type that is not listed (WSH, launchers, macro documents, `file://` web pages).
const OPENABLE: &[&str] = &[
    "md", "txt", "json", "csv", "log", "pdf", "png", "jpg", "jpeg", "gif", "webp", "yaml", "yml",
    "toml",
];

/// Whether `target` has an [`OPENABLE`] extension.
fn is_openable(target: &Path) -> bool {
    target
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| OPENABLE.contains(&extension.to_ascii_lowercase().as_str()))
}

/// Checks that `path` is a regular file of a chat's folder and returns its resolved (canonical)
/// form. Whether it is opened or only revealed is decided by [`launch_plan`].
///
/// # Errors
/// [`AppError::Validation`] for a blank, over-long, relative or control-character path, one
/// outside every chat folder, or anything but a regular file; [`AppError::NotFound`] when it no
/// longer exists.
pub fn open_path_validate(state: &AppState, path: &str) -> Result<PathBuf, AppError> {
    let target = resolve_absolute(path)?;
    let folders: Vec<PathBuf> = state
        .store()
        .agent_session_folders()
        .map_err(command_error)?
        .into_iter()
        .map(PathBuf::from)
        .collect();
    if !is_allowed(&target, &folders) {
        return Err(AppError::validation(
            "only files inside a chat's folder can be opened",
        ));
    }
    if !target.is_file() {
        return Err(AppError::validation("only a file can be opened"));
    }
    Ok(target)
}

/// The program and arguments that open `path` with its default application on `os`.
fn open_command(os: HostOs, path: &Path) -> (&'static str, Vec<OsString>) {
    match os {
        HostOs::MacOs => ("open", vec!["--".into(), path.into()]),
        HostOs::Windows => ("explorer", vec![path.into()]),
        HostOs::Linux => ("xdg-open", vec![path.into()]),
    }
}

/// The command that handles `target` on `os`: the default application for an [`OPENABLE`]
/// document, the file manager's reveal for any other file.
fn launch_plan(os: HostOs, target: &Path) -> (&'static str, Vec<OsString>) {
    if is_openable(target) {
        open_command(os, target)
    } else {
        reveal_command(os, target, false)
    }
}

/// Re-checks, on an opened handle, that `target` is still the regular file validation saw (a
/// swap for a pipe or another file in between is refused).
fn confirm_regular_file(target: &Path) -> Result<(), AppError> {
    let changed = || AppError::validation("the file changed while it was being opened");
    let before = std::fs::metadata(target).map_err(|_| changed())?;
    if !before.is_file() {
        return Err(changed());
    }
    let handle = std::fs::File::open(target).map_err(|_| changed())?;
    let opened = handle.metadata().map_err(|_| changed())?;
    #[cfg(unix)]
    let same = {
        use std::os::unix::fs::MetadataExt;
        before.dev() == opened.dev() && before.ino() == opened.ino()
    };
    #[cfg(not(unix))]
    let same = true;
    if opened.is_file() && same {
        Ok(())
    } else {
        Err(changed())
    }
}

fn launch_confirmed(target: &Path) -> Result<(), AppError> {
    confirm_regular_file(target)?;
    let (program, args) = launch_plan(HostOs::current(), target);
    launch(program, args)
}
/// Opens a file of a chat's folder with its default application when its type is on the
/// allow-list ([`OPENABLE`]) and shows any other file in the file manager (see
/// [`open_path_validate`] for what is accepted). Blocking: it resolves the path and starts the
/// opener on the resolved path.
///
/// # Errors
/// As [`open_path_validate`]; [`AppError::Io`] when the opener cannot be started.
pub fn open_path_impl(state: &AppState, path: &str) -> Result<(), AppError> {
    let target = open_path_validate(state, path)?;
    launch_confirmed(&target)
}

/// Safe allow-listed file extension for a sniffed MIME type; never derived from user or model file
/// names so opening a staged copy cannot launch a script, HTML page or executable.
fn open_extension(media_type: &str) -> &'static str {
    let essence = media_type
        .split(';')
        .next()
        .unwrap_or(media_type)
        .trim()
        .to_ascii_lowercase();
    match essence.as_str() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "text/markdown" => "md",
        "application/json" => "json",
        "text/csv" => "csv",
        "application/yaml" | "text/yaml" => "yaml",
        "application/toml" | "text/toml" => "toml",
        _ => "txt",
    }
}

/// Writes `bytes` to `<data_dir>/tmp/open/<sha256>.<ext>` with owner-only permissions.
fn stage_open_bytes(state: &AppState, bytes: &[u8], media_type: &str) -> Result<PathBuf, AppError> {
    let dirs = state.data_dirs().ok_or_else(|| {
        AppError::unsupported("opening attachments needs an on-disk data directory")
    })?;
    let dir = dirs.tmp.join("open");
    create_private_dir_all(&dir)
        .map_err(|error| AppError::io(format!("{}: {error}", dir.display())))?;
    let target = dir.join(format!(
        "{}.{}",
        sha256_hex(bytes),
        open_extension(media_type)
    ));
    write_atomic(&target, bytes, true)
        .map_err(|error| AppError::io(format!("{}: {error}", target.display())))?;
    Ok(target)
}

/// Stages the blob of `id` under `<data_dir>/tmp/open/<sha256>.<ext>` using only its stored hash
/// and sniffed MIME type (never its display name or an external path).
fn stage_attachment_for_open(state: &AppState, id: &str) -> Result<PathBuf, AppError> {
    let attachment = state
        .store()
        .get_attachment(id)
        .map_err(command_error)?
        .ok_or_else(|| AppError::not_found(format!("attachment {id} not found")))?;
    if attachment.removed {
        return Err(AppError::not_found(format!("attachment {id} was removed")));
    }
    let bytes = state.attachment_store()?.read(&attachment.sha256)?;
    stage_open_bytes(state, &bytes, &attachment.media_type)
}

/// Stages an inline (MCP) image under `<data_dir>/tmp/open/<sha256>.<ext>` after sniffing its
/// magic bytes so non-image content is never written with an image extension.
fn stage_inline_image_for_open(
    state: &AppState,
    bytes: &[u8],
    media_type: &str,
) -> Result<PathBuf, AppError> {
    let store = state.attachment_store()?;
    if bytes.len() as u64 > store.max_image_bytes() {
        return Err(store.too_large(bytes.len() as u64));
    }
    let sniffed = sniff(bytes, "", media_type)
        .filter(|s| s.kind == AttachmentKind::Image && s.width.is_some() && s.height.is_some())
        .ok_or_else(|| {
            AppError::validation("only a valid PNG, JPEG, GIF or WebP image can be opened")
        })?;
    stage_open_bytes(state, bytes, sniffed.media_type)
}

/// Writes a temporary copy of attachment `id` under `<data_dir>/tmp/open` with an extension derived
/// from its sniffed MIME type and opens it with the operating system's default application.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown id or missing blob, [`AppError::Unsupported`] without a
/// data directory, [`AppError::Io`] when staging or launching fails.
pub fn agent_attachment_open_impl(state: &AppState, id: &str) -> Result<(), AppError> {
    let target = stage_attachment_for_open(state, id)?;
    launch_confirmed(&target)
}

/// Writes a temporary copy of an inline image (such as an MCP tool result image, which has no
/// attachment row and may exceed the composer upload's 2,250 px edge cap) under
/// `<data_dir>/tmp/open` after verifying its image header and opens it with the default
/// application.
///
/// # Errors
/// [`AppError::Validation`] when `bytes` is not a valid image or exceeds `ai.agent.max_image_mb`,
/// [`AppError::Unsupported`] without a data directory, [`AppError::Io`] when staging or launching
/// fails.
pub fn agent_image_open_impl(
    state: &AppState,
    bytes: &[u8],
    media_type: &str,
) -> Result<(), AppError> {
    let target = stage_inline_image_for_open(state, bytes, media_type)?;
    launch_confirmed(&target)
}

/// Largest text `save_text_file` writes, in bytes.
const MAX_SAVED_TEXT_BYTES: usize = 64 * 1024 * 1024;

/// Writes `content` to the file the user chose in a save dialog and lets `reveal_path` show it.
/// Blocking.
///
/// # Errors
/// [`AppError::Validation`] for a text over 64 MiB, [`AppError::Io`] when the file cannot be
/// written.
pub fn save_text_file_impl(state: &AppState, path: &Path, content: &str) -> Result<(), AppError> {
    if content.len() > MAX_SAVED_TEXT_BYTES {
        return Err(AppError::validation("the text is too large to save"));
    }
    crate::platform::write_atomic(path, content, false)
        .map_err(|error| AppError::io(format!("{}: {error}", path.display())))?;
    state.note_user_file(path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::model::AgentSession;
    use crate::settings::ToolApproval;

    fn write(path: &Path, text: &str) -> PathBuf {
        std::fs::write(path, text).unwrap();
        path.to_path_buf()
    }

    #[test]
    fn only_plain_documents_inside_a_chat_folder_may_be_opened() {
        let top = std::env::temp_dir().join(format!("pd-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&top);
        let top = {
            std::fs::create_dir_all(&top).unwrap();
            top.canonicalize().unwrap()
        };
        let folder = top.join("chat");
        let outside = top.join("elsewhere");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(top.join("data")).unwrap();
        let state = AppState::open(top.join("data").join("spanbay.sqlite3")).unwrap();
        state.on_start().unwrap();
        state
            .store()
            .save_agent_session(&AgentSession {
                id: "s-open".to_owned(),
                title: "chat".to_owned(),
                target: None,
                workspace_dir: Some(folder.to_string_lossy().into_owned()),
                permission_mode: ToolApproval::Ask,
                reasoning_effort: None,
                disabled_tools: Vec::new(),
                disabled_skills: Vec::new(),
                web_enabled: false,
                title_locked: false,
                head_message_id: None,
                pinned_at_ms: None,
                archived_at_ms: None,
                deleted_at_ms: None,
                forked_from: None,
                forked_from_message_id: None,
                parent_session_id: None,
                parent_run_id: None,
                todos: Vec::new(),
                total_usage: Default::default(),
                total_cost_micro_usd: 0,
                last_context_tokens: 0,
                created_at_ms: 1,
                updated_at_ms: 1,
                spanbay_workspace_id: None,
                spanbay_environment_id: None,
                spanbay_workspace_name: None,
                notice: None,
                instructions: None,
            })
            .unwrap();
        let ok = |path: &Path| open_path_validate(&state, &path.to_string_lossy()).is_ok();

        let notes = write(&folder.join("AGENTS.md"), "# rules\n");
        assert!(ok(&notes));
        // Validation only vets the location and the file kind; `launch_plan` picks open or reveal.
        assert!(ok(&write(&folder.join("run.SH"), "echo hi\n")));
        let bundle = folder.join("Evil.app");
        std::fs::create_dir_all(&bundle).unwrap();
        assert!(!ok(&bundle), "a bundle is a folder");
        assert!(!ok(&folder), "a folder");
        assert!(
            !ok(&write(&outside.join("note.md"), "x")),
            "outside every chat folder"
        );
        assert!(!ok(Path::new("relative.md")));
        assert!(!ok(&folder.join("missing.md")));
        #[cfg(unix)]
        {
            let link = folder.join("leak.md");
            std::os::unix::fs::symlink(outside.join("note.md"), &link).unwrap();
            assert!(!ok(&link), "a link out of the folder");
            // A document-named link to a script is judged by what it resolves to.
            let script = write(&folder.join("x.js"), "1");
            let alias = folder.join("alias.md");
            std::os::unix::fs::symlink(&script, &alias).unwrap();
            let resolved = open_path_validate(&state, &alias.to_string_lossy()).unwrap();
            assert_eq!(launch_plan(HostOs::MacOs, &resolved).1[0], "-R");
        }
        let _ = std::fs::remove_dir_all(&top);
    }

    #[test]
    fn a_saved_text_is_written_and_may_be_revealed() {
        let top = std::env::temp_dir().join(format!("pd-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&top);
        std::fs::create_dir_all(&top).unwrap();
        let top = top.canonicalize().unwrap();
        std::fs::create_dir_all(top.join("data")).unwrap();
        let state = AppState::open(top.join("data").join("spanbay.sqlite3")).unwrap();
        state.on_start().unwrap();
        let target = top.join("chat.md");
        save_text_file_impl(&state, &target, "# chat\n").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "# chat\n");
        assert!(
            super::super::reveal_service::reveal_path_validate(&state, &target.to_string_lossy())
                .is_ok()
        );
        let missing_parent = top.join("nope").join("chat.md");
        assert!(matches!(
            save_text_file_impl(&state, &missing_parent, "x"),
            Err(AppError::Io { .. })
        ));
        let _ = std::fs::remove_dir_all(&top);
    }

    #[test]
    fn only_allow_listed_documents_are_opened_everything_else_is_revealed() {
        for name in [
            "a.md", "A.PDF", "n.txt", "d.json", "p.png", "c.yml", "t.toml",
        ] {
            let path = PathBuf::from("/work").join(name);
            assert_eq!(launch_plan(HostOs::MacOs, &path).1[0], "--", "{name} opens");
        }
        for name in [
            "x.js", "x.py", "x.html", "x.svg", "x.docm", "x.xlsm", "x.sh", "x.exe", "x.chm",
            "x.jnlp", "x.msc", "Makefile", "tool",
        ] {
            let path = PathBuf::from("/work").join(name);
            let (program, args) = launch_plan(HostOs::MacOs, &path);
            assert_eq!(
                (program, args[0].to_str()),
                ("open", Some("-R")),
                "{name} reveals"
            );
        }
        let path = Path::new("/work/x.js");
        assert_eq!(
            launch_plan(HostOs::Windows, path).1[0],
            "/select,/work/x.js"
        );
        assert_eq!(
            launch_plan(HostOs::Linux, path).1,
            [OsString::from("/work")]
        );
    }

    #[test]
    fn a_file_swapped_for_a_directory_is_refused_at_launch() {
        let dir = std::env::temp_dir().join(format!("pd-open-swap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = write(&dir.join("a.md"), "x");
        assert!(confirm_regular_file(&file).is_ok());
        assert!(confirm_regular_file(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_opener_gets_the_path_after_a_guard_against_options() {
        let path = Path::new("/work/AGENTS.md");
        assert_eq!(
            open_command(HostOs::MacOs, path),
            ("open", vec!["--".into(), path.into()])
        );
        assert_eq!(open_command(HostOs::Linux, path).0, "xdg-open");
        assert_eq!(open_command(HostOs::Windows, path).0, "explorer");
    }

    #[test]
    fn staging_for_open_uses_safe_mime_extension_verifies_image_headers_and_clears_on_start() {
        use crate::agent::model::AgentAttachment;

        let top = std::env::temp_dir().join(format!("pd-open-stage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&top);
        std::fs::create_dir_all(top.join("data")).unwrap();
        let state = AppState::open(top.join("data").join("spanbay.sqlite3")).unwrap();
        state.on_start().unwrap();

        let store = state.attachment_store().unwrap();
        let ingested = store
            .ingest("../../evil.sh", "text/x-shellscript", b"echo hi\n")
            .unwrap();
        state
            .store()
            .insert_attachment(&AgentAttachment {
                id: "att-script".to_owned(),
                sha256: ingested.sha256,
                name: "evil.sh".to_owned(),
                media_type: ingested.media_type,
                size_bytes: ingested.size_bytes,
                kind: ingested.kind,
                width: None,
                height: None,
                thumb_data_url: None,
                lines: ingested.lines,
                created_at_ms: 1,
                removed: false,
            })
            .unwrap();
        let staged_script = stage_attachment_for_open(&state, "att-script").unwrap();
        assert!(staged_script.starts_with(top.join("data").join("tmp").join("open")));
        assert_eq!(
            staged_script.extension().and_then(|e| e.to_str()),
            Some("txt")
        );
        assert_eq!(std::fs::read(&staged_script).unwrap(), b"echo hi\n");

        state
            .store()
            .insert_attachment(&AgentAttachment {
                id: "att-removed".to_owned(),
                sha256: staged_script
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_owned(),
                name: "gone.txt".to_owned(),
                media_type: "text/plain".to_owned(),
                size_bytes: 8,
                kind: AttachmentKind::Text,
                width: None,
                height: None,
                thumb_data_url: None,
                lines: Some(1),
                created_at_ms: 2,
                removed: true,
            })
            .unwrap();
        assert!(matches!(
            stage_attachment_for_open(&state, "att-removed"),
            Err(AppError::NotFound { .. })
        ));
        // Inline MCP images over the composer's 2,250 px downscale cap still stage when valid PNGs,
        // while non-image bytes disguised as image/png are rejected.
        let mut retina = vec![
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R',
        ];
        retina.extend_from_slice(&2560_u32.to_be_bytes());
        retina.extend_from_slice(&1440_u32.to_be_bytes());
        retina.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        let staged_inline = stage_inline_image_for_open(&state, &retina, "image/png").unwrap();
        assert_eq!(
            staged_inline.extension().and_then(|e| e.to_str()),
            Some("png")
        );
        assert!(matches!(
            stage_inline_image_for_open(&state, b"#!/bin/sh\n", "image/png"),
            Err(AppError::Validation { .. })
        ));
        state.on_start().unwrap();
        assert!(!staged_script.exists());
        assert!(!staged_inline.exists());
        let _ = std::fs::remove_dir_all(&top);
    }
}
