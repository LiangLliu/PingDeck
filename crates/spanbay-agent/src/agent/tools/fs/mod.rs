//! The file tools of the agent: `read_file`, `write_file`, `edit_file`, `delete_file`,
//! `move_file`, `list_dir`, `glob` and `grep`.
//!
//! Single responsibility: expose the eight workspace file operations as [`AgentTool`]s. Every path
//! goes through [`paths::PathGuard`] (confinement to the session workspace, protected locations,
//! approval classification); `ops`, `manage` and `search` do the blocking file work, `diff`
//! renders the approval preview and counts changed lines, `hashes` implements the read-before-edit
//! guard and `change` carries what a change did and undoes it. Every change, deletes and moves
//! included, goes through the run's journal, so the Changes tab can take it back. Calls run on
//! `spawn_blocking`, never on the async runtime.

mod change;
mod diff;
mod hashes;
mod manage;
mod ops;
mod paths;
mod search;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{
    AgentTool, CallInspection, ToolApprovalPreview, ToolClass, ToolContext, ToolEvent, ToolOutput,
    ToolRegistry, ToolSource, ToolSpec,
};
pub use change::{
    AppliedWrite, BeforeWrite, FileChangeRecord, RevertHunkOutcome, UndoOutcome, apply_write,
    revert_file_hunk, undo_file_change,
};
pub use diff::unified_diff;
pub use hashes::ReadHashes;
pub use paths::{PathGuard, check_workspace, resolve_workspace_path};
use spanbay_base::cancel::CancelToken;
use spanbay_base::error::AppError;

/// Most lines one `read_file` call returns.
const MAX_READ_LINES: usize = 2000;
/// Most bytes of text one `read_file` call returns.
const MAX_READ_BYTES: usize = 256 * 1024;
/// Largest image `read_file` returns.
const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
/// Largest PDF `read_file` returns.
const MAX_PDF_BYTES: u64 = 20 * 1024 * 1024;
/// Largest file `write_file` writes and `edit_file` edits.
const MAX_WRITE_BYTES: usize = 5 * 1024 * 1024;
/// Most entries `list_dir` shows.
const MAX_LIST_ENTRIES: usize = 500;
/// Most paths `glob_files` returns.
const MAX_GLOB_RESULTS: usize = 1000;
/// Hits `grep_files` returns when `max_results` is absent.
const DEFAULT_GREP_RESULTS: usize = 100;
/// Most hits `grep_files` returns whatever `max_results` says.
const MAX_GREP_RESULTS: usize = 1000;
/// Longest hit line `grep_files` shows, in characters.
const MAX_GREP_LINE_CHARS: usize = 500;
/// Largest file `grep_files` searches.
const MAX_GREP_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Largest diff preview in an approval card.
const MAX_PREVIEW_BYTES: usize = 64 * 1024;
/// Most edits one `edit_file` call applies.
const MAX_EDITS: usize = 50;

