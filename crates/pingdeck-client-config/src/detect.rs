//! Detection of installed tools: file-writer tools by config directory or executable on `PATH`,
//! snippet clients by macOS app bundle or config directory. Informational only: an undetected
//! tool can still be configured.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{ClientApp, display, read_optional, snippets::SnippetClient};

/// The places detection looks, injectable so tests never depend on the real machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectEnv {
    /// The user's home directory.
    pub home: PathBuf,
    /// Directories searched for executables (the entries of `PATH`).
    pub path_dirs: Vec<PathBuf>,
    /// Directories searched for `*.app` bundles (macOS: `/Applications`, `~/Applications`).
    pub app_dirs: Vec<PathBuf>,
}

impl DetectEnv {
    /// The real machine: `$PATH` and, on macOS, the two application folders.
    #[must_use]
    pub fn system(home: &Path) -> Self {
        let path_dirs = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default();
        let app_dirs = if cfg!(target_os = "macos") {
            vec![PathBuf::from("/Applications"), home.join("Applications")]
        } else {
            Vec::new()
        };
        Self {
            home: home.to_owned(),
            path_dirs,
            app_dirs,
        }
    }

    /// Only `home`: no `PATH` and no app folders, so detection reduces to the config directories.
    #[must_use]
    pub fn home_only(home: &Path) -> Self {
        Self {
            home: home.to_owned(),
            path_dirs: Vec::new(),
            app_dirs: Vec::new(),
        }
    }

    /// `true` when `name` (plus `.exe`/`.cmd`/`.bat` on Windows) is a file in a `PATH` directory.
    fn has_executable(&self, name: &str) -> bool {
        let candidates: &[&str] = if cfg!(windows) {
            &["", ".exe", ".cmd", ".bat"]
        } else {
            &[""]
        };
        self.path_dirs.iter().any(|dir| {
            candidates
                .iter()
                .any(|ext| dir.join(format!("{name}{ext}")).is_file())
        })
    }

    /// `true` when `bundle` (for example `Zed.app`) exists in an application folder.
    fn has_bundle(&self, bundle: &str) -> bool {
        self.app_dirs.iter().any(|dir| dir.join(bundle).is_dir())
    }
}

/// Whether a tool looks installed on this machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectedClient {
    /// The tool.
    pub app: ClientApp,
    /// `true` when the tool's config directory exists or its executable is on `PATH`.
    pub installed: bool,
    /// Every file this writer manages for the tool, whether or not it exists yet.
    pub config_paths: Vec<String>,
    /// The worst state of the tool's config files (see [`ConfigState`]).
    pub config_state: ConfigState,
    /// The first config file that is a dangling symlink, with where it points.
    #[serde(default)]
    pub broken_link: Option<BrokenLink>,
}

/// Whether a snippet-only client looks installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedSnippetClient {
    /// The client.
    pub client: SnippetClient,
    /// `true` when its app bundle or config directory exists.
    pub installed: bool,
}

/// `true` when `app`'s config directory exists or its executable is on `PATH`.
pub(super) fn is_installed(env: &DetectEnv, app: ClientApp) -> bool {
    app.dir(&env.home).is_dir() || env.has_executable(app.executable())
}

/// How a tool's config files stand, worst last: a tool is only as healthy as its worst file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigState {
    /// No config file exists yet.
    Missing,
    /// At least one config file exists and reads as UTF-8 text.
    Ok,
    /// A config file cannot be read: permission denied, invalid UTF-8, not a regular file, larger
    /// than PingDeck reads, any I/O error.
    Unreadable,
    /// A config file is a symlink that points nowhere (apply refuses to write it).
    BrokenLink,
}

/// A dangling symlink at a config path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokenLink {
    /// The config file (display form).
    pub path: String,
    /// What the link points at (display form, as written in the link).
    pub target: String,
}

/// The state of one config file, with the link when it dangles. The dangling-link rule is the one
/// the write planner refuses on (`symlink_metadata` says link, the target does not resolve).
fn file_state(path: &Path) -> (ConfigState, Option<BrokenLink>) {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (ConfigState::Missing, None),
        Err(_) => return (ConfigState::Unreadable, None),
    };
    if meta.file_type().is_symlink()
        && matches!(fs::metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
    {
        return match fs::read_link(path) {
            Ok(target) => (
                ConfigState::BrokenLink,
                Some(BrokenLink {
                    path: display(path),
                    target: display(&target),
                }),
            ),
            Err(_) => (ConfigState::Unreadable, None),
        };
    }
    // The guarded read: a FIFO is refused without opening it, an oversized file unread.
    match read_optional(path) {
        Ok(Some(_)) => (ConfigState::Ok, None),
        Ok(None) => (ConfigState::Missing, None),
        Err(_) => (ConfigState::Unreadable, None),
    }
}

