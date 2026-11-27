//! The single-file operations of the file tools: `read_file`, `write_file`, `edit_file` and
//! `list_dir` (`delete_file` and `move_file` are in `manage`).
//!
//! Single responsibility: the blocking file work behind those four tools, on paths that
//! [`super::Env`] already confined. Everything here is synchronous; the tool wrapper runs it in
//! `spawn_blocking`.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD};

use super::diff::change_counts;
use super::{
    AppliedWrite, BeforeWrite, EditArgs, Env, FileChangeRecord, ListArgs, MAX_IMAGE_BYTES,
    MAX_LIST_ENTRIES, MAX_PDF_BYTES, MAX_READ_BYTES, MAX_READ_LINES, MAX_WRITE_BYTES, ReadArgs,
    WriteArgs,
};
use crate::agent::model::{ToolErrorCode, coded_error_text};
use crate::agent::tools::{FileChangeKind, ToolOutput, fs_safe};
use spanbay_base::cancel::CancelToken;
use spanbay_base::crypto::sha256_hex;
use spanbay_base::error::AppError;
use spanbay_llm::llm::hub::{HubDocument, HubDocumentSource, HubImage, HubPart};

/// Longest line `read_file` shows, in characters.
const MAX_LINE_CHARS: usize = 2000;

/// Most bytes of one line `read_file` reads; the rest of a longer line is skipped unread.
const MAX_LINE_READ_BYTES: u64 = 64 * 1024;

/// How `read_file` hands a file to the model.
enum Media {
    Image(&'static str),
    Pdf,
}

/// The media kind of `path` by extension, if it is not plain text.
fn media_of(path: &Path) -> Option<Media> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some(Media::Image("image/png")),
        "jpg" | "jpeg" => Some(Media::Image("image/jpeg")),
        "gif" => Some(Media::Image("image/gif")),
        "webp" => Some(Media::Image("image/webp")),
        "pdf" => Some(Media::Pdf),
        _ => None,
    }
}

/// An I/O failure on the workspace-relative `rel`.
pub(super) fn io_error(rel: &str, error: &io::Error) -> AppError {
    AppError::io(format!("{rel}: {error}"))
}

/// Opens the resolved `path` for reading and confirms the handle is still the file the path
/// names inside the workspace and outside the protected locations: `resolve` checked a name, and
/// a process sharing the folder can swap a symlink in before the open. Every read goes
/// through the returned handle, never through the name again.
pub(super) fn open_read(env: &Env, path: &Path, rel: &str) -> Result<File, AppError> {
    let file = File::open(path).map_err(|error| io_error(rel, &error))?;
    fs_safe::confirm_open(&file, path, |now| {
        if now.starts_with(&env.workspace) && !env.guard.denies(now) {
            Ok(())
        } else {
            Err(AppError::validation(coded_error_text(
                ToolErrorCode::OutsideFolder,
                &format!(
                    "'{rel}' now points outside the workspace or at a protected location and was not read"
                ),
            )))
        }
    })?;
    Ok(file)
}