/// The arguments of `read_file`.
#[derive(Debug, Deserialize)]
struct ReadArgs {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

/// The arguments of `write_file`.
#[derive(Debug, Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

/// One replacement of `edit_file`.
#[derive(Debug, Clone, Deserialize)]
struct EditOp {
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

/// The arguments of `edit_file`: one replacement, or several applied in order.
#[derive(Debug, Deserialize)]
struct EditArgs {
    path: String,
    old_string: Option<String>,
    new_string: Option<String>,
    #[serde(default)]
    replace_all: bool,
    edits: Option<Vec<EditOp>>,
}

impl EditArgs {
    /// The replacements of the call: `edits`, or the single `old_string` / `new_string` pair.
    fn edits(&self) -> Result<Vec<EditOp>, String> {
        match (&self.edits, &self.old_string, &self.new_string) {
            (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
                Err("pass either old_string and new_string, or edits, not both".to_owned())
            }
            (Some(edits), None, None) if edits.is_empty() => {
                Err("edits must hold at least one edit".to_owned())
            }
            (Some(edits), None, None) if edits.len() > MAX_EDITS => {
                Err(format!("edits holds at most {MAX_EDITS} edits"))
            }
            (Some(edits), None, None) => Ok(edits.clone()),
            (None, Some(old), Some(new)) => Ok(vec![EditOp {
                old_string: old.clone(),
                new_string: new.clone(),
                replace_all: self.replace_all,
            }]),
            (None, _, _) => Err("old_string and new_string are required".to_owned()),
        }
    }
}

/// The arguments of `delete_file`.
#[derive(Debug, Deserialize)]
struct DeleteArgs {
    path: String,
}

/// The arguments of `move_file`.
#[derive(Debug, Deserialize)]
struct MoveArgs {
    from: String,
    to: String,
}

/// The arguments of `list_dir`.
#[derive(Debug, Deserialize)]
struct ListArgs {
    path: Option<String>,
}

/// The arguments of `glob_files`.
#[derive(Debug, Deserialize)]
struct GlobArgs {
    pattern: String,
    path: Option<String>,
}

/// The arguments of `grep_files`.
#[derive(Debug, Deserialize)]
struct GrepArgs {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    #[serde(default)]
    case_insensitive: bool,
    max_results: Option<usize>,
}

/// Parses a tool's arguments object.
fn parse<T: DeserializeOwned>(arguments: &Value) -> Result<T, AppError> {
    serde_json::from_value(arguments.clone())
        .map_err(|error| AppError::validation(format!("Invalid arguments: {error}")))
}

/// The confined view of the workspace one call works in.
struct Env {
    /// The canonical workspace directory.
    workspace: PathBuf,
    guard: PathGuard,
    /// The session the call belongs to (read-before-edit guard key).
    session_id: String,
    /// What sessions last read.
    hashes: ReadHashes,
}

impl Env {
    /// Opens the workspace of a call; `data_root` is SpanBay's data directory, if it has one.
    fn open(
        workspace_dir: Option<&Path>,
        data_root: Option<&Path>,
        session_id: &str,
        hashes: ReadHashes,
    ) -> Result<Self, AppError> {
        let dir = workspace_dir.ok_or_else(|| {
            AppError::validation(
                "This session has no workspace folder, so file tools are unavailable. Ask the user to choose a folder for the session",
            )
        })?;
        let workspace = paths::canonical_workspace(dir)?;
        let home = std::env::var_os("HOME").map(PathBuf::from);
        // The data root is denied per path by the guard; only `shell`, which may write the whole
        // folder, refuses a folder around it.
        paths::check_workspace(&workspace, home.as_deref(), None)?;
        Ok(Self {
            workspace,
            guard: PathGuard::for_process(data_root),
            session_id: session_id.to_owned(),
            hashes,
        })
    }

    fn from_context(ctx: &ToolContext<'_>) -> Result<Self, AppError> {
        Self::open(
            ctx.workspace_dir,
            ctx.data_root,
            ctx.session_id,
            ctx.read_hashes.clone(),
        )
    }

    fn resolve(&self, candidate: &str, for_write: bool) -> Result<PathBuf, AppError> {
        self.guard.resolve(&self.workspace, candidate, for_write)
    }

    /// The workspace-relative spelling of a resolved path.
    fn relative(&self, path: &Path) -> String {
        paths::relative_display(&self.workspace, path)
    }

    /// The classification every call on `path` shares: its subject and approval reason.
    fn inspection(&self, path: &Path, for_write: bool) -> CallInspection {
        let relative = self.relative(path);
        // A write under `.git/` is refused outright, in every mode and without a card.
        let hard_block = (for_write && relative.split(['/', '\\']).any(|part| part == ".git"))
            .then(|| "Blocked: the file tools never write under .git/.".to_owned());
        let always_ask = paths::approval_reason(&self.workspace, path, for_write);
        CallInspection {
            read_only: !for_write,
            hard_block,
            ask_code: always_ask.is_some().then_some("protected_path"),
            always_ask,
            arg_subject: Some(relative),
            ..CallInspection::default()
        }
    }
}

/// Which of the eight tools a [`FsTool`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Read,
    Write,
    Edit,
    Delete,
    Move,
    List,
    Glob,
    Grep,
}

/// One file tool.
struct FsTool {
    kind: Kind,
    spec: ToolSpec,
}

impl FsTool {
    fn new(kind: Kind, name: &str, description: &str, input_schema: Value) -> Arc<Self> {
        let edits = matches!(kind, Kind::Write | Kind::Edit | Kind::Delete | Kind::Move);
        Arc::new(Self {
            kind,
            spec: ToolSpec {
                name: name.to_owned(),
                description: description.to_owned(),
                input_schema,
                read_only: !edits,
                destructive: edits,
                source: ToolSource::Builtin,
                class: if edits {
                    ToolClass::Edit
                } else {
                    ToolClass::Read
                },
                parallel_safe: !edits,
            },
        })
    }

