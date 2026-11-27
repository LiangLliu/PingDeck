//! The environment a tool reads besides its config files: the variables that move its config
//! directory (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GEMINI_CLI_HOME`), what sets values over the
//! file SpanBay wrote (a shell export, a project `.env` that shadows Gemini CLI's global one, a
//! Codex profile that selects another provider), the project folders the tools were used in, and
//! the dotenv reading and upserting the Gemini CLI writer shares.
//!
//! An app started from Finder does not inherit the user's shell, so the process environment is
//! not enough: the shell start-up files are scanned read-only for literal assignments. No shell is
//! ever run; a value built from a command substitution is ignored.

use std::fs;
use std::path::{Path, PathBuf};

use super::gemini_cli::{API_KEY_VAR, BASE_URL_VAR, MODEL_VAR};
use super::{ClientApp, parse_json_object, parse_toml, read_optional};

/// Most project folders [`recent_project_dirs`] returns.
const MAX_PROJECT_DIRS: usize = 64;
/// Most folders walked up from a project when looking for the `.env` Gemini CLI loads.
const MAX_ENV_DEPTH: usize = 64;

/// The variable that moves `app`'s config directory; `None` for a tool that has none.
const fn config_dir_var(app: ClientApp) -> Option<&'static str> {
    match app {
        ClientApp::ClaudeCode => Some("CLAUDE_CONFIG_DIR"),
        ClientApp::Codex => Some("CODEX_HOME"),
        ClientApp::GeminiCli => Some("GEMINI_CLI_HOME"),
        ClientApp::OpenCode => None,
    }
}

/// Shell start-up files scanned for exports, relative to the home directory. Later files win, so
/// zsh's files are in the order a login shell reads them (`.zprofile`, then `.zshrc`).
const SHELL_FILES: [&str; 6] = [
    ".zprofile",
    ".zshrc",
    ".bashrc",
    ".bash_profile",
    ".profile",
    ".config/fish/config.fish",
];

/// Variables whose shell value wins over the file SpanBay writes (dotenv never overrides an
/// exported variable). Claude Code applies its `settings.json` `env` over the shell, Codex and
/// OpenCode read only their own files, so only Gemini CLI has any.
fn overriding_vars(app: ClientApp) -> &'static [&'static str] {
    match app {
        ClientApp::GeminiCli => &[API_KEY_VAR, BASE_URL_VAR, MODEL_VAR],
        ClientApp::ClaudeCode | ClientApp::Codex | ClientApp::OpenCode => &[],
    }
}

/// One literal assignment found in a shell start-up file.
///
/// Deliberately not `Debug`: `value` may be the user's real API key (`GEMINI_API_KEY`).
struct ShellExport {
    /// The variable.
    name: String,
    /// Its value, `~` and `$HOME` expanded.
    value: String,
    /// The file it is in (`~/.zshrc`).
    file: String,
}

/// `value` trimmed and without one pair of matching quotes (`"x"` or `'x'`).
fn unquote(value: &str) -> &str {
    let value = value.trim();
    ['"', '\'']
        .into_iter()
        .find_map(|q| value.strip_prefix(q)?.strip_suffix(q))
        .unwrap_or(value)
}

/// `value` without one pair of quotes, `~/` and `$HOME` expanded; `None` for a value that needs a
/// shell (command substitution, other variables).
fn literal(value: &str, home: &Path) -> Option<String> {
    let value = unquote(value);
    let home = home.to_string_lossy();
    let expanded = if let Some(rest) = value.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else {
        expand_home(value, &home)
    };
    let needs_shell = expanded.contains('$') || expanded.contains('`') || expanded.is_empty();
    (!needs_shell).then_some(expanded)
}

/// `value` with `${HOME}`, and `$HOME` where no identifier character follows it, replaced by
/// `home`: `$HOMEBREW_PREFIX` is another variable and stays as it is.
fn expand_home(value: &str, home: &str) -> String {
    const VAR: &str = "$HOME";
    let value = value.replace("${HOME}", home);
    let mut out = String::with_capacity(value.len());
    let mut rest = value.as_str();
    while let Some(at) = rest.find(VAR) {
        out.push_str(&rest[..at]);
        let after = &rest[at + VAR.len()..];
        let longer_name = after
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_');
        out.push_str(if longer_name { VAR } else { home });
        rest = after;
    }
    out.push_str(rest);
    out
}

