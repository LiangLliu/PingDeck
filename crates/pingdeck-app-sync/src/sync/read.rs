//! Reading a sync directory: every known file is size-checked, parsed and validated.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::error::AppError;
use crate::settings::ai_from_v1_sections;

use super::docs::{Doc, syncable_section};
use super::export::{MANIFEST, Manifest, fingerprint, read_regular};
use super::{FORMAT_NAME, FORMAT_VERSION, OLDEST_FORMAT_VERSION, SyncKind};

/// The most sync files one read opens (a real export has a few per collection or provider).
const MAX_FILES: usize = 10_000;

/// Whether something sits at `path`, a symlink or special file included (it is then read, and
/// refused, rather than skipped as if absent).
fn present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// One file's JSON and its content fingerprint, or a message that names the file.
fn read_json(dir: &Path, relative: &str) -> Result<(Value, String), String> {
    let bytes =
        read_regular(&dir.join(relative)).map_err(|error| format!("{relative}: {error}"))?;
    let json = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{relative}: invalid JSON: {error}"))?;
    Ok((json, fingerprint(&bytes)))
}

/// The name a cloud client gives the stand-in of a file it has not downloaded yet (iCloud Drive
/// writes `.<name>.icloud`): such a file is in the folder but cannot be read.
fn placeholder_of(name: &str) -> Option<&str> {
    name.strip_prefix('.')?
        .strip_suffix(".icloud")
        .filter(|inner| inner.ends_with(".json"))
}

/// Names of the `*.json` files (or subfolders) directly inside `relative`, sorted; none when the
/// folder does not exist. Any other failure is an error naming the folder: an unreadable folder
/// (permissions, I/O, a cloud stand-in) read as empty would make every entity in it look
/// removed. A symbolic link where a folder belongs, a subfolder that is a link, and a cloud
/// stand-in of a sync file are refused rather than skipped. At most [`MAX_FILES`] + 1 entries are
/// listed.
fn list(dir: &Path, relative: &str, folders: bool) -> Result<Vec<String>, String> {
    let path = dir.join(relative);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{relative}: {error}")),
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(format!("{relative}: is a symbolic link, not a folder"));
        }
        Ok(meta) if !meta.is_dir() => return Err(format!("{relative}: is not a folder")),
        Ok(_) => {}
    }
    let entries = fs::read_dir(&path).map_err(|error| format!("{relative}: {error}"))?;
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("{relative}: {error}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry
            .file_type()
            .map_err(|error| format!("{relative}/{name}: {error}"))?;
        if let Some(inner) = placeholder_of(&name) {
            return Err(format!(
                "{relative}/{inner}: not downloaded yet (a cloud placeholder); download the folder and try again"
            ));
        }
        let wanted = if folders {
            if kind.is_symlink() {
                return Err(format!(
                    "{relative}/{name}: is a symbolic link, not a folder"
                ));
            }
            kind.is_dir()
        } else {
            !kind.is_dir() && name.ends_with(".json") && !name.ends_with(".tmp")
        };
        if wanted {
            names.push(name);
            if names.len() > MAX_FILES {
                break;
            }
        }
    }
    names.sort();
    Ok(names)
}

/// A cloud stand-in sitting where the fixed file `relative` belongs.
fn placeholder_problem(dir: &Path, relative: &str) -> Option<String> {
    let path = Path::new(relative);
    let name = path.file_name()?.to_string_lossy();
    let stand_in = path.with_file_name(format!(".{name}.icloud"));
    present(&dir.join(stand_in)).then(|| {
        format!(
            "{relative}: not downloaded yet (a cloud placeholder); download the folder and try again"
        )
    })
}