    /// The write/edit approval classification with `diff` as the preview.
    fn inspect_change(env: &Env, path: &Path, diff: String, destructive: bool) -> CallInspection {
        CallInspection {
            destructive,
            preview: Some(ToolApprovalPreview::Diff {
                path: env.relative(path),
                unified_diff: if diff.is_empty() {
                    "(no changes)".to_owned()
                } else {
                    diff
                },
            }),
            ..env.inspection(path, true)
        }
    }
}

/// Registers the eight file tools.
pub fn register(registry: &mut ToolRegistry) {
    let path = json!({ "type": "string", "description": "Path relative to the workspace folder (absolute paths inside it also work)." });
    let tools = [
        FsTool::new(
            Kind::Read,
            "read_file",
            "Reads a file of the workspace. Text comes back with line numbers (at most 2000 lines / 256 KiB per call; use offset and limit to page through bigger files). Images (png, jpeg, gif, webp) and PDFs are returned as attachments you can look at. Binary files are refused.",
            json!({
                "type": "object",
                "properties": {
                    "path": path,
                    "offset": { "type": "integer", "minimum": 1, "description": "First line to return (1-based). Default 1." },
                    "limit": { "type": "integer", "minimum": 1, "description": "Number of lines to return. Default and maximum 2000." }
                },
                "required": ["path"]
            }),
        ),
        FsTool::new(
            Kind::Write,
            "write_file",
            "Creates a file or overwrites it completely with `content` (missing folders are created; at most 5 MiB). Prefer edit_file for changing part of an existing file.",
            json!({
                "type": "object",
                "properties": {
                    "path": path,
                    "content": { "type": "string", "description": "The complete new content of the file." }
                },
                "required": ["path", "content"]
            }),
        ),
        FsTool::new(
            Kind::Edit,
            "edit_file",
            "Replaces an exact piece of text in an existing file. `old_string` must match the file exactly (whitespace included) and be unique unless replace_all is true; read the file first. To make several changes to one file in one call, pass `edits` instead: they apply in order, each to the result of the one before, and the call changes nothing unless every edit applies. Line endings of the file are preserved.",
            json!({
                "type": "object",
                "properties": {
                    "path": path,
                    "old_string": { "type": "string", "description": "The exact text to replace." },
                    "new_string": { "type": "string", "description": "The replacement text." },
                    "replace_all": { "type": "boolean", "description": "Replace every occurrence instead of requiring a unique match. Default false." },
                    "edits": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_EDITS,
                        "description": "Several replacements applied in order (instead of old_string and new_string).",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_string": { "type": "string" },
                                "new_string": { "type": "string" },
                                "replace_all": { "type": "boolean" }
                            },
                            "required": ["old_string", "new_string"]
                        }
                    }
                },
                "required": ["path"]
            }),
        ),
        FsTool::new(
            Kind::Delete,
            "delete_file",
            "Deletes one file of the workspace (not a folder). The previous content is kept, so the user can undo the delete in the Changes tab.",
            json!({
                "type": "object",
                "properties": { "path": path },
                "required": ["path"]
            }),
        ),
        FsTool::new(
            Kind::Move,
            "move_file",
            "Moves or renames one file inside the workspace (missing folders of the destination are created). The destination must not exist. The user can undo the move in the Changes tab.",
            json!({
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "The file to move, relative to the workspace folder." },
                    "to": { "type": "string", "description": "Its new path, relative to the workspace folder." }
                },
                "required": ["from", "to"]
            }),
        ),
        FsTool::new(
            Kind::List,
            "list_dir",
            "Lists the entries of one folder (not recursive): folders first, files with their size. At most 500 entries.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Folder relative to the workspace. Default: the workspace itself." }
                }
            }),
        ),
        FsTool::new(
            Kind::Glob,
            "glob",
            "Finds files by glob pattern, newest first (at most 1000). Supports `*`, `**`, `?`, `[a-z]` and `{a,b}`. A pattern without `/` matches file names at any depth (`*.rs`); with `/` it matches the path below the search folder (`src/**/*.ts`). Skips .git, node_modules and target.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "The glob pattern." },
                    "path": { "type": "string", "description": "Folder to search in. Default: the workspace." }
                },
                "required": ["pattern"]
            }),
        ),
        FsTool::new(
            Kind::Grep,
            "grep",
            "Searches file contents with a regular expression (Rust regex syntax) and returns `path:line:text` hits. Skips binary files, files over 2 MiB, .env files, .git, node_modules and target.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regular expression to look for." },
                    "path": { "type": "string", "description": "File or folder to search. Default: the workspace." },
                    "glob": { "type": "string", "description": "Only search files matching this glob (same rules as the glob tool)." },
                    "case_insensitive": { "type": "boolean", "description": "Ignore case. Default false." },
                    "max_results": { "type": "integer", "minimum": 1, "maximum": 1000, "description": "Most hits to return. Default 100." }
                },
                "required": ["pattern"]
            }),
        ),
    ];
    for tool in tools {
        registry.register(tool);
    }
}

