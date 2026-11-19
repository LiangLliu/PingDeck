//! The enums the agent's message rows and approval cards carry.

use serde::{Deserialize, Serialize};

/// The permission class of a tool: the capability matrix row the approval gate derives
/// from, next to the call's own read-only classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolClass {
    /// Reads files or listings of the workspace (`read_file`, `list_dir`, `glob`, `grep`,
    /// `read_skill`).
    Read,
    /// Changes workspace files (`write_file`, `edit_file`).
    Edit,
    /// Runs processes (`shell`, `shell_output`, `shell_kill`).
    Exec,
    /// Contacts the network (`fetch_url`, `web_search`, MCP tools).
    Network,
    /// Reads PingDeck's own data (requests, traces, gateway log).
    PingDeckRead,
    /// Changes or sends PingDeck data.
    PingDeckWrite,
    /// Harness bookkeeping that needs no approval (`todo_write`, `task`).
    Internal,
}

/// The origin of a tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolSource {
    /// Implemented by PingDeck (`web_search`, `fetch_url`, `read_skill`, file, shell, todo and
    /// sub-agent tools).
    Builtin,
    /// Served by the MCP server with this id.
    Mcp {
        /// The server's id.
        server_id: String,
    },
}

/// What the approval card shows before a call runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolApprovalPreview {
    /// A shell command and the directory it runs in.
    Command {
        /// The command line.
        cmd: String,
        /// Working directory.
        cwd: String,
    },
    /// A file change as a unified diff.
    Diff {
        /// Path relative to the workspace.
        path: String,
        /// The diff text.
        unified_diff: String,
    },
    /// A URL the tool will contact.
    Url {
        /// The URL.
        url: String,
    },
    /// A plain-words card for a call whose risk is not a command, a diff or a URL: the sentence
    /// the user reads before approving (`send_request`, `run_probe`).
    Summary {
        /// The sentence, shown as written.
        text: String,
    },
}

/// A structured hint for the UI about a tool's result, carried beside the text the
/// model reads.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolResultMeta {
    /// Which module opens the full view of this result; `None` when the result has no link-out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<ToolResultView>,
}

/// The link-out of a tool result: the module the UI opens for the full view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolResultView {
    /// A request's history row.
    Request {
        /// The row to open.
        history_id: String,
    },
    /// A history row's trace.
    Trace {
        /// The row to open.
        history_id: String,
    },
    /// A probe run.
    Probe {
        /// The run to open.
        run_id: String,
    },
    /// Gateway-log rows matching `filters`.
    GatewayLog {
        /// The filters the tool used.
        filters: serde_json::Value,
    },
    /// A saved request in its collection.
    SavedRequest {
        /// The request to open.
        request_id: String,
    },
}

/// The class of a tool failure the UI turns into one plain sentence and a next step. The code
/// travels as the first line of the result text (`coded_error_text`) so it survives a reload,
/// where only the text is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorCode {
    /// A PingDeck workspace tool was used in a chat with no bound workspace.
    NotBound,
    /// The bound workspace no longer exists.
    WorkspaceDeleted,
    /// The tool is not in this session's tool table (switched off or not registered).
    ToolUnavailable,
    /// The arguments could not be parsed or failed validation.
    InvalidArguments,
    /// The call did not finish in time.
    Timeout,
    /// A file path resolves outside the project folder.
    OutsideFolder,
}

impl ToolErrorCode {
    /// The wire name (`snake_case`), as written in the first result line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotBound => "not_bound",
            Self::WorkspaceDeleted => "workspace_deleted",
            Self::ToolUnavailable => "tool_unavailable",
            Self::InvalidArguments => "invalid_arguments",
            Self::Timeout => "timeout",
            Self::OutsideFolder => "outside_folder",
        }
    }
}

/// The result text of a failed tool call: a first line `{"error":{"code":"…"}}`, a newline, and
/// the English message the model reads unchanged.
#[must_use]
pub fn coded_error_text(code: ToolErrorCode, message: &str) -> String {
    format!(
        "{{\"error\":{{\"code\":\"{}\"}}}}\n{message}",
        code.as_str()
    )
}

/// Which output stream a streamed fragment belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// How a file tool changed a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    /// The file did not exist before.
    Create,
    /// An existing file was replaced or edited.
    Edit,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coded_error_text_puts_the_code_on_its_own_first_line() {
        let text = coded_error_text(
            ToolErrorCode::OutsideFolder,
            "path is outside the workspace",
        );
        assert_eq!(
            text,
            "{\"error\":{\"code\":\"outside_folder\"}}\npath is outside the workspace"
        );
        let first = text.lines().next().unwrap_or_default();
        let value: serde_json::Value = serde_json::from_str(first).unwrap();
        assert_eq!(value["error"]["code"], "outside_folder");
    }

    #[test]
    fn code_names_match_their_serde_form() {
        for code in [
            ToolErrorCode::NotBound,
            ToolErrorCode::WorkspaceDeleted,
            ToolErrorCode::ToolUnavailable,
            ToolErrorCode::InvalidArguments,
            ToolErrorCode::Timeout,
            ToolErrorCode::OutsideFolder,
        ] {
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!("\"{}\"", code.as_str()));
        }
    }
}