/// Reads and validates the whole directory. Any unreadable, oversized, malformed or invalid file
/// fails the read with a message naming every such file: importing around a broken file would
/// make its entity look removed.
pub(crate) fn read_dir(dir: &Path) -> Result<Vec<Doc>, AppError> {
    if !dir.is_dir() {
        return Err(AppError::not_found(format!(
            "sync directory {} does not exist",
            dir.display()
        )));
    }
    if let Some(problem) = placeholder_problem(dir, MANIFEST) {
        return Err(AppError::validation(problem));
    }
    let manifest = match read_json(dir, MANIFEST) {
        Ok((value, _)) => serde_json::from_value::<Manifest>(value)
            .map_err(|error| AppError::validation(format!("{MANIFEST}: {error}")))?,
        Err(message) => {
            let text = format!(
                "{} is not a PingDeck sync directory ({message})",
                dir.display()
            );
            // Only a missing manifest means "no export here yet"; callers tell it apart from an
            // unreadable or broken one by this kind.
            return Err(if present(&dir.join(MANIFEST)) {
                AppError::validation(text)
            } else {
                AppError::not_found(text)
            });
        }
    };
    if manifest.format == FORMAT_NAME && manifest.version > FORMAT_VERSION {
        return Err(AppError::validation(format!(
            "{MANIFEST}: written by a newer PingDeck (format version {}, this one reads {FORMAT_VERSION}): update PingDeck to import it",
            manifest.version
        )));
    }
    if manifest.format != FORMAT_NAME || manifest.version < OLDEST_FORMAT_VERSION {
        return Err(AppError::validation(format!(
            "{MANIFEST}: unsupported format {} version {} (expected {FORMAT_NAME} version {FORMAT_VERSION})",
            manifest.format, manifest.version
        )));
    }

    let mut docs: Vec<Doc> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    let mut origin: HashMap<(SyncKind, String), String> = HashMap::new();
    let mut reads = 0_usize;
    let mut add =
        |kind: SyncKind, relative: String, docs: &mut Vec<Doc>, problems: &mut Vec<String>| {
            reads += 1;
            if reads > MAX_FILES {
                if reads == MAX_FILES + 1 {
                    problems.push(format!(
                        "more than {MAX_FILES} files; the rest were not read"
                    ));
                }
                return;
            }
            match read_json(dir, &relative).and_then(|(raw, fingerprint)| {
                Doc::from_value(kind, raw)
                    .map(|doc| (doc, fingerprint))
                    .map_err(|e| format!("{relative}: {e}"))
            }) {
                Ok((mut doc, fingerprint)) => {
                    if let Some(first) = origin.insert((kind, doc.key.clone()), relative.clone()) {
                        problems.push(format!(
                            "{relative}: duplicates {} {} from {first}",
                            doc.name, doc.key
                        ));
                    } else {
                        doc.file = Some(relative);
                        doc.fingerprint = Some(fingerprint);
                        docs.push(doc);
                    }
                }
                Err(message) => problems.push(message),
            }
        };

    let listed = |relative: &str, folders: bool, problems: &mut Vec<String>| {
        list(dir, relative, folders).unwrap_or_else(|problem| {
            problems.push(problem);
            Vec::new()
        })
    };
    for workspace in listed("workspaces", true, &mut problems) {
        let base = format!("workspaces/{workspace}");
        for (file, kind) in [
            ("workspace.json", SyncKind::Workspace),
            ("globals.json", SyncKind::Globals),
            ("requests.json", SyncKind::LooseRequests),
        ] {
            let relative = format!("{base}/{file}");
            if present(&dir.join(&relative)) {
                add(kind, relative, &mut docs, &mut problems);
            } else if let Some(problem) = placeholder_problem(dir, &relative) {
                problems.push(problem);
            }
        }
        for (folder, kind) in [
            ("collections", SyncKind::Collection),
            ("environments", SyncKind::Environment),
            ("mocks", SyncKind::Mock),
            ("flows", SyncKind::Flow),
        ] {
            let folder = format!("{base}/{folder}");
            for file in listed(&folder, false, &mut problems) {
                add(kind, format!("{folder}/{file}"), &mut docs, &mut problems);
            }
        }
    }
    for (folder, kind) in [
        ("llm/providers", SyncKind::LlmProvider),
        ("llm/aliases", SyncKind::LlmAlias),
        ("agent/mcp-servers", SyncKind::McpServer),
    ] {
        for file in listed(folder, false, &mut problems) {
            add(kind, format!("{folder}/{file}"), &mut docs, &mut problems);
        }
    }
    if present(&dir.join("agent/skills.json")) {
        add(
            SyncKind::Skills,
            "agent/skills.json".to_owned(),
            &mut docs,
            &mut problems,
        );
    }
    for fixed in ["agent/skills.json", "settings.json"] {
        if !present(&dir.join(fixed))
            && let Some(problem) = placeholder_problem(dir, fixed)
        {
            problems.push(problem);
        }
    }

    match read_json(dir, "settings.json") {
        Ok((Value::Object(mut sections), fingerprint)) => {
            // An older export carries the schema-1 `gateway`, `models` and `agent` sections;
            // translate them. The credentials the translation returns are dropped: a file never
            // writes to the local secret store.
            if !sections.contains_key("ai")
                && ["gateway", "models", "agent"]
                    .iter()
                    .any(|name| sections.contains_key(*name))
            {
                let legacy = ai_from_v1_sections(
                    sections.get("gateway"),
                    sections.get("models"),
                    sections.get("agent"),
                );
                sections.insert("ai".to_owned(), legacy.ai);
            }
            for (name, value) in sections {
                // Unknown sections (a newer app) and `sync` are ignored.
                if let Some(section) = syncable_section(&name) {
                    if value.is_object() {
                        let mut doc = Doc::settings(section, value, None);
                        doc.file = Some("settings.json".to_owned());
                        doc.fingerprint = Some(fingerprint.clone());
                        docs.push(doc);
                    } else {
                        problems.push(format!("settings.json: section {name} must be an object"));
                    }
                }
            }
        }
        Ok(_) => problems.push("settings.json: expected a JSON object".to_owned()),
        Err(message) if present(&dir.join("settings.json")) => problems.push(message),
        Err(_) => {}
    }

    if problems.is_empty() {
        // An entity whose file has no timestamp is as old as the export that wrote it.
        for doc in &mut docs {
            doc.updated_at_ms.get_or_insert(manifest.exported_at_ms);
        }
        docs.sort_by(|a, b| (a.kind, &a.key).cmp(&(b.kind, &b.key)));
        Ok(docs)
    } else {
        Err(AppError::validation(problems.join("; ")))
    }
}