impl AgentTool for FsTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        let env = Env::from_context(ctx)?;
        // A path outside the Folder (or in a protected place) is refused in every mode, with no card.
        let candidates: Vec<(&str, bool)> = match self.kind {
            Kind::Move => ["from", "to"]
                .iter()
                .map(|key| {
                    (
                        arguments.get(*key).and_then(Value::as_str).unwrap_or("."),
                        true,
                    )
                })
                .collect(),
            kind => vec![(
                arguments.get("path").and_then(Value::as_str).unwrap_or("."),
                matches!(kind, Kind::Write | Kind::Delete),
            )],
        };
        for (candidate, for_write) in candidates {
            if let Err(reason) = env.guard.confine(&env.workspace, candidate, for_write)? {
                return Ok(CallInspection {
                    read_only: self.spec.read_only,
                    hard_block: Some(format!("Blocked: {reason}.")),
                    ..CallInspection::default()
                });
            }
        }
        match self.kind {
            Kind::Read => {
                let args: ReadArgs = parse(arguments)?;
                Ok(env.inspection(&env.resolve(&args.path, false)?, false))
            }
            Kind::List => {
                let args: ListArgs = parse(arguments)?;
                let path = env.resolve(args.path.as_deref().unwrap_or("."), false)?;
                Ok(env.inspection(&path, false))
            }
            Kind::Glob => {
                let args: GlobArgs = parse(arguments)?;
                search::Glob::new(&args.pattern)?;
                let path = env.resolve(args.path.as_deref().unwrap_or("."), false)?;
                Ok(env.inspection(&path, false))
            }
            Kind::Grep => {
                let args: GrepArgs = parse(arguments)?;
                search::compile_regex(&args.pattern, args.case_insensitive)?;
                args.glob.as_deref().map(search::Glob::new).transpose()?;
                let path = env.resolve(args.path.as_deref().unwrap_or("."), false)?;
                Ok(env.inspection(&path, false))
            }
            Kind::Write => {
                let args: WriteArgs = parse(arguments)?;
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
                let existing = path.exists();
                let diff = match ops::read_existing_text(&path, &rel) {
                    Ok(old) => diff::unified_diff(&rel, Some(&old), &args.content, MAX_PREVIEW_BYTES),
                    Err(_) if existing => "(the existing file is binary or larger than 5 MiB; its content will be replaced)".to_owned(),
                    Err(_) => diff::unified_diff(&rel, None, &args.content, MAX_PREVIEW_BYTES),
                };
                Ok(Self::inspect_change(&env, &path, diff, existing))
            }
            Kind::Edit => {
                let args: EditArgs = parse(arguments)?;
                let path = env.resolve(&args.path, false)?;
                let rel = env.relative(&path);
                let old = ops::read_existing_text(&path, &rel)?;
                let (edited, _) = ops::apply_edits(&old, &args)
                    .map_err(|message| AppError::validation(format!("{rel}: {message}")))?;
                let diff = diff::unified_diff(&rel, Some(&old), &edited, MAX_PREVIEW_BYTES);
                Ok(Self::inspect_change(&env, &path, diff, true))
            }
            Kind::Delete => {
                let args: DeleteArgs = parse(arguments)?;
                let path = env.resolve(&args.path, true)?;
                let rel = env.relative(&path);
                let diff = match ops::read_existing_text(&path, &rel) {
                    Ok(old) => diff::unified_diff(&rel, Some(&old), "", MAX_PREVIEW_BYTES),
                    Err(_) => format!("(deletes {rel})"),
                };
                manage::check_deletable(&path, &rel)?;
                Ok(Self::inspect_change(&env, &path, diff, true))
            }
            Kind::Move => {
                let args: MoveArgs = parse(arguments)?;
                let from = env.resolve(&args.from, true)?;
                let to = env.resolve(&args.to, true)?;
                let (from_rel, to_rel) = (env.relative(&from), env.relative(&to));
                manage::check_movable(&from, &from_rel, &to, &to_rel)?;
                // The stricter of the two paths decides (a protected destination asks, a
                // destination under `.git/` is refused).
                let source = env.inspection(&from, true);
                let target = env.inspection(&to, true);
                Ok(CallInspection {
                    destructive: true,
                    preview: Some(ToolApprovalPreview::Diff {
                        path: to_rel.clone(),
                        unified_diff: format!("rename from {from_rel}\nrename to {to_rel}\n"),
                    }),
                    hard_block: source.hard_block.or(target.hard_block),
                    ask_code: source.ask_code.or(target.ask_code),
                    always_ask: source.always_ask.or(target.always_ask),
                    ..source
                })
            }
        }
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        let kind = self.kind;
        let workspace_dir = ctx.workspace_dir.map(Path::to_path_buf);
        let data_root = ctx.data_root.map(std::path::Path::to_path_buf);
        let cancel = ctx.cancel.clone();
        let session_id = ctx.session_id.to_owned();
        let hashes = ctx.read_hashes.clone();
        Box::pin(async move {
            let work = tokio::task::spawn_blocking(move || {
                let env = Env::open(
                    workspace_dir.as_deref(),
                    data_root.as_deref(),
                    &session_id,
                    hashes,
                )?;
                run(kind, &env, &arguments, &cancel)
            });
            match work.await {
                Ok(Ok((output, changes))) => {
                    if let Some(events) = ctx.events {
                        for change in changes {
                            events(ToolEvent::FileChanged {
                                path: change.path,
                                kind: change.kind,
                                added: change.added,
                                removed: change.removed,
                            });
                        }
                    }
                    output
                }
                Ok(Err(error)) => ToolOutput::error(error.to_string()),
                Err(error) => ToolOutput::error(format!("The file tool failed: {error}")),
            }
        })
    }
}