/// `read_file`: numbered text lines, or an image / PDF as content parts.
pub(super) fn read_file(
    env: &Env,
    args: &ReadArgs,
    cancel: &CancelToken,
) -> Result<ToolOutput, AppError> {
    let path = env.resolve(&args.path, false)?;
    let rel = env.relative(&path);
    // Only for the friendlier message: `File::open` succeeds on a directory on Unix.
    if path.is_dir() {
        return Err(AppError::validation(format!(
            "'{rel}' is a directory; use list_dir to see its entries"
        )));
    }
    let mut file = open_read(env, &path, &rel)?;
    let len = file
        .metadata()
        .map_err(|error| io_error(&rel, &error))?
        .len();
    if cancel.is_cancelled() {
        return Err(AppError::cancelled());
    }
    let output = match media_of(&path) {
        Some(Media::Image(media_type)) => {
            let data = read_media(&mut file, &rel, len, MAX_IMAGE_BYTES)?;
            ToolOutput {
                parts: vec![HubPart::Image {
                    image: HubImage::Base64 {
                        media_type: media_type.to_owned(),
                        data,
                    },
                }],
                is_error: false,
                meta: None,
                external: false,
                cap: None,
                exit_code: None,
            }
        }
        Some(Media::Pdf) => {
            let data = read_media(&mut file, &rel, len, MAX_PDF_BYTES)?;
            ToolOutput {
                parts: vec![HubPart::Document {
                    document: HubDocument {
                        media_type: "application/pdf".to_owned(),
                        name: path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .map(str::to_owned),
                        source: HubDocumentSource::Base64 { data },
                    },
                }],
                is_error: false,
                meta: None,
                external: false,
                cap: None,
                exit_code: None,
            }
        }
        None => read_text(&mut file, &rel, args.offset, args.limit, cancel)?,
    };
    // What the model just saw is the baseline `write_file` and `edit_file` compare against.
    if let Some(sha256) = hash_open(&mut file) {
        env.hashes.record(&env.session_id, &path, sha256);
    }
    Ok(output)
}

/// The base64 of a media file of at most `limit` bytes.
fn read_media(file: &mut File, rel: &str, len: u64, limit: u64) -> Result<String, AppError> {
    if len > limit {
        return Err(AppError::validation(format!(
            "'{rel}' is {len} bytes; the limit for this file type is {limit} bytes"
        )));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(rel, &error))?;
    Ok(STANDARD.encode(bytes))
}

/// Reads one line of at most [`MAX_LINE_READ_BYTES`] into `buf` (cleared first), skipping the rest
/// of a longer one. `None` at end of file.
fn next_line(reader: &mut impl BufRead, buf: &mut Vec<u8>) -> io::Result<Option<()>> {
    buf.clear();
    let read = reader
        .by_ref()
        .take(MAX_LINE_READ_BYTES)
        .read_until(b'\n', buf)?;
    if read == 0 {
        return Ok(None);
    }
    if !buf.ends_with(b"\n") && read as u64 == MAX_LINE_READ_BYTES {
        reader.skip_until(b'\n')?;
    }
    Ok(Some(()))
}

/// The text window of a file as `{line number}\t{text}` rows.
fn read_text(
    file: &mut File,
    rel: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    cancel: &CancelToken,
) -> Result<ToolOutput, AppError> {
    let first = offset.unwrap_or(1).max(1);
    let limit = limit.unwrap_or(MAX_READ_LINES).clamp(1, MAX_READ_LINES);
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    let mut out = String::new();
    let mut line_no = 0usize;
    let mut shown = 0usize;
    let mut more = false;
    loop {
        if line_no.is_multiple_of(1024) && cancel.is_cancelled() {
            return Err(AppError::cancelled());
        }
        if shown == limit || out.len() >= MAX_READ_BYTES {
            more = !reader
                .fill_buf()
                .map_err(|error| io_error(rel, &error))?
                .is_empty();
            break;
        }
        if next_line(&mut reader, &mut buf)
            .map_err(|error| io_error(rel, &error))?
            .is_none()
        {
            break;
        }
        line_no += 1;
        if memchr::memchr(0, &buf).is_some() {
            return Err(AppError::validation(format!(
                "'{rel}' is a binary file (it contains NUL bytes) and cannot be shown as text"
            )));
        }
        if line_no < first {
            continue;
        }
        let text = String::from_utf8_lossy(&buf);
        let text = text.trim_end_matches(['\n', '\r']);
        let mut chars = text.chars();
        let shown_text: String = chars.by_ref().take(MAX_LINE_CHARS).collect();
        out.push_str(&format!("{line_no:>6}\t{shown_text}"));
        if chars.next().is_some() {
            out.push_str(" … [line truncated]");
        }
        out.push('\n');
        shown += 1;
    }
    if shown == 0 {
        return Ok(ToolOutput::text(if line_no == 0 {
            format!("'{rel}' is empty")
        } else {
            format!("'{rel}' has {line_no} lines; offset {first} is past the end")
        }));
    }
    if more {
        let last = first + shown - 1;
        out.push_str(&format!(
            "\n[Showing lines {first}-{last}. Call read_file again with offset={} to continue.]",
            last + 1
        ));
    }
    Ok(ToolOutput::text(out))
}

