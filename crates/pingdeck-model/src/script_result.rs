//! What a script reports back: console output, `pm.test` outcomes and the failure that ended it.

use serde::{Deserialize, Serialize};

/// Severity of a console entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsoleLevel {
    Log,
    Info,
    Warn,
    Error,
    Debug,
}

/// One `console.*` call captured from a script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleEntry {
    pub level: ConsoleLevel,
    pub message: String,
    pub source: String,
}

/// Outcome of one `pm.test`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptTestResult {
    pub name: String,
    pub passed: bool,
    #[serde(default)]
    pub skipped: bool,
    pub error: Option<String>,
    pub source: String,
}

/// An uncaught script failure and the script that raised it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptError {
    pub source: String,
    pub message: String,
}
