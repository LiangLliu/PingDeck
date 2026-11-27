//! Showing a file or folder of SpanBay in the system file manager (`reveal_path`).
//!
//! Single responsibility: decide which paths may be shown and how the OS is asked to show them.
//! A path is allowed when it lies inside the data folder (logs, attachments, exports), the sync
//! folder, a configured skills folder, the folder a chat is bound to, or is a file the user
//! chose in a save dialog and the core wrote ([`AppStateInner::note_user_file`]). The path is
//! resolved first (symbolic links and `..` included), so a link inside an allowed folder cannot
//! lead out of it. The OS is asked to *show* the item, never to open it: a file is selected in
//! its folder, so nothing can be launched through this command.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};

use crate::agent::skills::expand_home;
use crate::error::AppError;
use crate::platform::HostOs;

use super::{AppState, AppStateInner, command_error};

/// Longest path accepted, in bytes.
const MAX_PATH_BYTES: usize = 4096;

/// Files the user saved that may be revealed; the oldest are forgotten first.
const USER_FILES_KEPT: usize = 64;

/// The files the user chose in a save dialog and the core wrote, newest last.
#[derive(Debug, Default)]
pub(crate) struct UserFiles(Mutex<VecDeque<PathBuf>>);

impl UserFiles {
    fn note(&self, path: PathBuf) {
        let mut files = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        files.retain(|known| known != &path);
        if files.len() == USER_FILES_KEPT {
            files.pop_front();
        }
        files.push_back(path);
    }

    fn snapshot(&self) -> Vec<PathBuf> {
        let files = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        files.iter().cloned().collect()
    }
}

impl AppStateInner {
    /// Remembers a file the user chose in a save dialog and the core just wrote, so
    /// `reveal_path` may show it ("Show in Finder" on the export toast).
    pub(crate) fn note_user_file(&self, path: &Path) {
        self.user_files
            .note(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
    }
}

/// The program and arguments that show `path` in the file manager of `os`. A folder is opened;
/// a file is selected in its folder (macOS, Windows) or its folder is opened (Linux, which has
/// no select). A folder that is a bundle (`.app` …) is selected, not opened, because opening it
/// would start the program.
pub(super) fn reveal_command(
    os: HostOs,
    path: &Path,
    is_dir: bool,
) -> (&'static str, Vec<OsString>) {
    const BUNDLES: [&str; 6] = ["app", "command", "workflow", "bundle", "saver", "prefpane"];
    let is_bundle = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| BUNDLES.contains(&extension.to_ascii_lowercase().as_str()));
    let opens_folder = is_dir && !is_bundle;
    match os {
        HostOs::MacOs if opens_folder => ("open", vec!["--".into(), path.into()]),
        HostOs::MacOs => ("open", vec!["-R".into(), "--".into(), path.into()]),
        HostOs::Windows if opens_folder => ("explorer", vec![path.into()]),
        HostOs::Windows => {
            let mut select = OsString::from("/select,");
            select.push(path);
            ("explorer", vec![select])
        }
        HostOs::Linux => {
            let folder = if opens_folder {
                path
            } else {
                path.parent().unwrap_or(path)
            };
            ("xdg-open", vec![folder.into()])
        }
    }
}

/// Whether `target` lies in or is one of `allowed`.
pub(super) fn is_allowed(target: &Path, allowed: &[PathBuf]) -> bool {
    allowed
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| target.starts_with(root))
}

/// The places `reveal_path` may show, before resolving them.
fn revealable_roots(state: &AppState) -> Result<Vec<PathBuf>, AppError> {
    let mut roots = Vec::new();
    if let Some(dirs) = state.data_dirs() {
        roots.push(dirs.root.clone());
    }
    let settings = state.settings();
    if !settings.sync.directory.is_empty() {
        roots.push(PathBuf::from(&settings.sync.directory));
    }
    roots.extend(
        settings
            .ai
            .agent
            .skill_dirs
            .iter()
            .map(|dir| expand_home(dir)),
    );
    roots.extend(
        state
            .store()
            .agent_session_folders()
            .map_err(command_error)?
            .into_iter()
            .map(PathBuf::from),
    );
    roots.extend(state.user_files.snapshot());
    Ok(roots)
}