/// The worst [`ConfigState`] over `files` and the first dangling link among them.
fn config_state(files: &[PathBuf]) -> (ConfigState, Option<BrokenLink>) {
    let mut worst = ConfigState::Missing;
    let mut broken = None;
    for file in files {
        let (state, link) = file_state(file);
        worst = worst.max(state);
        broken = broken.or(link);
    }
    (worst, broken)
}

/// Reports, for every file-writer tool, whether it looks installed (its config directory exists
/// or its executable is on `PATH`) and the state of its config files.
#[must_use]
pub fn detect_all(env: &DetectEnv) -> Vec<DetectedClient> {
    ClientApp::ALL
        .into_iter()
        .map(|app| {
            let files = app.files(&env.home);
            let (config_state, broken_link) = config_state(&files);
            DetectedClient {
                app,
                installed: is_installed(env, app),
                config_paths: files.iter().map(|p| display(p)).collect(),
                config_state,
                broken_link,
            }
        })
        .collect()
}

/// Reports, for Cursor, Zed and Cherry Studio, whether the app bundle or config directory exists.
/// [`SnippetClient::GenericEnv`] is not an application and is not listed.
#[must_use]
pub fn detect_snippet_clients(env: &DetectEnv) -> Vec<DetectedSnippetClient> {
    SnippetClient::ALL
        .into_iter()
        .filter_map(|client| {
            let bundle = client.app_bundle()?;
            let config_dir = client.config_dir(&env.home);
            Some(DetectedSnippetClient {
                client,
                installed: env.has_bundle(bundle) || config_dir.is_some_and(|dir| dir.is_dir()),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::super::test_util::TempHome;
    use super::*;

    fn installed(env: &DetectEnv) -> Vec<(ClientApp, bool)> {
        detect_all(env)
            .into_iter()
            .map(|d| (d.app, d.installed))
            .collect()
    }

    #[test]
    fn an_empty_machine_detects_nothing_but_lists_the_paths() {
        let home = TempHome::new();
        let all = detect_all(&DetectEnv::home_only(&home.0));
        assert_eq!(all.len(), 4);
        assert!(
            all.iter()
                .all(|c| !c.installed && !c.config_paths.is_empty())
        );
        assert_eq!(all[3].config_paths.len(), 2);
        assert!(all[3].config_paths[1].ends_with(".env"));
    }

    #[test]
    fn only_codex_dir_detects_only_codex() {
        let home = TempHome::new();
        fs::create_dir_all(home.0.join(".codex")).unwrap();
        let found = installed(&DetectEnv::home_only(&home.0));
        assert_eq!(
            found,
            vec![
                (ClientApp::ClaudeCode, false),
                (ClientApp::Codex, true),
                (ClientApp::OpenCode, false),
                (ClientApp::GeminiCli, false),
            ]
        );
    }

    #[test]
    fn every_config_dir_counts() {
        let home = TempHome::new();
        for dir in [".claude", ".codex", ".config/opencode", ".gemini"] {
            fs::create_dir_all(home.0.join(dir)).unwrap();
        }
        assert!(
            installed(&DetectEnv::home_only(&home.0))
                .iter()
                .all(|(_, yes)| *yes)
        );
    }

    fn state_of(home: &TempHome, app: ClientApp) -> DetectedClient {
        detect_all(&DetectEnv::home_only(&home.0))
            .into_iter()
            .find(|d| d.app == app)
            .unwrap()
    }

    fn first_file(home: &TempHome) -> PathBuf {
        let file = ClientApp::ClaudeCode.files(&home.0).remove(0);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        file
    }

    #[test]
    fn config_state_is_missing_without_files_and_ok_with_a_readable_one() {
        let home = TempHome::new();
        let missing = state_of(&home, ClientApp::ClaudeCode);
        assert_eq!(missing.config_state, ConfigState::Missing);
        assert_eq!(missing.broken_link, None);
        fs::write(first_file(&home), "{}").unwrap();
        assert_eq!(
            state_of(&home, ClientApp::ClaudeCode).config_state,
            ConfigState::Ok
        );
    }

    #[test]
    fn invalid_utf8_or_a_directory_is_unreadable() {
        let home = TempHome::new();
        let file = first_file(&home);
        fs::write(&file, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(
            state_of(&home, ClientApp::ClaudeCode).config_state,
            ConfigState::Unreadable
        );
        fs::remove_file(&file).unwrap();
        fs::create_dir(&file).unwrap();
        assert_eq!(
            state_of(&home, ClientApp::ClaudeCode).config_state,
            ConfigState::Unreadable
        );
    }

    #[test]
    fn an_oversized_config_file_is_unreadable() {
        let home = TempHome::new();
        let file = fs::File::create(first_file(&home)).unwrap();
        file.set_len(super::super::fsio::MAX_CONFIG_BYTES + 1)
            .unwrap();
        assert_eq!(
            state_of(&home, ClientApp::ClaudeCode).config_state,
            ConfigState::Unreadable
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_at_a_config_path_is_unreadable_without_blocking() {
        let home = TempHome::new();
        let made = std::process::Command::new("mkfifo")
            .arg(first_file(&home))
            .status();
        assert!(made.is_ok_and(|status| status.success()), "mkfifo");
        assert_eq!(
            state_of(&home, ClientApp::ClaudeCode).config_state,
            ConfigState::Unreadable
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_is_a_broken_link_with_its_target() {
        let home = TempHome::new();
        let file = first_file(&home);
        std::os::unix::fs::symlink(home.0.join("dotfiles/missing.json"), &file).unwrap();
        let detected = state_of(&home, ClientApp::ClaudeCode);
        assert_eq!(detected.config_state, ConfigState::BrokenLink);
        let link = detected.broken_link.unwrap();
        assert_eq!(link.path, display(&file));
        assert!(link.target.ends_with("dotfiles/missing.json"));
        // A link whose target exists is just a readable file.
        let target = home.0.join("real.json");
        fs::write(&target, "{}").unwrap();
        fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink(&target, &file).unwrap();
        let healthy = state_of(&home, ClientApp::ClaudeCode);
        assert_eq!(healthy.config_state, ConfigState::Ok);
        assert_eq!(healthy.broken_link, None);
    }

    #[test]
    fn an_executable_on_path_detects_the_tool() {
        let home = TempHome::new();
        let bin = home.0.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("claude"), "").unwrap();
        fs::write(bin.join("codexx"), "").unwrap();
        let mut env = DetectEnv::home_only(&home.0);
        env.path_dirs = vec![home.0.join("missing"), bin];
        let found = installed(&env);
        assert_eq!(found[0], (ClientApp::ClaudeCode, true));
        assert_eq!(found[1], (ClientApp::Codex, false));
    }

    #[test]
    fn a_directory_named_like_the_executable_does_not_count() {
        let home = TempHome::new();
        let bin = home.0.join("bin");
        fs::create_dir_all(bin.join("gemini")).unwrap();
        let mut env = DetectEnv::home_only(&home.0);
        env.path_dirs = vec![bin];
        assert!(!installed(&env)[3].1);
    }

    #[test]
    fn snippet_clients_are_detected_by_bundle_or_config_dir() {
        let home = TempHome::new();
        let apps = home.0.join("Applications");
        fs::create_dir_all(apps.join("Cursor.app")).unwrap();
        fs::create_dir_all(home.0.join(".config/zed")).unwrap();
        let mut env = DetectEnv::home_only(&home.0);
        env.app_dirs = vec![PathBuf::from("/nonexistent-apps"), apps];
        let found = detect_snippet_clients(&env);
        assert_eq!(
            found
                .iter()
                .map(|d| (d.client, d.installed))
                .collect::<Vec<_>>(),
            vec![
                (SnippetClient::Cursor, true),
                (SnippetClient::Zed, true),
                (SnippetClient::CherryStudio, false),
            ]
        );
    }

    #[test]
    fn cherry_studio_is_found_by_its_bundle() {
        let home = TempHome::new();
        let apps = home.0.join("apps");
        fs::create_dir_all(apps.join("Cherry Studio.app")).unwrap();
        let mut env = DetectEnv::home_only(&home.0);
        env.app_dirs = vec![apps];
        let found = detect_snippet_clients(&env);
        assert!(found[2].installed && !found[0].installed && !found[1].installed);
    }
}
