//! CSV and JSON export of a bench run.
//!
//! Single responsibility: serialise the visible rows of a run, with messages redacted and the
//! masked key hint only. CSV is UTF-8 without BOM, RFC 4180 quoting, CRLF line ends and a
//! decimal point. Pure: the core's save dialog writes the bytes.

use std::collections::HashMap;
use std::fmt::{Display, Write as _};

use serde_json::{Map, Value, json};

use super::{ProbeStatus, ProbeTest};
use crate::llm::failure::FailureKind;
use crate::llm::usage::{MAX_TZ_OFFSET_MINUTES, local_day};
use spanbay_base::secrets::redact_secrets;

/// The CSV columns, in order.
pub const CSV_COLUMNS: [&str; 16] = [
    "run_id",
    "started_at",
    "provider_id",
    "provider_name",
    "key_hint",
    "model_id",
    "check",
    "status",
    "http_status",
    "latency_ms",
    "ttft_ms",
    "tok_per_s",
    "input_tokens",
    "output_tokens",
    "failure_kind",
    "reason",
];

/// The run the rows belong to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRun {
    /// Run id.
    pub run_id: String,
    /// When the run started, Unix ms.
    pub started_at_ms: i64,
    /// Provider id.
    pub provider_id: String,
    /// Provider display name.
    pub provider_name: String,
}

/// One cell of the grid: one check of one model with one key.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportRow {
    /// The masked key hint (`sk-…3f9a`), never the secret.
    pub key_hint: String,
    /// Model id.
    pub model_id: String,
    /// The check.
    pub check: ProbeTest,
    /// The status word.
    pub status: ProbeStatus,
    /// HTTP status.
    pub http_status: Option<u16>,
    /// Latency in ms.
    pub latency_ms: Option<u64>,
    /// Time to first token in ms.
    pub ttft_ms: Option<u64>,
    /// Output tokens per second.
    pub tok_per_s: Option<f64>,
    /// Prompt tokens.
    pub input_tokens: Option<u64>,
    /// Output tokens.
    pub output_tokens: Option<u64>,
    /// Failure category.
    pub failure_kind: Option<FailureKind>,
    /// The one-line reason.
    pub reason: String,
}

/// Hours, minutes and seconds of the day of a Unix-ms time.
fn clock(ts_ms: i64) -> (i64, i64, i64) {
    let of_day = ts_ms.div_euclid(1000).rem_euclid(86_400);
    (of_day / 3600, of_day % 3600 / 60, of_day % 60)
}

/// `YYYY-MM-DDTHH:MM:SSZ` of a Unix-ms time.
fn iso_utc(ts_ms: i64) -> String {
    let (hours, minutes, seconds) = clock(ts_ms);
    format!(
        "{}T{hours:02}:{minutes:02}:{seconds:02}Z",
        local_day(ts_ms, 0)
    )
}

/// The save-dialog file name `spanbay-bench-<provider>-<YYYYMMDD-HHMMSS>.<extension>`, in the
/// user's local time (`tz_offset_minutes` east of UTC, clamped to +-14 h for the date and the
/// time alike). The provider id is reduced to `[A-Za-z0-9_-]` so it is a safe path segment.
#[must_use]
pub fn file_name(
    provider_id: &str,
    started_at_ms: i64,
    tz_offset_minutes: i16,
    extension: &str,
) -> String {
    let provider: String = provider_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let tz_offset_minutes = tz_offset_minutes.clamp(-MAX_TZ_OFFSET_MINUTES, MAX_TZ_OFFSET_MINUTES);
    let local_ms = started_at_ms.saturating_add(i64::from(tz_offset_minutes) * 60_000);
    let (hours, minutes, seconds) = clock(local_ms);
    format!(
        "spanbay-bench-{provider}-{}-{hours:02}{minutes:02}{seconds:02}.{extension}",
        local_day(started_at_ms, tz_offset_minutes).replace('-', "")
    )
}

/// A text value of the export with its secrets redacted. Line breaks and control characters are
/// kept: CSV quotes them (`push_csv_text`) and JSON escapes them.
fn clean(text: &str) -> String {
    redact_secrets(text, &[])
}

