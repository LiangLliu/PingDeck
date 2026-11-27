//! Static architecture boundary enforcement (`cargo xtask check-arch`).
//!
//! Enforces strict 4-layer decoupling between Core (`crates/spanbay-*`),
//! UI-agnostic Desktop Host (`crates/spanbay-desktop`), and thin GPUI Renderer
//! (`crates/spanbay-gpui`):
//!   1. Core crates never depend on `spanbay-desktop`, `spanbay-gpui`, `gpui*`, or `tauri*`.
//!   2. `spanbay-desktop` never depends on `gpui*`, `tauri*`, or `objc2*`.
//!   3. Inside `spanbay-gpui/src/`: `gpui_kit::component` / `gpui_component` is quarantined
//!      inside `src/ui/` (and `src/app/` bootstrap); `src/views/` never imports `gpui_kit::component`,
//!      `spanbay_core::storage`, or spawns `tokio::spawn` directly.
//!   4. Non-test view source files under `crates/spanbay-gpui/src/views/` stay <= 600 lines.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use toml_edit::DocumentMut;

use crate::{err_line, out_line};

const MAX_VIEW_LINES: usize = 600;

fn dep_names(manifest_path: &Path) -> Result<Vec<String>, String> {
    let raw = fs::read_to_string(manifest_path)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let doc: DocumentMut = raw
        .parse()
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let mut names = Vec::new();
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = doc.get(section).and_then(|v| v.as_table()) {
            for (k, _) in table {
                names.push(k.to_owned());
            }
        }
    }
    if let Some(targets) = doc.get("target").and_then(|v| v.as_table()) {
        for (_, target_val) in targets {
            if let Some(target_tbl) = target_val.as_table() {
                for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
                    if let Some(table) = target_tbl.get(section).and_then(|v| v.as_table()) {
                        for (k, _) in table {
                            names.push(k.to_owned());
                        }
                    }
                }
            }
        }
    }
    Ok(names)
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn code_lines_before_tests(content: &str) -> usize {
    let mut count = 0usize;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "#[cfg(test)]" {
            break;
        }
        count += 1;
    }
    count
}

fn check_crate_deps(
    crates_dir: &Path,
    crate_name: &str,
    forbidden_prefixes: &[&str],
    problems: &mut Vec<String>,
) {
    let manifest = crates_dir.join(crate_name).join("Cargo.toml");
    if !manifest.exists() {
        return;
    }
    match dep_names(&manifest) {
        Ok(deps) => {
            for dep in deps {
                if forbidden_prefixes
                    .iter()
                    .any(|prefix| dep == *prefix || dep.starts_with(prefix))
                {
                    problems.push(format!(
                        "{}: forbidden dependency `{dep}` violates layer boundary",
                        manifest.display()
                    ));
                }
            }
        }
        Err(e) => problems.push(e),
    }
}

pub fn check_all(root: &Path) -> Result<usize, Vec<String>> {
    let mut problems = Vec::new();
    let crates_dir = root.join("crates");
    let mut checked_crates = 0usize;

    if let Ok(entries) = fs::read_dir(&crates_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || !path.join("Cargo.toml").exists() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            checked_crates += 1;
            match name {
                "spanbay-desktop" => {
                    check_crate_deps(
                        &crates_dir,
                        name,
                        &["gpui", "tauri", "objc2", "spanbay-gpui"],
                        &mut problems,
                    );
                }
                "spanbay-gpui" => {
                    check_crate_deps(&crates_dir, name, &["tauri", "rusqlite"], &mut problems);
                }
                _ => {
                    // Core domain crates: completely UI-agnostic.
                    check_crate_deps(
                        &crates_dir,
                        name,
                        &["spanbay-desktop", "spanbay-gpui", "gpui", "tauri"],
                        &mut problems,
                    );
                }
            }
        }
    }

    // Enforce source-level boundaries inside `crates/spanbay-gpui/src/views`.
    let views_dir = crates_dir.join("spanbay-gpui/src/views");
    if views_dir.exists() {
        let mut files = Vec::new();
        collect_rs_files(&views_dir, &mut files);
        for file in files {
            let Ok(content) = fs::read_to_string(&file) else {
                continue;
            };
            let rel = file.strip_prefix(root).unwrap_or(&file);
            let lines = code_lines_before_tests(&content);
            if lines > MAX_VIEW_LINES {
                problems.push(format!(
                    "{}: {lines} non-test lines exceeds limit {MAX_VIEW_LINES}",
                    rel.display()
                ));
            }
            for (idx, line) in content.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed == "#[cfg(test)]" {
                    break;
                }
                if trimmed.starts_with("//") {
                    continue;
                }
                for (needle, reason) in [
                    (
                        "gpui_kit::component",
                        "views must use `crate::ui::*` adapter instead of `gpui_kit::component` directly",
                    ),
                    (
                        "gpui_component::",
                        "views must use `crate::ui::*` adapter instead of `gpui_component` directly",
                    ),
                    (
                        "spanbay_core::storage",
                        "views must go through `spanbay_desktop` instead of `spanbay_core::storage`",
                    ),
                    (
                        "tokio::spawn",
                        "views must schedule async work through `crate::bridge` / `spanbay_desktop`",
                    ),
                ] {
                    if trimmed.contains(needle) {
                        problems.push(format!("{}:{}: {reason}", rel.display(), idx + 1));
                    }
                }
            }
        }
    }

    if problems.is_empty() {
        Ok(checked_crates)
    } else {
        Err(problems)
    }
}

pub fn run(root: &Path, args: &[String]) -> ExitCode {
    if !args.is_empty() {
        err_line("usage: cargo xtask check-arch");
        return ExitCode::from(2);
    }
    match check_all(root) {
        Ok(n) => {
            out_line(&format!(
                "arch ok: {n} workspace crates satisfy 4-layer UI/core isolation"
            ));
            ExitCode::SUCCESS
        }
        Err(problems) => {
            for p in &problems {
                err_line(&format!("arch error: {p}"));
            }
            err_line(&format!(
                "check-arch failed with {} issue(s)",
                problems.len()
            ));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo_root;

    #[test]
    fn workspace_obeys_layer_isolation() {
        let n = check_all(&repo_root()).expect("workspace crates must obey layer isolation");
        assert!(n >= 17, "expected at least 17 core crates, got {n}");
    }
}
