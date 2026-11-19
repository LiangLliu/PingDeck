//! History query filter SQL builder (`HistoryFilter` -> parameterized `WHERE` clause).

use rusqlite::types::Value;

use pingdeck_model::history_filter::{HistoryFilter, executed_at_bound};

use crate::sqlite::{escape_like, to_i64};
use crate::storage::requests::http_method_to_storage;

/// A failed row: a transport failure, an HTTP error or a failed check. The "failed" filter and
/// the run totals share it.
pub(super) const FAILED_ROW_SQL: &str =
    "(result_kind = 'Failure' OR status_code >= 400 OR COALESCE(check_failed, 0) > 0)";

/// A `WHERE` fragment with its positional parameters (`?1` is always the workspace id).
pub(super) struct Clause {
    pub(super) sql: String,
    pub(super) params: Vec<Value>,
}

impl Clause {
    /// Appends `value` and returns its 1-based placeholder index.
    pub(super) fn bind(&mut self, value: Value) -> usize {
        self.params.push(value);
        self.params.len()
    }

    /// Appends ` AND {column} = ?n` binding `value`.
    pub(super) fn push_eq(&mut self, column: &'static str, value: Value) {
        let index = self.bind(value);
        self.sql.push_str(&format!(" AND {column} = ?{index}"));
    }

    /// Appends ` AND {column} IN (?a, ?b, ...)` binding `values` in order; nothing when there are
    /// none.
    pub(super) fn push_in(
        &mut self,
        column: &'static str,
        values: impl IntoIterator<Item = Value>,
    ) {
        let placeholders = values
            .into_iter()
            .map(|value| format!("?{}", self.bind(value)))
            .collect::<Vec<_>>();
        if placeholders.is_empty() {
            return;
        }
        self.sql
            .push_str(&format!(" AND {column} IN ({})", placeholders.join(", ")));
    }

    /// The free-text search: `query` (trimmed) as a literal substring of any searchable column.
    ///
    /// Short metadata columns (name, method, URLs, status, error, source, environment) are
    /// compared after `pd_casefold`, the Unicode lowercase, against the Unicode-lowercased query,
    /// so `É` matches `é`. The recorded payloads (response body preview, response headers and the
    /// sent request with its masked headers and body preview) are larger, so they are matched
    /// without folding them: `LIKE` (ASCII case-insensitive), plus an exact-case `instr` when the
    /// query has non-ASCII letters. JSON columns are matched on their stored text, where quotes and
    /// backslashes are escaped.
    fn push_search(&mut self, query: &str) {
        let folded = query.to_lowercase();
        let pattern = self.bind(Value::Text(format!("%{}%", escape_like(&folded))));
        let metadata = [
            "request_name",
            "method",
            "request_url",
            "final_url",
            "status_text",
            "COALESCE(error_kind, '')",
            "COALESCE(error_message, '')",
            "source_name",
            "environment_name",
        ]
        .iter()
        .map(|column| format!("pd_casefold({column}) LIKE ?{pattern} ESCAPE '\\'"))
        .collect::<Vec<_>>()
        .join("\n                OR ");
        let mut payload_terms = vec![
            format!("response_body_preview LIKE ?{pattern} ESCAPE '\\'"),
            format!("COALESCE(response_headers_json, '') LIKE ?{pattern} ESCAPE '\\'"),
            format!("COALESCE(sent_request_json, '') LIKE ?{pattern} ESCAPE '\\'"),
        ];
        if !query.is_ascii() {
            let exact = self.bind(Value::Text(query.to_owned()));
            payload_terms.extend([
                format!("instr(response_body_preview, ?{exact}) > 0"),
                format!("instr(COALESCE(response_headers_json, ''), ?{exact}) > 0"),
                format!("instr(COALESCE(sent_request_json, ''), ?{exact}) > 0"),
            ]);
        }
        self.sql.push_str(&format!(
            " AND (
                {metadata}
                OR CAST(status_code AS TEXT) LIKE ?{pattern} ESCAPE '\\'
                OR CAST(attempt_count AS TEXT) LIKE ?{pattern} ESCAPE '\\'
                OR (CAST(attempt_count AS TEXT) || ' attempt') LIKE ?{pattern} ESCAPE '\\'
                OR (CAST(attempt_count AS TEXT) || ' attempts') LIKE ?{pattern} ESCAPE '\\'
                OR {}
            )",
            payload_terms.join("\n                OR ")
        ));
    }

    /// The status filter: any of the selected `Nxx` classes, or a failed row.
    fn push_status(&mut self, filter: &HistoryFilter) {
        let mut terms = Vec::new();
        for class in filter.valid_status_classes() {
            let low = self.bind(Value::Integer(i64::from(class) * 100));
            let high = self.bind(Value::Integer(i64::from(class) * 100 + 99));
            terms.push(format!("(status_code BETWEEN ?{low} AND ?{high})"));
        }
        if filter.include_failed {
            terms.push(FAILED_ROW_SQL.to_owned());
        }
        if !terms.is_empty() {
            self.sql.push_str(&format!(" AND ({})", terms.join(" OR ")));
        }
    }
}

/// The `WHERE` clause of `filter` over `workspace_id`'s rows. Parameters bind in a fixed order:
/// search, sources, status, since/until, environments, methods, request, run, duration.
pub(super) fn filter_clause(workspace_id: &str, filter: &HistoryFilter) -> Clause {
    let mut clause = Clause {
        sql: "workspace_id = ?1".to_owned(),
        params: vec![Value::Text(workspace_id.to_owned())],
    };

    let query = filter.query.trim();
    if !query.is_empty() {
        clause.push_search(query);
    }
    clause.push_in(
        "source",
        filter
            .sources
            .iter()
            .map(|source| Value::Text(source.as_str().to_owned())),
    );
    clause.push_status(filter);
    if let Some(since) = filter.since_ms {
        let index = clause.bind(Value::Text(executed_at_bound(since)));
        clause
            .sql
            .push_str(&format!(" AND executed_at >= ?{index}"));
    }
    if let Some(until) = filter.until_ms {
        let index = clause.bind(Value::Text(executed_at_bound(until)));
        clause.sql.push_str(&format!(" AND executed_at < ?{index}"));
    }
    clause.push_in(
        "environment_id",
        filter
            .environment_ids
            .iter()
            .map(|id| Value::Text(id.clone())),
    );
    clause.push_in(
        "method",
        filter
            .methods
            .iter()
            .map(|method| Value::Text(http_method_to_storage(*method).to_owned())),
    );
    if let Some(request_id) = &filter.request_id {
        clause.push_eq("request_id", Value::Text(request_id.clone()));
    }
    if let Some(run_id) = &filter.run_id {
        clause.push_eq("run_id", Value::Text(run_id.clone()));
    }
    if let Some(min) = filter.min_elapsed_ms {
        let index = clause.bind(Value::Integer(to_i64(min)));
        clause.sql.push_str(&format!(" AND elapsed_ms >= ?{index}"));
    }

    clause
}