/// Why `old_string` cannot be applied, or the edited text and the number of replacements.
///
/// `old_string` must match exactly; as a fallback for a CRLF file the model's `\n` line breaks are
/// retried as `\r\n`, so the edit keeps the file's line endings.
pub(super) fn apply_edit(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<(String, usize), String> {
    if old.is_empty() {
        return Err(
            "old_string must not be empty; use write_file to create or replace a whole file"
                .to_owned(),
        );
    }
    if old == new {
        return Err("old_string and new_string are identical; nothing to change".to_owned());
    }
    let mut count = content.matches(old).count();
    let (mut old, mut new) = (old.to_owned(), new.to_owned());
    if count == 0 && content.contains("\r\n") && !old.contains('\r') {
        let crlf = |text: &str| text.replace("\r\n", "\n").replace('\n', "\r\n");
        let (old_crlf, new_crlf) = (crlf(&old), crlf(&new));
        let crlf_count = content.matches(old_crlf.as_str()).count();
        if crlf_count > 0 {
            (old, new, count) = (old_crlf, new_crlf, crlf_count);
        }
    }
    if count == 0 {
        return Err("old_string was not found; it must match the file exactly, including whitespace and indentation. Read the file again and copy the text from it".to_owned());
    }
    if count > 1 && !replace_all {
        return Err(format!(
            "old_string occurs {count} times; include more surrounding lines to make it unique, or set replace_all to true to replace every occurrence"
        ));
    }
    let edited = if replace_all {
        content.replace(&old, &new)
    } else {
        content.replacen(&old, &new, 1)
    };
    Ok((edited, if replace_all { count } else { 1 }))
}

/// The SHA-256 of the open file's content from the start, or `None` when it is unreadable or
/// larger than [`MAX_WRITE_BYTES`] (such a file can be neither overwritten nor edited).
fn hash_open(file: &mut File) -> Option<String> {
    use std::io::{Seek, SeekFrom};
    if file.metadata().ok()?.len() > MAX_WRITE_BYTES as u64 {
        return None;
    }
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(sha256_hex(&bytes))
}

/// The bytes of the existing regular file `path`, at most [`MAX_WRITE_BYTES`] long.
pub(super) fn read_existing_bytes(path: &Path, rel: &str) -> Result<Vec<u8>, AppError> {
    let meta = fs::metadata(path).map_err(|error| io_error(rel, &error))?;
    if !meta.is_file() {
        return Err(AppError::validation(format!("'{rel}' is not a file")));
    }
    if meta.len() > MAX_WRITE_BYTES as u64 {
        return Err(AppError::validation(format!(
            "'{rel}' is larger than {MAX_WRITE_BYTES} bytes"
        )));
    }
    fs::read(path).map_err(|error| io_error(rel, &error))
}

/// The current text of the existing file `path` for an edit or a diff preview.
pub(super) fn read_existing_text(path: &Path, rel: &str) -> Result<String, AppError> {
    utf8(read_existing_bytes(path, rel)?, rel)
}

/// `bytes` as text, or a validation error naming `rel`.
fn utf8(bytes: Vec<u8>, rel: &str) -> Result<String, AppError> {
    String::from_utf8(bytes)
        .map_err(|_| AppError::validation(format!("'{rel}' is not valid UTF-8 text")))
}

/// Fails unless this session last saw exactly the content whose hash is `current_sha256`.
fn require_fresh_read(
    env: &Env,
    path: &Path,
    rel: &str,
    current_sha256: &str,
) -> Result<(), AppError> {
    match env.hashes.get(&env.session_id, path) {
        Some(seen) if seen == current_sha256 => Ok(()),
        Some(_) => Err(AppError::conflict(format!(
            "{rel} changed since you last read it. Read it again with read_file, then retry"
        ))),
        None => Err(AppError::conflict(format!(
            "You have not read {rel} in this session. Read it with read_file before changing it"
        ))),
    }
}

/// The message of a write that found the folder changed under it.
pub(super) const FOLDER_CHANGED: &str = "The folder changed while writing; nothing was written.";

/// Replaces `path` with `content`. The journal record is built first and handed to `before_write`
/// (which stores the previous bytes and the `applying` row; an error aborts with nothing written),
/// then the path is resolved once more (same location inside the folder) and written through
/// directory handles that also confirm it is the file seen at the path check and that no parent
/// became a symlink.
fn commit(
    env: &Env,
    path: &Path,
    rel: &str,
    before: Option<Vec<u8>>,
    seen: Option<fs_safe::FileIdentity>,
    content: &str,
    before_write: BeforeWrite<'_>,
) -> Result<FileChangeRecord, AppError> {
    let after_sha256 = sha256_hex(content.as_bytes());
    let before_text = before.as_deref().map(String::from_utf8_lossy);
    let (added, removed) = change_counts(before_text.as_deref(), content);
    let record = FileChangeRecord {
        path: rel.to_owned(),
        kind: if before.is_some() {
            FileChangeKind::Edit
        } else {
            FileChangeKind::Create
        },
        before_sha256: before.as_deref().map(sha256_hex),
        before_bytes: before,
        after_sha256: Some(after_sha256.clone()),
        added,
        removed,
    };
    before_write(&record)?;
    let inside = match env.resolve(rel, true) {
        Ok(now) if now == path => path.strip_prefix(&env.workspace).ok(),
        _ => None,
    }
    .ok_or_else(|| AppError::conflict(FOLDER_CHANGED))?;
    fs_safe::write_in(&env.workspace, inside, seen, content.as_bytes()).map_err(
        |error| match error {
            AppError::Conflict { .. } => AppError::conflict(FOLDER_CHANGED),
            other => other,
        },
    )?;
    env.hashes.record(&env.session_id, path, after_sha256);
    Ok(record)
}

/// `write_file`: creates or overwrites a file, creating missing parent directories. Overwriting
/// needs a read of the current content in this session ([`require_fresh_read`]).
pub(super) fn write_file(
    env: &Env,
    args: &WriteArgs,
    cancel: &CancelToken,
    before_write: BeforeWrite<'_>,
) -> Result<AppliedWrite, AppError> {
    if args.content.len() > MAX_WRITE_BYTES {
        return Err(AppError::validation(format!(
            "content is {} bytes; write_file accepts at most {MAX_WRITE_BYTES}",
            args.content.len()
        )));
    }
    let path = env.resolve(&args.path, true)?;
    let rel = env.relative(&path);
    if path.is_dir() {
        return Err(AppError::validation(format!("'{rel}' is a directory")));
    }
    let seen = fs_safe::identity(&path)?;
    let before = if path.exists() {
        let bytes = read_existing_bytes(&path, &rel)?;
        require_fresh_read(env, &path, &rel, &sha256_hex(&bytes))?;
        Some(bytes)
    } else {
        None
    };
    if cancel.is_cancelled() {
        return Err(AppError::cancelled());
    }
    let verb = if before.is_some() {
        "Overwrote"
    } else {
        "Created"
    };
    let change = commit(env, &path, &rel, before, seen, &args.content, before_write)?;
    Ok(AppliedWrite {
        output: ToolOutput::text(format!("{verb} {rel} ({} bytes)", args.content.len())),
        changes: vec![change],
    })
}

/// Applies every edit of `args` to `content` in order, each to the result of the one before:
/// the edited text and the replacements made. All or nothing: the first edit that cannot be
/// applied fails the whole call, naming its position.
pub(super) fn apply_edits(content: &str, args: &EditArgs) -> Result<(String, usize), String> {
    let edits = args.edits()?;
    let single = edits.len() == 1;
    let mut text = content.to_owned();
    let mut count = 0;
    for (index, edit) in edits.iter().enumerate() {
        let (next, replaced) =
            apply_edit(&text, &edit.old_string, &edit.new_string, edit.replace_all).map_err(
                |message| {
                    if single {
                        message
                    } else {
                        format!("edit {} of {}: {message}", index + 1, edits.len())
                    }
                },
            )?;
        text = next;
        count += replaced;
    }
    Ok((text, count))
}

/// `edit_file`: replaces `old_string` with `new_string` (or each of `edits`, in order) in an
/// existing text file. Needs a read of the current content in this session
/// ([`require_fresh_read`]).
pub(super) fn edit_file(
    env: &Env,
    args: &EditArgs,
    cancel: &CancelToken,
    before_write: BeforeWrite<'_>,
) -> Result<AppliedWrite, AppError> {
    let path = env.resolve(&args.path, false)?;
    let rel = env.relative(&path);
    let seen = fs_safe::identity(&path)?;
    let bytes = read_existing_bytes(&path, &rel)?;
    require_fresh_read(env, &path, &rel, &sha256_hex(&bytes))?;
    let content = utf8(bytes.clone(), &rel)?;
    let (edited, count) = apply_edits(&content, args)
        .map_err(|message| AppError::validation(format!("{rel}: {message}")))?;
    if edited.len() > MAX_WRITE_BYTES {
        return Err(AppError::validation(format!(
            "the edited file would be larger than {MAX_WRITE_BYTES} bytes"
        )));
    }
    if cancel.is_cancelled() {
        return Err(AppError::cancelled());
    }
    let change = commit(env, &path, &rel, Some(bytes), seen, &edited, before_write)?;
    Ok(AppliedWrite {
        output: ToolOutput::text(format!(
            "Edited {rel}: replaced {count} occurrence{}",
            if count == 1 { "" } else { "s" }
        )),
        changes: vec![change],
    })
}

/// A byte count as `1.5 KiB`.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `list_dir`: the entries of one directory, directories first.
pub(super) fn list_dir(
    env: &Env,
    args: &ListArgs,
    cancel: &CancelToken,
) -> Result<ToolOutput, AppError> {
    let path = env.resolve(args.path.as_deref().unwrap_or("."), false)?;
    let rel = env.relative(&path);
    if !path.is_dir() {
        return Err(AppError::validation(format!(
            "'{rel}' is not a directory; use read_file for files"
        )));
    }
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in fs::read_dir(&path).map_err(|error| io_error(&rel, &error))? {
        if cancel.is_cancelled() {
            return Err(AppError::cancelled());
        }
        let entry = entry.map_err(|error| io_error(&rel, &error))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let entry_path = path.join(entry.file_name());
        if env.guard.denies(&entry_path) {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            let target = fs::read_link(&entry_path)
                .map(|target| target.display().to_string())
                .unwrap_or_default();
            files.push((name.clone(), format!("{name} -> {target}")));
        } else if kind.is_dir() {
            dirs.push((name.clone(), format!("{name}/")));
        } else {
            let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            files.push((name.clone(), format!("{name}  ({})", human_size(size))));
        }
    }
    dirs.sort();
    files.sort();
    let total = dirs.len() + files.len();
    if total == 0 {
        return Ok(ToolOutput::text(format!("{rel}/ is empty")));
    }
    let mut out = format!("{rel}/ ({total} entries)\n");
    for (_, line) in dirs.iter().chain(&files).take(MAX_LIST_ENTRIES) {
        out.push_str(line);
        out.push('\n');
    }
    if total > MAX_LIST_ENTRIES {
        out.push_str(&format!(
            "[Showing the first {MAX_LIST_ENTRIES} of {total} entries; use glob or grep to narrow down]\n"
        ));
    }
    Ok(ToolOutput::text(out))
}