/// The name and raw value of a `export NAME=value`, `NAME=value` or fish `set -gx NAME value`
/// line.
fn assignment(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    if let Some(mut rest) = line.strip_prefix("set ") {
        // Word by word past the flags; the value is whatever follows the name.
        loop {
            rest = rest.trim_start();
            let (word, after) = rest.split_at(rest.find(char::is_whitespace).unwrap_or(rest.len()));
            if word.is_empty() {
                return None;
            }
            if !word.starts_with('-') {
                return Some((word, after.trim()));
            }
            rest = after;
        }
    }
    let line = line.strip_prefix("export ").unwrap_or(line);
    let (name, value) = line.split_once('=')?;
    let name = name.trim();
    let valid = !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    valid.then_some((name, value))
}

/// The literal assignments of `names` in the shell start-up files under `home`; per variable the
/// last one read wins (files in [`SHELL_FILES`] order).
fn shell_exports(home: &Path, names: &[&str]) -> Vec<ShellExport> {
    let mut found: Vec<ShellExport> = Vec::new();
    for file in SHELL_FILES {
        let Ok(text) = fs::read_to_string(home.join(file)) else {
            continue;
        };
        for (name, raw) in text.lines().filter_map(assignment) {
            if !names.contains(&name) {
                continue;
            }
            let Some(value) = literal(raw, home) else {
                continue;
            };
            found.retain(|export| export.name != name);
            found.push(ShellExport {
                name: name.to_owned(),
                value,
                file: format!("~/{file}"),
            });
        }
    }
    found
}

/// Where `app`'s config directory moved, and what moved it (`environment` or `~/.zshrc`): the
/// process environment first, then the shell files. Only an absolute path counts; Gemini CLI's
/// variable names the home that holds `.gemini`. `process` reads the process environment.
fn resolve_dir(
    home: &Path,
    app: ClientApp,
    process: &dyn Fn(&str) -> Option<String>,
) -> Option<(PathBuf, String)> {
    let var = config_dir_var(app)?;
    let (value, source) = match process(var).filter(|value| !value.trim().is_empty()) {
        Some(value) => (literal(&value, home)?, "environment".to_owned()),
        None => {
            let export = shell_exports(home, &[var]).pop()?;
            (export.value, export.file)
        }
    };
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return None;
    }
    let dir = if app == ClientApp::GeminiCli {
        path.join(".gemini")
    } else {
        path
    };
    Some((dir, source))
}

/// `default`, or the directory a variable moved it to. Variables only apply to the user's real
/// home: a test's temporary home never follows the machine's variables (so a test can never
/// write into the user's real `~/.claude`).
pub(super) fn tool_dir(home: &Path, app: ClientApp, default: PathBuf) -> PathBuf {
    let real = super::home_dir().is_ok_and(|real| real == home);
    if !real {
        return default;
    }
    resolve_dir(home, app, &|name| std::env::var(name).ok()).map_or(default, |(dir, _)| dir)
}

/// What makes `app` not use what SpanBay wrote, `None` when nothing does:
/// - Gemini CLI loads only the first `.env` it finds walking up from the folder it runs in, so a
///   `.env` (or `.gemini/.env`) in or above one of `project_dirs` shadows the global one SpanBay
///   writes (`~/proj/.env: shadows ~/.gemini/.env`) unless it points at the same gateway; and the
///   process environment or a shell export of its variables wins over any `.env`
///   (`~/.zshrc: GOOGLE_GEMINI_BASE_URL`).
/// - Codex: the default `profile` selects a profile whose `model_provider` is another provider
///   (`~/.codex/config.toml: profiles.work.model_provider`).
#[must_use]
pub fn override_source(
    home: &Path,
    app: ClientApp,
    process: &dyn Fn(&str) -> Option<String>,
    project_dirs: &[PathBuf],
) -> Option<String> {
    match app {
        ClientApp::GeminiCli => {}
        ClientApp::Codex => return codex_profile_override(home),
        ClientApp::ClaudeCode | ClientApp::OpenCode => return None,
    }
    if let Some(shadow) = gemini_env_shadow(home, project_dirs) {
        return Some(shadow);
    }
    let names = overriding_vars(app);
    if let Some(name) = names.iter().find(|name| process(name).is_some()) {
        return Some(format!("environment: {name}"));
    }
    shell_exports(home, names)
        .into_iter()
        .next()
        .map(|export| format!("{}: {}", export.file, export.name))
}