/// Appends a redacted CSV text field: text that a spreadsheet would evaluate as a formula (`=`,
/// `+`, `-`, `@`, tab, CR first) gets a leading apostrophe, and the field is quoted (RFC 4180)
/// when it has a comma, quote or line break.
fn push_csv_text(out: &mut String, text: &str) {
    let text = clean(text);
    let formula = text.starts_with(['=', '+', '-', '@', '\t', '\r']);
    let quoted = text.contains([',', '"', '\r', '\n']);
    if quoted {
        out.push('"');
    }
    if formula {
        out.push('\'');
    }
    if quoted {
        out.push_str(&text.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(&text);
    }
}

/// Appends a number field; empty when there is none.
fn push_number<T: Display>(out: &mut String, value: Option<T>) {
    if let Some(value) = value {
        // Writing to a `String` cannot fail.
        let _ = write!(out, "{value}");
    }
}

/// The CSV of `rows`: a header, then one line per row, every line ended by CRLF.
#[must_use]
pub fn to_csv(run: &ExportRun, rows: &[ExportRow]) -> String {
    let mut out = CSV_COLUMNS.join(",");
    out.push_str("\r\n");
    // The run columns are the same on every line.
    let mut run_columns = String::new();
    push_csv_text(&mut run_columns, &run.run_id);
    run_columns.push(',');
    run_columns.push_str(&iso_utc(run.started_at_ms));
    run_columns.push(',');
    push_csv_text(&mut run_columns, &run.provider_id);
    run_columns.push(',');
    push_csv_text(&mut run_columns, &run.provider_name);
    run_columns.push(',');
    for row in rows {
        out.push_str(&run_columns);
        push_csv_text(&mut out, &row.key_hint);
        out.push(',');
        push_csv_text(&mut out, &row.model_id);
        out.push(',');
        out.push_str(row.check.as_str());
        out.push(',');
        out.push_str(row.status.label());
        out.push(',');
        push_number(&mut out, row.http_status);
        out.push(',');
        push_number(&mut out, row.latency_ms);
        out.push(',');
        push_number(&mut out, row.ttft_ms);
        out.push(',');
        push_number(&mut out, row.tok_per_s);
        out.push(',');
        push_number(&mut out, row.input_tokens);
        out.push(',');
        push_number(&mut out, row.output_tokens);
        out.push(',');
        if let Some(kind) = row.failure_kind {
            out.push_str(kind.as_str());
        }
        out.push(',');
        push_csv_text(&mut out, &row.reason);
        out.push_str("\r\n");
    }
    out
}

fn check_object(row: &ExportRow) -> Value {
    json!({
        "status": row.status.label(),
        "http_status": row.http_status,
        "latency_ms": row.latency_ms,
        "ttft_ms": row.ttft_ms,
        "tok_per_s": row.tok_per_s,
        "input_tokens": row.input_tokens,
        "output_tokens": row.output_tokens,
        "failure_kind": row.failure_kind.map(FailureKind::as_str),
        "reason": clean(&row.reason),
    })
}

/// `{run, rows: [{model_id, key_hint, checks: {chat: {…}, …}}]}`: one entry per model and key,
/// in first-seen order, one object per check.
#[must_use]
pub fn to_json(run: &ExportRun, rows: &[ExportRow]) -> Value {
    let mut grouped: Vec<(&str, &str, Map<String, Value>)> = Vec::new();
    let mut slot: HashMap<(&str, &str), usize> = HashMap::new();
    for row in rows {
        let key = (row.model_id.as_str(), row.key_hint.as_str());
        let index = *slot.entry(key).or_insert_with(|| {
            grouped.push((key.0, key.1, Map::new()));
            grouped.len() - 1
        });
        grouped[index]
            .2
            .insert(row.check.as_str().to_owned(), check_object(row));
    }
    json!({
        "run": {
            "run_id": clean(&run.run_id),
            "started_at": iso_utc(run.started_at_ms),
            "provider_id": clean(&run.provider_id),
            "provider_name": clean(&run.provider_name),
        },
        "rows": grouped
            .into_iter()
            .map(|(model, hint, checks)| json!({
                "model_id": clean(model),
                "key_hint": clean(hint),
                "checks": checks,
            }))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> ExportRun {
        ExportRun {
            run_id: "run-1".to_owned(),
            started_at_ms: 1_793_975_760_000, // 2026-11-06T14:36:00Z
            provider_id: "relay".to_owned(),
            provider_name: "Relay, Acme".to_owned(),
        }
    }

    fn row(model: &str, check: ProbeTest) -> ExportRow {
        ExportRow {
            key_hint: "sk-…3f9a".to_owned(),
            model_id: model.to_owned(),
            check,
            status: ProbeStatus::Ready,
            http_status: Some(200),
            latency_ms: Some(540),
            ttft_ms: Some(120),
            tok_per_s: Some(74.0),
            input_tokens: Some(40),
            output_tokens: Some(450),
            failure_kind: None,
            reason: String::new(),
        }
    }

    #[test]
    fn csv_has_a_header_crlf_and_no_bom() {
        let csv = to_csv(&run(), &[row("gpt-6.1", ProbeTest::Stream)]);
        assert!(!csv.starts_with('\u{feff}'));
        let lines: Vec<&str> = csv.split("\r\n").collect();
        assert_eq!(
            lines.len(),
            3,
            "header, one row, trailing empty after the last CRLF"
        );
        assert_eq!(lines[0], CSV_COLUMNS.join(","));
        assert_eq!(lines[2], "");
        assert!(!csv.replace("\r\n", "").contains('\n'));
        assert_eq!(
            lines[1],
            "run-1,2026-11-06T14:36:00Z,relay,\"Relay, Acme\",sk-…3f9a,gpt-6.1,stream,Ready,200,540,120,74,40,450,,"
        );
        assert_eq!(
            lines[1].matches(',').count(),
            15 + 1,
            "one comma is inside the quoted name"
        );
    }

    #[test]
    fn csv_quotes_follow_rfc_4180() {
        let mut cell = row("m", ProbeTest::Chat);
        cell.status = ProbeStatus::Rejected;
        cell.http_status = Some(403);
        cell.failure_kind = Some(FailureKind::Auth);
        cell.reason = "He said \"no\",\r\nthen left".to_owned();
        let csv = to_csv(&run(), &[cell]);
        assert!(csv.contains("\"He said \"\"no\"\",\r\nthen left\""));
        assert!(csv.contains(",Rejected,403,"));
        assert!(csv.contains(",auth,"));
    }

    #[test]
    fn spreadsheet_formulas_are_neutralised_in_text_fields_only() {
        let mut cell = row("=HYPERLINK(\"http://x\")", ProbeTest::Chat);
        cell.reason = "+1".to_owned();
        let csv = to_csv(&run(), &[cell]);
        assert!(csv.contains("\"'=HYPERLINK(\"\"http://x\"\")\""));
        assert!(csv.contains(",'+1"));
        // Numbers are not text: a negative number would keep its sign.
        assert!(csv.contains(",540,"));
    }

    #[test]
    fn nothing_that_looks_like_a_key_survives() {
        let mut cell = row("m", ProbeTest::Chat);
        cell.reason = "bad key sk-abcdef0123456789abcdef0123".to_owned();
        let csv = to_csv(&run(), &[cell.clone()]);
        assert!(!csv.contains("sk-abcdef"), "{csv}");
        let json = to_json(&run(), &[cell]).to_string();
        assert!(!json.contains("sk-abcdef"), "{json}");
    }

    #[test]
    fn json_groups_checks_per_model_and_key() {
        let rows = [
            row("a", ProbeTest::Chat),
            row("a", ProbeTest::Stream),
            ExportRow {
                key_hint: "sk-…beef".to_owned(),
                ..row("a", ProbeTest::Chat)
            },
            row("b", ProbeTest::Chat),
        ];
        let json = to_json(&run(), &rows);
        assert_eq!(json["run"]["run_id"], "run-1");
        assert_eq!(json["run"]["started_at"], "2026-11-06T14:36:00Z");
        let listed = json["rows"].as_array().unwrap();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0]["model_id"], "a");
        assert_eq!(listed[0]["checks"]["stream"]["status"], "Ready");
        assert_eq!(listed[0]["checks"]["chat"]["latency_ms"], 540);
        assert_eq!(listed[1]["key_hint"], "sk-…beef");
        assert_eq!(listed[0]["checks"]["chat"]["failure_kind"], Value::Null);
    }

    #[test]
    fn a_full_grid_has_one_csv_line_per_cell() {
        let checks = [
            ProbeTest::Connectivity,
            ProbeTest::Auth,
            ProbeTest::Chat,
            ProbeTest::Stream,
            ProbeTest::Tools,
            ProbeTest::JsonSchema,
            ProbeTest::Vision,
            ProbeTest::Reasoning,
            ProbeTest::Cache,
            ProbeTest::Embeddings,
        ];
        let rows: Vec<ExportRow> = (0..12)
            .flat_map(|n| {
                checks
                    .iter()
                    .map(move |check| row(&format!("m{n}"), *check))
            })
            .collect();
        let csv = to_csv(&run(), &rows);
        // 12 x 10 = 120 data rows plus the header.
        assert_eq!(csv.matches("\r\n").count(), 121);
    }

    /// The date and the time of day come from the same offset, clamped to UTC+-14 h.
    #[test]
    fn an_out_of_range_offset_clamps_the_date_and_the_time_alike() {
        let started = 1_793_957_400_000; // 2026-11-06T09:30:00Z
        assert_eq!(
            file_name("relay", started, 900, "csv"),
            "spanbay-bench-relay-20261106-233000.csv"
        );
        assert_eq!(
            file_name("relay", started, -900, "csv"),
            "spanbay-bench-relay-20261105-193000.csv"
        );
    }

    #[test]
    fn file_names_use_the_local_start_time() {
        let started = 1_793_975_760_000; // 2026-11-06T14:36:00Z
        assert_eq!(
            file_name("relay", started, 0, "csv"),
            "spanbay-bench-relay-20261106-143600.csv"
        );
        assert_eq!(
            file_name("relay", started, 480, "json"),
            "spanbay-bench-relay-20261106-223600.json"
        );
        // Crossing midnight changes the date too.
        assert_eq!(
            file_name("a/b c", started, 600, "csv"),
            "spanbay-bench-a-b-c-20261107-003600.csv"
        );
    }
}