/// Checks that `path` may be shown and returns its resolved form.
///
/// # Errors
/// [`AppError::Validation`] for a blank, over-long, relative or control-character path, or one
/// outside the places listed in the module docs; [`AppError::NotFound`] when it no longer
/// exists.
pub fn reveal_path_validate(state: &AppState, path: &str) -> Result<PathBuf, AppError> {
    let target = resolve_absolute(path)?;
    if is_allowed(&target, &revealable_roots(state)?) {
        Ok(target)
    } else {
        Err(AppError::validation(
            "only SpanBay's own folders and files you saved can be shown",
        ))
    }
}

/// The resolved form of an absolute `path` (symbolic links and `..` followed), before any check
/// of where it lies.
///
/// # Errors
/// [`AppError::Validation`] for a blank, over-long, relative or control-character path,
/// [`AppError::NotFound`] when it no longer exists.
pub(super) fn resolve_absolute(path: &str) -> Result<PathBuf, AppError> {
    let path = path.trim();
    if path.is_empty() || path.len() > MAX_PATH_BYTES || path.chars().any(char::is_control) {
        return Err(AppError::validation(
            "the path must be 1 to 4096 characters without control characters",
        ));
    }
    if !Path::new(path).is_absolute() {
        return Err(AppError::validation("the path must be absolute"));
    }
    Path::new(path)
        .canonicalize()
        .map_err(|_| AppError::not_found("that location no longer exists"))
}