/// The first `.env` Gemini CLI would load in one of `project_dirs` when it is not the one
/// SpanBay writes and does not point at the same gateway.
fn gemini_env_shadow(home: &Path, project_dirs: &[PathBuf]) -> Option<String> {
    let ours = super::gemini_cli::env_path(home);
    let our_text = fs::read_to_string(&ours).unwrap_or_default();
    let our_base = dotenv_value(&our_text, BASE_URL_VAR);
    for project in project_dirs {
        for dir in project.ancestors().take(MAX_ENV_DEPTH) {
            // Gemini CLI looks in `.gemini/.env`, then `.env`, of every folder on the way up; the
            // home folder is where it finds the global file SpanBay writes.
            let found = [dir.join(".gemini").join(".env"), dir.join(".env")]
                .into_iter()
                .find(|file| file.is_file());
            let Some(file) = found else {
                if dir == home {
                    break;
                }
                continue;
            };
            if file == ours {
                break;
            }
            let text = fs::read_to_string(&file).unwrap_or_default();
            let same_gateway = our_base.is_some()
                && dotenv_value(&text, BASE_URL_VAR) == our_base
                && dotenv_value(&text, API_KEY_VAR).is_some_and(|key| !key.trim().is_empty());
            if same_gateway {
                break;
            }
            return Some(format!(
                "{}: shadows {}",
                super::display_in_home(home, &file),
                super::display_in_home(home, &ours)
            ));
        }
    }
    None
}

/// The Codex profile that selects another provider than SpanBay's by default.
fn codex_profile_override(home: &Path) -> Option<String> {
    let path = super::codex::config_path(home);
    let text = read_optional(&path).ok()??;
    let doc = parse_toml(home, &path, &text).ok()?;
    let profile = doc.get("profile")?.as_str()?;
    let provider = doc
        .get("profiles")?
        .get(profile)?
        .get("model_provider")?
        .as_str()?;
    (provider != "spanbay").then(|| {
        format!(
            "{}: profiles.{profile}.model_provider",
            super::display_in_home(home, &path)
        )
    })
}

/// The project folders the coding tools were used in, read from their own files: Claude Code's
/// `~/.claude.json` (`projects`), Codex's `config.toml` (`[projects."<path>"]`) and Gemini CLI's
/// `trustedFolders.json`. Only existing absolute folders, each once, at most 64.
#[must_use]
pub fn recent_project_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut add = |path: &str| {
        let dir = PathBuf::from(path);
        if dirs.len() < MAX_PROJECT_DIRS
            && dir.is_absolute()
            && dir.is_dir()
            && !dirs.contains(&dir)
        {
            dirs.push(dir);
        }
    };
    let json_keys = |path: PathBuf, field: Option<&str>| -> Vec<String> {
        let Ok(Some(text)) = read_optional(&path) else {
            return Vec::new();
        };
        let Ok(root) = parse_json_object(home, &path, &text) else {
            return Vec::new();
        };
        let object = match field {
            Some(field) => root.get(field).and_then(|value| value.as_object()).cloned(),
            None => Some(root),
        };
        object
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default()
    };
    for key in json_keys(home.join(".claude.json"), Some("projects")) {
        add(&key);
    }
    let gemini_dir = super::ClientApp::GeminiCli.dir(home);
    for key in json_keys(gemini_dir.join("trustedFolders.json"), None) {
        add(&key);
    }
    let codex = super::codex::config_path(home);
    if let Ok(Some(text)) = read_optional(&codex)
        && let Ok(doc) = parse_toml(home, &codex, &text)
        && let Some(projects) = doc.get("projects").and_then(|item| item.as_table_like())
    {
        for (key, _) in projects.iter() {
            add(key);
        }
    }
    dirs
}

/// The value of the last `KEY=` assignment of a dotenv text (an `export ` prefix and one pair of
/// quotes are ignored).
pub(super) fn dotenv_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (name, value) = line.split_once('=')?;
            (name.trim() == key).then(|| unquote(value))
        })
        .next_back()
}

