//! Collection-runner iteration data: CSV/JSON parsing and the runner's limits.
//!
//! Single responsibility: turn the text a user pastes or loads into one data row per iteration,
//! with the exact CSV and JSON rules the runner dialog used to implement in TypeScript
//! (`fixtures/runner-data-cases.json` pins them), and own the iteration/delay ceilings that
//! [`crate::app::run_collection_impl`] enforces.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AppError;

/// Most iterations one collection run executes.
pub const RUNNER_MAX_ITERATIONS: u32 = 1000;
/// Longest delay between two requests of a run, in milliseconds.
pub const RUNNER_MAX_DELAY_MS: u64 = 60_000;

/// How [`runner_parse_data`] reads its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunnerDataKind {
    /// Always CSV (first row = column names).
    Csv,
    /// Always a JSON array of objects.
    Json,
    /// JSON when the trimmed text starts with `[` or `{`, CSV otherwise (what the UI offers).
    Auto,
}

/// The runner's ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerLimits {
    /// Most iterations per run.
    pub max_iterations: u32,
    /// Longest delay between requests, in milliseconds.
    pub max_delay_ms: u64,
}

/// Parsed iteration data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerData {
    /// One row per iteration, column name to cell text.
    pub rows: Vec<BTreeMap<String, String>>,
    /// Column names: the CSV header order, or the sorted union of the JSON keys.
    pub columns: Vec<String>,
    /// Non-fatal notes for the user.
    pub warnings: Vec<String>,
}

/// The runner's iteration and delay ceilings.
#[must_use]
pub fn runner_limits() -> RunnerLimits {
    RunnerLimits {
        max_iterations: RUNNER_MAX_ITERATIONS,
        max_delay_ms: RUNNER_MAX_DELAY_MS,
    }
}

/// Parses iteration data; blank text is no data. Errors are [`AppError::Validation`] with the
/// message the user should see (line numbers are 1-based).
pub fn runner_parse_data(text: &str, kind: RunnerDataKind) -> Result<RunnerData, AppError> {
    let trimmed = js_trim(text);
    if trimmed.is_empty() {
        return Ok(RunnerData {
            rows: Vec::new(),
            columns: Vec::new(),
            warnings: Vec::new(),
        });
    }
    let json = match kind {
        RunnerDataKind::Json => true,
        RunnerDataKind::Csv => false,
        RunnerDataKind::Auto => trimmed.starts_with(['[', '{']),
    };
    let (rows, columns) = if json {
        parse_json(trimmed)
    } else {
        parse_csv(text)
    }
    .map_err(AppError::validation)?;
    let mut warnings = Vec::new();
    if rows.len() > RUNNER_MAX_ITERATIONS as usize {
        warnings.push(format!(
            "{} data rows found, but a run executes at most {RUNNER_MAX_ITERATIONS} iterations; the extra rows are ignored.",
            rows.len()
        ));
    }
    Ok(RunnerData {
        rows,
        columns,
        warnings,
    })
}

type Parsed = (Vec<BTreeMap<String, String>>, Vec<String>);

/// JavaScript's `String.prototype.trim` whitespace: Unicode spaces plus U+FEFF, minus U+0085.
fn is_js_space(c: char) -> bool {
    c == '\u{FEFF}' || (c.is_whitespace() && c != '\u{85}')
}

fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_space)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CellState {
    Unquoted,
    Quoted,
    Closed,
}

/// Incremental CSV reader state (cells of the current row, header, finished rows).
struct Csv {
    rows: Vec<BTreeMap<String, String>>,
    headers: Option<Vec<String>>,
    cells: Vec<String>,
    cell: String,
    state: CellState,
    quoted_cell: bool,
    has_content: bool,
    line: usize,
    row_line: usize,
}

impl Csv {
    fn finish_cell(&mut self) {
        let cell = std::mem::take(&mut self.cell);
        self.cells.push(if self.quoted_cell {
            cell
        } else {
            js_trim(&cell).to_owned()
        });
        self.quoted_cell = false;
        self.state = CellState::Unquoted;
    }

    fn finish_row(&mut self) -> Result<(), String> {
        if !self.has_content {
            self.cell.clear();
            return Ok(());
        }
        self.finish_cell();
        let cells = std::mem::take(&mut self.cells);
        if let Some(headers) = &self.headers {
            if cells.len() != headers.len() {
                return Err(format!(
                    "CSV line {} has {} values; expected {}.",
                    self.row_line,
                    cells.len(),
                    headers.len()
                ));
            }
            self.rows.push(headers.iter().cloned().zip(cells).collect());
        } else {
            let headers: Vec<String> = cells.iter().map(|c| js_trim(c).to_owned()).collect();
            if headers.iter().any(String::is_empty) {
                return Err("CSV header row has an empty column name.".to_owned());
            }
            let mut seen = BTreeSet::new();
            if let Some(duplicate) = headers.iter().find(|h| !seen.insert(h.as_str())) {
                return Err(format!("CSV header repeats the column \"{duplicate}\"."));
            }
            self.headers = Some(headers);
        }
        self.has_content = false;
        Ok(())
    }

