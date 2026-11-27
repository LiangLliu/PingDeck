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

use crate::error::AppError;
use crate::platform::HostOs;

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

/// Opens a file of a chat's folder with its default application when its type is on the
/// allow-list ([`OPENABLE`]) and shows any other file in the file manager (see
/// [`open_path_validate`] for what is accepted). Blocking: it resolves the path and starts the
/// opener on the resolved path.
///
/// # Errors
/// As [`open_path_validate`]; [`AppError::Io`] when the opener cannot be started.
pub fn open_path_impl(state: &AppState, path: &str) -> Result<(), AppError> {
    let target = open_path_validate(state, path)?;
    confirm_regular_file(&target)?;
    let (program, args) = launch_plan(HostOs::current(), &target);
    launch(program, args)
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
}