/// Replaces (every) `KEY=` line of a dotenv text or appends one, keeping all other lines and the
/// file's line ending. Assignments may carry an `export ` prefix.
pub(super) fn upsert_dotenv(text: &str, key: &str, value: &str) -> String {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let line = format!("{key}={value}");
    let mut found = false;
    let mut lines: Vec<String> = text
        .lines()
        .map(|existing| {
            let trimmed = existing.trim_start();
            let name = trimmed.strip_prefix("export ").unwrap_or(trimmed);
            if name
                .trim_start()
                .strip_prefix(key)
                .is_some_and(|rest| rest.trim_start().starts_with('='))
            {
                found = true;
                line.clone()
            } else {
                existing.to_owned()
            }
        })
        .collect();
    if !found {
        lines.push(line);
    }
    lines.join(eol) + eol
}

#[cfg(test)]
mod tests {
    use super::super::test_util::TempHome;
    use super::*;

    #[test]
    fn each_config_dir_variable_moves_its_tool_from_the_environment_or_a_shell_file() {
        let home = TempHome::new();
        let abs = home.0.join("elsewhere");
        let value = abs.to_string_lossy().into_owned();
        let from_env = |name: &str| (name != "CODEX_HOME").then(|| value.clone());
        assert_eq!(
            resolve_dir(&home.0, ClientApp::ClaudeCode, &from_env),
            Some((abs.clone(), "environment".to_owned()))
        );
        assert_eq!(
            resolve_dir(&home.0, ClientApp::GeminiCli, &from_env).map(|(dir, _)| dir),
            Some(abs.join(".gemini"))
        );
        assert_eq!(resolve_dir(&home.0, ClientApp::OpenCode, &from_env), None);
        let none = |_: &str| None;
        assert_eq!(resolve_dir(&home.0, ClientApp::Codex, &none), None);
        home.write(
            ".zshrc",
            "# export CODEX_HOME=/no\nexport CODEX_HOME=\"$HOME/cx\"\nexport CODEX_HOME=$(pwd)\n",
        );
        assert_eq!(
            resolve_dir(&home.0, ClientApp::Codex, &none),
            Some((home.0.join("cx"), "~/.zshrc".to_owned()))
        );
        let relative = |_: &str| Some("rel/dir".to_owned());
        assert_eq!(resolve_dir(&home.0, ClientApp::ClaudeCode, &relative), None);
    }

    #[test]
    fn only_the_home_variable_itself_expands() {
        let home = Path::new("/Users/me");
        let lit = |value: &str| literal(value, home);
        assert_eq!(lit("$HOME/cx").as_deref(), Some("/Users/me/cx"));
        assert_eq!(lit("${HOME}/cx").as_deref(), Some("/Users/me/cx"));
        assert_eq!(lit("$HOME").as_deref(), Some("/Users/me"));
        assert_eq!(
            lit("$HOME.d:$HOME").as_deref(),
            Some("/Users/me.d:/Users/me")
        );
        // A longer variable that starts with `HOME` needs a shell.
        assert_eq!(lit("$HOMEBREW_PREFIX/etc/codex"), None);
        assert_eq!(lit("$HOME_2/x"), None);
        assert_eq!(lit("$HOME/a/$HOMEDIR"), None);
    }

    #[test]
    fn a_fish_set_value_starts_after_the_name_not_inside_a_flag() {
        assert_eq!(assignment("set -x x 1"), Some(("x", "1")));
        assert_eq!(
            assignment("set -gx CODEX_HOME  ~/cx "),
            Some(("CODEX_HOME", "~/cx"))
        );
        assert_eq!(
            assignment("set --export --global GEMINI_MODEL m"),
            Some(("GEMINI_MODEL", "m"))
        );
        assert_eq!(assignment("set -gx"), None);
    }

    #[test]
    fn zshrc_wins_over_zprofile_as_in_a_login_shell() {
        let home = TempHome::new();
        home.write(".zshrc", "export CODEX_HOME=\"$HOME/from-zshrc\"\n");
        home.write(".zprofile", "export CODEX_HOME=\"$HOME/from-zprofile\"\n");
        assert_eq!(
            resolve_dir(&home.0, ClientApp::Codex, &|_| None),
            Some((home.0.join("from-zshrc"), "~/.zshrc".to_owned()))
        );
    }

