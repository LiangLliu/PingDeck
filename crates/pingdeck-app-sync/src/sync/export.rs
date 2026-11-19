//! Writing documents to a sync directory: deterministic files, atomic writes, stale-file cleanup.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::AppError;
use crate::platform::write_atomic;

use super::docs::{Doc, settings_map};
use super::secrets;
use super::{FORMAT_NAME, FORMAT_VERSION, MAX_FILE_BYTES, SyncExportResult, SyncKind};

/// Name of the manifest file.
pub(super) const MANIFEST: &str = "manifest.json";

/// The manifest: format identity plus the list of files an export wrote.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Manifest {
    pub(super) format: String,
    pub(super) version: u32,
    pub(super) app_version: String,
    pub(super) exported_at_ms: i64,
    pub(super) secrets_included: bool,
    /// Relative paths (with `/`) of every file besides the manifest, sorted.
    pub(super) files: Vec<String>,
}

/// Lowercase ASCII slug of `text` (at most 40 characters), or `fallback`.
fn slug(text: &str, fallback: &str) -> String {
    let mut out = String::new();
    for ch in text.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(40);
    let out = out.trim_matches('-');
    if out.is_empty() {
        fallback.to_owned()
    } else {
        out.to_owned()
    }
}

/// The last eight alphanumeric characters of an id.
fn short_id(id: &str) -> String {
    let alnum: Vec<char> = id.chars().filter(char::is_ascii_alphanumeric).collect();
    let start = alnum.len().saturating_sub(8);
    let short: String = alnum[start..].iter().collect();
    if short.is_empty() {
        "x".to_owned()
    } else {
        short
    }
}

/// `name-id` file stem of an entity.
fn stem(doc: &Doc) -> String {
    if doc.kind == SyncKind::McpServer {
        slug(&doc.name, "server")
    } else {
        format!("{}-{}", slug(&doc.name, "item"), short_id(&doc.key))
    }
}

/// Pretty JSON, two-space indent, trailing newline.
pub(super) fn pretty(value: &Value) -> Result<Vec<u8>, AppError> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|error| AppError::internal(error.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// One file of an export: its bytes and the entity's last-change time (when its file records one).
struct Rendered {
    bytes: Vec<u8>,
    updated_at_ms: Option<i64>,
}

/// The files of an export by relative path, secrets masked unless `include_secrets`.
fn render(docs: &[Doc], include_secrets: bool) -> Result<BTreeMap<String, Rendered>, AppError> {
    let workspace_dirs: BTreeMap<&str, String> = docs
        .iter()
        .filter(|doc| doc.kind == SyncKind::Workspace)
        .map(|doc| (doc.key.as_str(), format!("workspaces/{}", stem(doc))))
        .collect();
    let mut files = BTreeMap::new();
    let mut settings = settings_map(docs);
    if !include_secrets {
        for value in settings.values_mut() {
            secrets::mask(SyncKind::Settings, value);
            secrets::check_slots(SyncKind::Settings, value).map_err(AppError::internal)?;
        }
    }

    let mut taken = HashSet::new();
    for doc in docs.iter().filter(|doc| doc.kind != SyncKind::Settings) {
        let wd = match (doc.kind, doc.workspace.as_deref()) {
            (SyncKind::Workspace, _) => workspace_dirs.get(doc.key.as_str()).cloned(),
            (_, Some(id)) => workspace_dirs
                .get(id)
                .cloned()
                .or_else(|| Some(format!("workspaces/{}", short_id(id)))),
            _ => None,
        }
        .unwrap_or_default();
        let path_of = |stem: &str| match doc.kind {
            SyncKind::Workspace => format!("{wd}/workspace.json"),
            SyncKind::LooseRequests => format!("{wd}/requests.json"),
            SyncKind::Globals => format!("{wd}/globals.json"),
            SyncKind::Collection => format!("{wd}/collections/{stem}.json"),
            SyncKind::Environment => format!("{wd}/environments/{stem}.json"),
            SyncKind::Mock => format!("{wd}/mocks/{stem}.json"),
            SyncKind::Flow => format!("{wd}/flows/{stem}.json"),
            SyncKind::LlmProvider => format!("llm/providers/{stem}.json"),
            SyncKind::LlmAlias => format!("llm/aliases/{stem}.json"),
            SyncKind::McpServer => format!("agent/mcp-servers/{stem}.json"),
            SyncKind::Skills | SyncKind::Settings => "agent/skills.json".to_owned(),
        };
        // Docs are sorted, so a name clash always resolves the same way.
        let base = stem(doc);
        let mut path = path_of(&base);
        let mut counter = 1;
        while !taken.insert(path.clone()) {
            counter += 1;
            path = path_of(&format!("{base}-{counter}"));
        }
        let mut body = doc.body.clone();
        if !include_secrets {
            secrets::mask(doc.kind, &mut body);
            secrets::check_slots(doc.kind, &body).map_err(AppError::internal)?;
        }
        if let (Some(updated), Value::Object(fields)) = (doc.updated_at_ms, &mut body) {
            fields.insert("updated_at_ms".to_owned(), json!(updated));
        }
        files.insert(
            path,
            Rendered {
                bytes: pretty(&body)?,
                updated_at_ms: doc.updated_at_ms,
            },
        );
    }
    files.insert(
        "settings.json".to_owned(),
        Rendered {
            bytes: pretty(
                &serde_json::to_value(&settings).map_err(|e| AppError::internal(e.to_string()))?,
            )?,
            updated_at_ms: None,
        },
    );
    Ok(files)
}

/// Whether a relative path from a manifest is one an export could have written.
pub(super) fn is_known_path(path: &str) -> bool {
    let components: Vec<&str> = path.split('/').collect();
    let plain = Path::new(path)
        .components()
        .all(|part| matches!(part, Component::Normal(_)));
    plain
        && path.ends_with(".json")
        && matches!(
            components.as_slice(),
            ["settings.json"]
                | ["agent", "skills.json"]
                | ["llm", "providers" | "aliases", _]
                | ["agent", "mcp-servers", _]
                | [
                    "workspaces",
                    _,
                    "workspace.json" | "globals.json" | "requests.json"
                ]
                | [
                    "workspaces",
                    _,
                    "collections" | "environments" | "mocks" | "flows",
                    _
                ]
        )
}

/// A temp file older than this is a leftover of an export that died mid-write.
const TEMP_MAX_AGE: Duration = Duration::from_secs(60);

/// Writes `bytes` to `path` atomically (see [`write_atomic`]), creating its folder first.
/// `private` creates the file 0600.
fn write_file(path: &Path, bytes: &[u8], private: bool) -> Result<(), AppError> {
    let io = |error: std::io::Error| AppError::io(format!("{}: {error}", path.display()));
    fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new("."))).map_err(io)?;
    write_atomic(path, bytes, private).map_err(io)
}

