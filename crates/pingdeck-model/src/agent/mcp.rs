//! How to reach an MCP server.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How to reach an MCP server.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpTransportConfig {
    /// Spawn a child process and talk newline-delimited JSON-RPC over its stdio.
    Stdio {
        /// Executable to run.
        command: String,
        /// Arguments of the executable.
        #[serde(default)]
        args: Vec<String>,
        /// Variables added to the minimal base environment. Stored configs hold `{{secret:…}}`
        /// placeholders here; [`resolve_transport`] swaps in the values.
        #[serde(default)]
        env: BTreeMap<String, String>,
        /// Names of parent-process variables passed through besides the base set
        /// ([`BASE_ENV`]); the user's explicit choice, so no name is refused.
        #[serde(default)]
        env_allow: Vec<String>,
        /// Working directory (an absolute path). Default: the session's workspace, else
        /// `<data dir>/mcp/<server id>`; never the home directory.
        #[serde(default)]
        cwd: Option<String>,
    },
    /// Talk streamable HTTP to this endpoint.
    Http {
        /// The MCP endpoint (http or https).
        url: String,
        /// Extra headers sent with every request (for example `Authorization`). Stored configs
        /// hold `{{secret:…}}` placeholders as values.
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

impl std::fmt::Debug for McpTransportConfig {
    /// Names only: after [`resolve_transport`] the environment and header values (and arguments)
    /// hold the secrets themselves, and a URL may carry a token, so none of them is printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stdio {
                command,
                args,
                env,
                env_allow,
                cwd,
            } => f
                .debug_struct("Stdio")
                .field("command", command)
                .field("args", &format_args!("<{} args>", args.len()))
                .field("env", &env.keys().collect::<Vec<_>>())
                .field("env_allow", env_allow)
                .field("cwd", cwd)
                .finish(),
            Self::Http { url, headers } => f
                .debug_struct("Http")
                .field(
                    "origin",
                    &url::Url::parse(url).map_or_else(
                        |_| "<invalid url>".to_owned(),
                        |url| url.origin().ascii_serialization(),
                    ),
                )
                .field("headers", &headers.keys().collect::<Vec<_>>())
                .finish(),
        }
    }
}