    #[test]
    fn a_project_env_or_a_shell_export_overrides_gemini_but_not_claude_code() {
        let home = TempHome::new();
        let none = |_: &str| None;
        home.write(
            ".gemini/.env",
            "GEMINI_API_KEY=pd\nGOOGLE_GEMINI_BASE_URL=http://gw\n",
        );
        home.write("proj/src/main.rs", "");
        let projects = [home.0.join("proj/src")];
        // The walk up from the project reaches the global file: nothing shadows it.
        assert_eq!(
            override_source(&home.0, ClientApp::GeminiCli, &none, &projects),
            None
        );
        // Any `.env` on the way up shadows the global one, whatever it holds.
        home.write("proj/.env", "OTHER=1\n");
        assert_eq!(
            override_source(&home.0, ClientApp::GeminiCli, &none, &projects).as_deref(),
            Some("~/proj/.env: shadows ~/.gemini/.env")
        );
        // One that points at the same gateway with a key does not.
        home.write(
            "proj/.env",
            "GEMINI_API_KEY=other\nGOOGLE_GEMINI_BASE_URL=http://gw\n",
        );
        assert_eq!(
            override_source(&home.0, ClientApp::GeminiCli, &none, &projects),
            None
        );
        home.write(
            ".config/fish/config.fish",
            "set -gx GOOGLE_GEMINI_BASE_URL http://x\n",
        );
        assert_eq!(
            override_source(&home.0, ClientApp::GeminiCli, &none, &[]).as_deref(),
            Some("~/.config/fish/config.fish: GOOGLE_GEMINI_BASE_URL")
        );
        assert_eq!(
            override_source(&home.0, ClientApp::ClaudeCode, &none, &projects),
            None
        );
    }

    #[test]
    fn a_codex_profile_with_another_provider_overrides_the_top_level_one() {
        let home = TempHome::new();
        let none = |_: &str| None;
        let config = "model_provider = \"spanbay\"\nprofile = \"work\"\n\n[profiles.work]\nmodel_provider = \"openai\"\n";
        home.write(".codex/config.toml", config);
        assert_eq!(
            override_source(&home.0, ClientApp::Codex, &none, &[]).as_deref(),
            Some("~/.codex/config.toml: profiles.work.model_provider")
        );
        home.write(
            ".codex/config.toml",
            &config.replace("\"openai\"", "\"spanbay\""),
        );
        assert_eq!(override_source(&home.0, ClientApp::Codex, &none, &[]), None);
    }

    #[test]
    fn project_folders_come_from_the_tools_own_files() {
        let home = TempHome::new();
        for dir in ["a", "b", "c"] {
            home.write(&format!("{dir}/x"), "");
        }
        let abs = |dir: &str| home.0.join(dir).to_string_lossy().into_owned();
        home.write(
            ".claude.json",
            &format!(
                "{{\"projects\":{{\"{}\":{{}},\"{}\":{{}},\"/no/such/dir\":{{}}}}}}",
                abs("a"),
                abs("b")
            ),
        );
        home.write(
            ".codex/config.toml",
            &format!("[projects.\"{}\"]\ntrust_level = \"trusted\"\n", abs("c")),
        );
        home.write(
            ".gemini/trustedFolders.json",
            &format!("{{\"{}\": \"TRUST_FOLDER\"}}", abs("a")),
        );
        let mut dirs = recent_project_dirs(&home.0);
        dirs.sort();
        assert_eq!(dirs, [home.0.join("a"), home.0.join("b"), home.0.join("c")]);
    }

    #[test]
    fn dotenv_upsert_keeps_line_endings_and_appends_without_newline() {
        assert_eq!(upsert_dotenv("A=1", "B", "2"), "A=1\nB=2\n");
        assert_eq!(upsert_dotenv("A=1\r\nB=0\r\n", "B", "2"), "A=1\r\nB=2\r\n");
        assert_eq!(upsert_dotenv("", "B", "2"), "B=2\n");
        // A longer key sharing the prefix is not ours.
        assert_eq!(upsert_dotenv("BB=1\n", "B", "2"), "BB=1\nB=2\n");
    }

    #[test]
    fn dotenv_value_takes_the_last_assignment_and_strips_quotes() {
        assert_eq!(dotenv_value("A=1\nA=2\n", "A"), Some("2"));
        assert_eq!(dotenv_value("export A=\"x y\"\n", "A"), Some("x y"));
        assert_eq!(dotenv_value("A='q'", "A"), Some("q"));
        assert_eq!(dotenv_value("AB=1\n# A=2\n", "A"), None);
    }
}