/// Narrows an existing file to 0600 (an unchanged file written before `include_secrets` was on).
#[cfg(unix)]
fn make_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::metadata(path)
        && meta.permissions().mode() & 0o077 != 0
    {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
}

#[cfg(not(unix))]
fn make_private(_path: &Path) {}

/// Removes temp files of dead exports from `dir` (`.<name>.<pid>.<n>.tmp`, or the older
/// `<name>.json.tmp`); young ones belong to a concurrent export and stay.
fn remove_stale_temps(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let ours = name.ends_with(".tmp") && (name.starts_with('.') || name.ends_with(".json.tmp"));
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > TEMP_MAX_AGE);
        if ours && old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// The regular file at `path`, read through one handle: a symlink, FIFO, device or directory is
/// refused (a hostile folder could point at `/dev/zero` or a pipe) before it is opened, and at
/// most [`MAX_FILE_BYTES`] are read however the size changes meanwhile.
pub(super) fn read_regular(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::{Error, ErrorKind, Read};
    let refuse = |what: &str| Error::new(ErrorKind::InvalidData, what.to_owned());
    let kind = fs::symlink_metadata(path)?.file_type();
    if kind.is_symlink() {
        return Err(refuse("is a symbolic link, not a regular file"));
    }
    // Opening a FIFO without a writer blocks, so the type is checked before the open.
    if !kind.is_file() {
        return Err(refuse("is not a regular file"));
    }
    let file = fs::File::open(path)?;
    // The path may have been swapped between the check and the open.
    if !file.metadata()?.is_file() {
        return Err(refuse("is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(refuse(&format!("exceeds the {MAX_FILE_BYTES}-byte limit")));
    }
    Ok(bytes)
}

/// The previous manifest of `dir`, if there is a readable one of this format.
fn previous_manifest(dir: &Path) -> Option<Manifest> {
    let manifest: Manifest =
        serde_json::from_slice(&read_regular(&dir.join(MANIFEST)).ok()?).ok()?;
    (manifest.format == FORMAT_NAME && manifest.version == FORMAT_VERSION).then_some(manifest)
}

/// The file at `path` if it is a regular file small enough to be one of ours.
fn read_existing(path: &Path) -> Option<Vec<u8>> {
    read_regular(path).ok()
}

/// The `updated_at_ms` recorded in an exported file.
fn recorded_time(bytes: &[u8]) -> Option<i64> {
    serde_json::from_slice::<Value>(bytes)
        .ok()?
        .get("updated_at_ms")?
        .as_i64()
}

/// Content fingerprint of `bytes` (SHA-256, hex) so the local ledger can tell whether a file
/// changed on disk since this machine last wrote it.
pub(crate) fn fingerprint(bytes: &[u8]) -> String {
    crate::crypto::sha256_hex(bytes)
}

/// Length of the fingerprints older versions recorded (a 64-bit `DefaultHasher` value in hex).
const LEGACY_FINGERPRINT_LEN: usize = 16;

/// Whether the ledger's `recorded` fingerprint says the file with fingerprint `current` is
/// still the one this machine wrote. A legacy fingerprint cannot be recomputed (its hasher is not
/// stable across Rust releases) and counts as unchanged, so an update does not make every file
/// look edited by another machine; the next export records the SHA-256 value.
pub(crate) fn fingerprint_unchanged(recorded: Option<&String>, current: &str) -> bool {
    recorded.is_some_and(|recorded| {
        recorded == current
            || (recorded.len() == LEGACY_FINGERPRINT_LEN
                && recorded.bytes().all(|byte| byte.is_ascii_hexdigit()))
    })
}

/// What an export did, and what the local ledger of written files must learn from it.
pub(crate) struct ExportOutcome {
    /// The result shown to the user.
    pub(crate) result: SyncExportResult,
    /// Paths this machine wrote or found identical: it owns them and may delete them later.
    pub(crate) owned: Vec<(String, String)>,
    /// Paths no longer on disk: they leave the ledger.
    pub(crate) forgotten: Vec<String>,
}

/// Exports `docs` into `dir`. Changed files are rewritten and unchanged ones left alone, except
/// that a file whose recorded `updated_at_ms` is newer than the local entity's is skipped (another
/// machine's newer edit) and reported in `skipped_newer`. Of the files no longer produced, only
/// those in `owned` (written by this machine earlier) are removed, and only inside the known
/// layout. The manifest is rewritten only when something else changed, so re-exporting unchanged
/// data leaves the directory byte-identical.
pub(crate) fn write_dir(
    dir: &Path,
    docs: &[Doc],
    include_secrets: bool,
    now_ms: i64,
    owned: &HashMap<String, String>,
) -> Result<ExportOutcome, AppError> {
    if !dir.is_absolute() {
        return Err(AppError::validation(
            "the sync directory must be an absolute path",
        ));
    }
    let files = render(docs, include_secrets)?;
    fs::create_dir_all(dir).map_err(|error| AppError::io(format!("{}: {error}", dir.display())))?;
    let previous = previous_manifest(dir);
    let parents: BTreeSet<PathBuf> = files
        .keys()
        .filter_map(|path| dir.join(path).parent().map(Path::to_path_buf))
        .chain([dir.to_path_buf()])
        .collect();
    parents.iter().for_each(|parent| remove_stale_temps(parent));

    let mut written = 0_u32;
    let mut skipped_newer = Vec::new();
    for (path, file) in &files {
        let target = dir.join(path);
        let existing = read_existing(&target);
        if existing.as_deref() == Some(file.bytes.as_slice()) {
            if include_secrets {
                make_private(&target);
            }
            continue;
        }
        if let (Some(local), Some(on_disk)) = (
            file.updated_at_ms,
            existing.as_deref().and_then(recorded_time),
        ) && on_disk > local
        {
            skipped_newer.push(path.clone());
            continue;
        }
        write_file(&target, &file.bytes, include_secrets)?;
        written += 1;
    }

    let mut removed = 0_u32;
    let mut forgotten = Vec::new();
    for stale in owned
        .keys()
        .filter(|path| !files.contains_key(*path) && is_known_path(path))
    {
        let target = dir.join(stale);
        match fs::remove_file(&target) {
            Ok(()) => {
                removed += 1;
                forgotten.push(stale.clone());
                // Tidy folders the removal emptied; `remove_dir` refuses non-empty ones.
                for parent in target.ancestors().skip(1).take(2) {
                    if parent == dir || fs::remove_dir(parent).is_err() {
                        break;
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                forgotten.push(stale.clone());
            }
            Err(_) => {}
        }
    }

    let owned_now = files
        .iter()
        .filter(|(path, _)| !skipped_newer.contains(path))
        .map(|(path, file)| (path.clone(), fingerprint(&file.bytes)))
        .collect();
    let listed: Vec<String> = files.into_keys().collect();
    let unchanged = previous
        .as_ref()
        .is_some_and(|old| old.files == listed && old.secrets_included == include_secrets);
    if written > 0 || removed > 0 || !unchanged {
        let manifest = Manifest {
            format: FORMAT_NAME.to_owned(),
            version: FORMAT_VERSION,
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            exported_at_ms: now_ms,
            secrets_included: include_secrets,
            files: listed,
        };
        let value = serde_json::to_value(&manifest)
            .map_err(|error| AppError::internal(error.to_string()))?;
        write_file(&dir.join(MANIFEST), &pretty(&value)?, include_secrets)?;
        written += 1;
    }
    Ok(ExportOutcome {
        result: SyncExportResult {
            directory: dir.display().to_string(),
            files_written: written,
            files_removed: removed,
            skipped_newer,
            secrets_included: include_secrets,
        },
        owned: owned_now,
        forgotten,
    })
}