/// Starts `program` detached with no input or output; a thread reaps it so it never lingers as a
/// zombie.
pub(super) fn launch(program: &str, args: Vec<OsString>) -> Result<(), AppError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| AppError::io(format!("could not start {program}: {error}")))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Shows `path` in the system file manager (see [`reveal_path_validate`] for what is allowed).
/// Blocking: it resolves the path and starts the file manager.
///
/// # Errors
/// As [`reveal_path_validate`]; [`AppError::Io`] when the file manager cannot be started.
pub fn reveal_path_impl(state: &AppState, path: &str) -> Result<(), AppError> {
    let target = reveal_path_validate(state, path)?;
    let (program, args) = reveal_command(HostOs::current(), &target, target.is_dir());
    launch(program, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("pd-reveal-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root.canonicalize().unwrap()
    }

    fn state_in(root: &Path) -> AppState {
        std::fs::create_dir_all(root.join("data")).unwrap();
        let state = AppState::open(root.join("data").join("spanbay.sqlite3")).unwrap();
        state.on_start().unwrap();
        state
    }

    #[test]
    fn the_os_is_asked_to_show_never_to_open_a_file() {
        let file = Path::new("/data/export.json");
        let folder = Path::new("/data/logs");
        let bundle = Path::new("/Applications/Evil.app");
        let show = |os, path: &Path, is_dir| {
            let (program, args) = reveal_command(os, path, is_dir);
            let args: Vec<_> = args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            (program, args)
        };
        assert_eq!(
            show(HostOs::MacOs, file, false),
            (
                "open",
                vec!["-R".into(), "--".into(), "/data/export.json".into()]
            )
        );
        assert_eq!(
            show(HostOs::MacOs, folder, true),
            ("open", vec!["--".into(), "/data/logs".into()])
        );
        assert_eq!(
            show(HostOs::MacOs, bundle, true).1[0],
            "-R",
            "a bundle is selected, never launched"
        );
        assert_eq!(
            show(HostOs::Windows, file, false),
            ("explorer", vec!["/select,/data/export.json".into()])
        );
        assert_eq!(
            show(HostOs::Windows, folder, true),
            ("explorer", vec!["/data/logs".into()])
        );
        assert_eq!(
            show(HostOs::Linux, file, false),
            ("xdg-open", vec!["/data".into()])
        );
        assert_eq!(
            show(HostOs::Linux, folder, true),
            ("xdg-open", vec!["/data/logs".into()])
        );
    }

    #[test]
    fn data_files_and_chosen_exports_may_be_shown_and_nothing_else() {
        let root = temp_root("allowed");
        let state = state_in(&root);
        let logs = root.join("data").join("logs");
        assert_eq!(
            reveal_path_validate(&state, logs.to_str().unwrap()).unwrap(),
            logs.canonicalize().unwrap()
        );

        let outside = root.join("outside.json");
        std::fs::write(&outside, "{}").unwrap();
        let refused = reveal_path_validate(&state, outside.to_str().unwrap()).unwrap_err();
        assert!(
            matches!(refused, AppError::Validation { .. }),
            "{refused:?}"
        );

        state.note_user_file(&outside);
        assert!(reveal_path_validate(&state, outside.to_str().unwrap()).is_ok());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn malformed_relative_missing_and_escaping_paths_are_refused() {
        let root = temp_root("refused");
        let state = state_in(&root);
        let data = root.join("data");
        for bad in ["", "   ", "relative/path", "../x", "/tmp/a\u{0}b"] {
            let error = reveal_path_validate(&state, bad).unwrap_err();
            assert!(
                matches!(error, AppError::Validation { .. }),
                "{bad:?}: {error:?}"
            );
        }
        let missing = data.join("logs").join("gone.log");
        assert!(matches!(
            reveal_path_validate(&state, missing.to_str().unwrap()),
            Err(AppError::NotFound { .. })
        ));
        // `..` out of the data folder resolves outside and is refused.
        let sneaky = data.join("logs").join("..").join("..");
        assert!(matches!(
            reveal_path_validate(&state, sneaky.to_str().unwrap()),
            Err(AppError::Validation { .. })
        ));
        let too_long = format!("/{}", "a".repeat(MAX_PATH_BYTES));
        assert!(reveal_path_validate(&state, &too_long).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_link_inside_the_data_folder_cannot_lead_out_of_it() {
        let root = temp_root("link");
        let state = state_in(&root);
        let secret_dir = root.join("private");
        std::fs::create_dir_all(&secret_dir).unwrap();
        let link = root.join("data").join("logs").join("escape");
        std::os::unix::fs::symlink(&secret_dir, &link).unwrap();
        assert!(matches!(
            reveal_path_validate(&state, link.to_str().unwrap()),
            Err(AppError::Validation { .. })
        ));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn skill_sync_and_chat_folders_may_be_shown() {
        use serde_json::json;
        let root = temp_root("folders");
        let state = state_in(&root);
        let skills = root.join("skills-here");
        let sync = root.join("sync-here");
        let chat = root.join("chat-here");
        for dir in [&skills, &sync, &chat] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let path = |dir: &Path| dir.to_str().unwrap().to_owned();
        crate::app::settings_update_impl(
            &state,
            crate::settings::SettingsSection::Sync,
            &json!({ "directory": path(&sync) }),
        )
        .unwrap();
        crate::app::settings_update_impl(
            &state,
            crate::settings::SettingsSection::Ai,
            &json!({ "agent": { "skill_dirs": [path(&skills)] } }),
        )
        .unwrap();
        state
            .store()
            .connection()
            .execute(
                "INSERT INTO agent_sessions (id, title, created_at_ms, updated_at_ms, workspace_dir)
                 VALUES ('s1', 't', 0, 0, ?1)",
                [path(&chat)],
            )
            .unwrap();
        for dir in [&skills, &sync, &chat] {
            assert!(
                reveal_path_validate(&state, dir.to_str().unwrap()).is_ok(),
                "{dir:?}"
            );
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_user_file_list_forgets_the_oldest() {
        let files = UserFiles::default();
        for index in 0..=USER_FILES_KEPT {
            files.note(PathBuf::from(format!("/f/{index}")));
        }
        let kept = files.snapshot();
        assert_eq!(kept.len(), USER_FILES_KEPT);
        assert_eq!(kept[0], PathBuf::from("/f/1"));
        files.note(PathBuf::from("/f/1"));
        assert_eq!(files.snapshot().last(), Some(&PathBuf::from("/f/1")));
        assert_eq!(files.snapshot().len(), USER_FILES_KEPT);
    }
}