    fn quote_error(&self, message: &str) -> String {
        if self.headers.is_none() {
            format!("CSV header row has {message}.")
        } else {
            format!("CSV line {} has {message}.", self.row_line)
        }
    }
}

/// CSV: comma separated, `"` quotes (with `""` escapes and embedded newlines), unquoted cells
/// trimmed, blank lines skipped, an optional BOM, and every row exactly as wide as the header.
fn parse_csv(text: &str) -> Result<Parsed, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut p = Csv {
        rows: Vec::new(),
        headers: None,
        cells: Vec::new(),
        cell: String::new(),
        state: CellState::Unquoted,
        quoted_cell: false,
        has_content: false,
        line: 1,
        row_line: 1,
    };
    let mut index = usize::from(chars.first() == Some(&'\u{FEFF}'));
    while index <= chars.len() {
        let current = chars.get(index).copied();
        let next = chars.get(index + 1).copied();
        if p.state == CellState::Quoted {
            let Some(c) = current else {
                return Err(p.quote_error("an unclosed quote"));
            };
            match c {
                '"' if next == Some('"') => {
                    p.cell.push('"');
                    index += 1;
                }
                '"' => p.state = CellState::Closed,
                '\r' | '\n' => {
                    p.cell.push(c);
                    if c == '\r' && next == Some('\n') {
                        p.cell.push('\n');
                        index += 1;
                    }
                    p.line += 1;
                }
                _ => p.cell.push(c),
            }
        } else if matches!(current, None | Some('\r' | '\n')) {
            p.finish_row()?;
            if current == Some('\r') && next == Some('\n') {
                index += 1;
            }
            p.line += 1;
            p.row_line = p.line;
        } else if let Some(c) = current {
            match c {
                ',' => {
                    p.has_content = true;
                    p.finish_cell();
                }
                '"' => {
                    if p.state != CellState::Unquoted || !js_trim(&p.cell).is_empty() {
                        return Err(p.quote_error("an invalid quote"));
                    }
                    p.cell.clear();
                    p.quoted_cell = true;
                    p.has_content = true;
                    p.state = CellState::Quoted;
                }
                _ if p.state == CellState::Closed => {
                    if !is_js_space(c) {
                        return Err(p.quote_error("an invalid quote"));
                    }
                }
                _ => {
                    p.cell.push(c);
                    if !is_js_space(c) {
                        p.has_content = true;
                    }
                }
            }
        }
        index += 1;
    }
    Ok((p.rows, p.headers.unwrap_or_default()))
}

/// JSON: an array of objects whose values are strings, numbers, booleans or null (null is an
/// empty cell). Numbers are rendered the way JavaScript's `String(number)` does.
fn parse_json(text: &str) -> Result<Parsed, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|error| format!("Invalid JSON: {error}"))?;
    let Value::Array(entries) = value else {
        return Err("JSON data must be an array of objects.".to_owned());
    };
    let mut rows = Vec::with_capacity(entries.len());
    let mut columns = BTreeSet::new();
    for (index, entry) in entries.into_iter().enumerate() {
        let Value::Object(object) = entry else {
            return Err(format!("JSON row {} is not an object.", index + 1));
        };
        let mut row = BTreeMap::new();
        for (key, cell) in object {
            let text = match cell {
                Value::String(text) => text,
                Value::Number(number) => number
                    .as_f64()
                    .map_or_else(|| number.to_string(), js_number_string),
                Value::Bool(flag) => flag.to_string(),
                Value::Null => String::new(),
                Value::Array(_) | Value::Object(_) => {
                    return Err(format!(
                        "JSON row {}, column \"{key}\" must be a string, number, boolean, or null.",
                        index + 1
                    ));
                }
            };
            columns.insert(key.clone());
            row.insert(key, text);
        }
        rows.push(row);
    }
    Ok((rows, columns.into_iter().collect()))
}

/// ECMAScript `Number::toString(10)`: shortest round-trip digits, plain notation for exponents
/// in `-7 < e < 21`, otherwise `d.ddde±x`.
fn js_number_string(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    // `{:e}` yields the shortest digits that round-trip: `d[.ddd]e<exp>`.
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let k = digits.len() as i32;
    let n = exponent + 1; // decimal point position relative to the digits
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let power = n - 1;
        let power_sign = if power < 0 { '-' } else { '+' };
        let head = &digits[..1];
        let tail = &digits[1..];
        let fraction = if tail.is_empty() {
            String::new()
        } else {
            format!(".{tail}")
        };
        format!("{head}{fraction}e{power_sign}{}", power.abs())
    };
    format!("{sign}{body}")
}