/// Runs one call to completion (blocking); a change also returns what it changed.
fn run(
    kind: Kind,
    env: &Env,
    arguments: &Value,
    cancel: &CancelToken,
) -> Result<(ToolOutput, Vec<FileChangeRecord>), AppError> {
    let plain = |output: Result<ToolOutput, AppError>| output.map(|output| (output, Vec::new()));
    let written = |applied: Result<AppliedWrite, AppError>| {
        applied.map(|applied| (applied.output, applied.changes))
    };
    match kind {
        Kind::Read => plain(ops::read_file(env, &parse(arguments)?, cancel)),
        Kind::Write => written(ops::write_file(
            env,
            &parse(arguments)?,
            cancel,
            &|_| Ok(()),
        )),
        Kind::Edit => written(ops::edit_file(env, &parse(arguments)?, cancel, &|_| Ok(()))),
        Kind::Delete => written(manage::delete_file(
            env,
            &parse(arguments)?,
            cancel,
            &|_| Ok(()),
        )),
        Kind::Move => written(manage::move_file(env, &parse(arguments)?, cancel, &|_| {
            Ok(())
        })),
        Kind::List => plain(ops::list_dir(env, &parse(arguments)?, cancel)),
        Kind::Glob => plain(search::glob_files(env, &parse(arguments)?, cancel)),
        Kind::Grep => plain(search::grep_files(env, &parse(arguments)?, cancel)),
    }
}
