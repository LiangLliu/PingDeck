//! Catalog read queries: ranked offer selection, filtering, facets, paging and single-model rows.

use std::collections::HashMap;

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, params_from_iter};

use spanbay_base::error::AppError;
use spanbay_model::model_catalog::{
    ModelCatalogFamily, ModelCatalogPage, ModelCatalogQuery, ModelCatalogRow, ModelPricing,
    PricingFilter, PricingMode,
};

use super::offers::{offers_of, stamp_default};
use super::{COLUMNS, db_error, row_record};
use crate::sqlite::{escape_like, to_u32};

const DEFAULT_PAGE: u32 = 100;
const MAX_PAGE: u32 = 500;

/// `model_pricing` columns in the order [`pricing_from_row`] reads them.
pub(crate) const PRICING_COLUMNS: &str = "provider_id, model_id, mode, per_call, input_per_mtok, output_per_mtok, cache_read_per_mtok, cache_write_per_mtok, reasoning_per_mtok, currency, updated_at_ms, catalog_family";

/// Maps the [`PRICING_COLUMNS`] that start at column `offset`; `None` when the mode column is
/// NULL (a `LEFT JOIN` that found no row).
pub(crate) fn pricing_from_row(
    row: &Row<'_>,
    offset: usize,
) -> rusqlite::Result<Option<ModelPricing>> {
    let Some(mode) = row.get::<_, Option<String>>(offset + 2)? else {
        return Ok(None);
    };
    Ok(Some(ModelPricing {
        provider_id: row.get::<_, Option<String>>(offset)?.unwrap_or_default(),
        model_id: row
            .get::<_, Option<String>>(offset + 1)?
            .unwrap_or_default(),
        mode: PricingMode::parse(&mode).unwrap_or_default(),
        per_call: row.get(offset + 3)?,
        input_per_mtok: row.get(offset + 4)?,
        output_per_mtok: row.get(offset + 5)?,
        cache_read_per_mtok: row.get(offset + 6)?,
        cache_write_per_mtok: row.get(offset + 7)?,
        reasoning_per_mtok: row.get(offset + 8)?,
        currency: row
            .get::<_, Option<String>>(offset + 9)?
            .unwrap_or_default(),
        updated_at_ms: row.get::<_, Option<i64>>(offset + 10)?.unwrap_or_default(),
        catalog_family: row.get(offset + 11)?,
    }))
}

fn qualified(table: &str, columns: &str) -> String {
    columns
        .split(", ")
        .map(|column| format!("{table}.{column}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The labs (every models.dev canonical id `<lab>/<id>` names one).
const LABS: &str = "labs AS (
        SELECT DISTINCT substr(canonical_id, 1, instr(canonical_id, '/') - 1) AS id
        FROM model_catalog WHERE instr(canonical_id, '/') > 1
    )";

/// Every catalog row with its rank among the offers of its model (`offer_rank` 1 is the row the
/// list shows: the publisher's own entry, then a priced row, then family id and model id), the
/// number of offers (`offers`) and whether a lab released it (`is_lab`; otherwise it is a
/// distributor's own entry).
fn ranked() -> String {
    format!(
        "WITH {LABS}, ranked AS (
            SELECT c.*,
                   ROW_NUMBER() OVER (PARTITION BY c.model_key ORDER BY
                       CASE WHEN c.family = c.publisher THEN 0 ELSE 1 END,
                       CASE WHEN c.input_per_mtok IS NOT NULL AND c.output_per_mtok IS NOT NULL
                            THEN 0 ELSE 1 END,
                       c.family, c.model_id) AS offer_rank,
                   COUNT(*) OVER (PARTITION BY c.model_key) AS offers,
                   c.publisher IN (SELECT id FROM labs) AS is_lab
            FROM model_catalog c
        )"
    )
}

/// The model's every-provider pricing row (the newest when several ids of it have one).
const GLOBAL_JOIN: &str = "LEFT JOIN model_pricing g ON g.rowid = (
        SELECT p.rowid FROM model_pricing p WHERE p.provider_id = '' AND p.model_key = c.model_key
        ORDER BY p.updated_at_ms DESC, p.model_id LIMIT 1)";

/// The one list order: newest release first (`YYYY-MM-DD` and `YYYY-MM` compare as text; within
/// one month a dated day sorts before the bare month), undated rows (every user row) last, then
/// display name, family id, model id.
const LIST_ORDER: &str =
    "c.release_date IS NULL, c.release_date DESC, c.name COLLATE NOCASE, c.family, c.model_id";

/// The order of one model's offers: the publisher's own entry first, then by provider name.
const OFFER_ORDER: &str = "CASE WHEN c.family = c.publisher THEN 0 ELSE 1 END, c.family_name COLLATE NOCASE, c.family, c.model_id";

/// The `SELECT` over [`ranked`] rows matching `filter`, joined with their global pricing row, in
/// `order`; `LIMIT` and `OFFSET` are the placeholders numbered `limit_param` and `offset_param`.
/// `filter` and `order` are trusted SQL fragments.
fn select_sql(filter: &str, order: &str, limit_param: usize, offset_param: usize) -> String {
    format!(
        "{ranked}
         SELECT {catalog}, {pricing},
                (SELECT COUNT(DISTINCT p.provider_id) FROM model_pricing p
                 WHERE p.model_key = c.model_key AND p.provider_id <> ''),
                c.model_key, c.publisher, c.publisher_name, c.offers
         FROM ranked c
         {GLOBAL_JOIN}
         {filter}
         ORDER BY {order}
         LIMIT ?{limit_param} OFFSET ?{offset_param}",
        ranked = ranked(),
        catalog = qualified("c", COLUMNS),
        pricing = qualified("g", PRICING_COLUMNS),
    )
}

/// `args` followed by the `LIMIT` and `OFFSET` values, with the placeholder numbers of the two.
fn with_paging(args: &[Value], limit: i64, offset: i64) -> (Vec<Value>, usize, usize) {
    let mut all = args.to_vec();
    all.push(Value::Integer(limit));
    let limit_param = all.len();
    all.push(Value::Integer(offset));
    let offset_param = all.len();
    (all, limit_param, offset_param)
}

/// Where a [`select_sql`] row's pricing columns start, and where the columns after them start.
fn layout() -> (usize, usize) {
    let width = COLUMNS.split(", ").count();
    (width, width + PRICING_COLUMNS.split(", ").count())
}

/// Maps a [`select_sql`] row laid out as [`layout`] says.
fn list_row(
    row: &Row<'_>,
    width: usize,
    overrides_column: usize,
) -> rusqlite::Result<ModelCatalogRow> {
    let record = row_record(row)?;
    let model_key: String = row.get(overrides_column + 1)?;
    let pricing =
        pricing_from_row(row, width)?.unwrap_or_else(|| ModelPricing::dynamic("", &model_key));
    let overrides = to_u32(row.get(overrides_column)?);
    Ok(ModelCatalogRow {
        model_key,
        publisher: row.get(overrides_column + 2)?,
        publisher_name: row.get(overrides_column + 3)?,
        provider_count: to_u32(row.get(overrides_column + 4)?),
        ..ModelCatalogRow::new(record, pricing, overrides)
    })
}

/// One page of the [`select_sql`] rows. User values travel in `args`.
fn select_rows(
    conn: &Connection,
    filter: &str,
    args: &[Value],
    order: &str,
    limit: i64,
    offset: i64,
) -> rusqlite::Result<Vec<ModelCatalogRow>> {
    let (args, limit_param, offset_param) = with_paging(args, limit, offset);
    let sql = select_sql(filter, order, limit_param, offset_param);
    let (width, overrides_column) = layout();
    // `prepare`, not `prepare_cached`: `filter` yields one SQL text per combination of list
    // filters, more variants than the 64-statement cache holds.
    conn.prepare(&sql)?
        .query_map(params_from_iter(&args), |row| {
            list_row(row, width, overrides_column)
        })?
        .collect()
}

/// The first [`select_sql`] row matching `filter` in [`LIST_ORDER`]. User values travel in `args`.
pub(super) fn first_row(
    conn: &Connection,
    filter: &'static str,
    args: &[Value],
) -> rusqlite::Result<Option<ModelCatalogRow>> {
    let (args, limit_param, offset_param) = with_paging(args, 1, 0);
    let sql = select_sql(filter, LIST_ORDER, limit_param, offset_param);
    let (width, overrides_column) = layout();
    // A constant `filter` gives every caller one fixed SQL text, so the statement is cached.
    conn.prepare_cached(&sql)?
        .query_row(params_from_iter(&args), |row| {
            list_row(row, width, overrides_column)
        })
        .optional()
}

/// Any offer of the model matches the `LIKE` pattern `?{n}` in its id, name, provider or
/// publisher (lower-cased); the model's row is listed.
fn search_clause(n: usize) -> String {
    let like = |column: &str| format!("lower(s.{column}) LIKE ?{n} ESCAPE '\\'");
    format!(
        "c.model_key IN (SELECT s.model_key FROM model_catalog s WHERE {})",
        [
            "model_id",
            "name",
            "family",
            "family_name",
            "publisher",
            "publisher_name"
        ]
        .map(like)
        .join(" OR ")
    )
}

/// The rows whose global pricing row is of `mode` (`None`: dynamic, and no offer has a price).
fn mode_clause(mode: PricingFilter) -> String {
    let dynamic = "COALESCE(g.mode, 'dynamic') = 'dynamic'";
    match mode {
        PricingFilter::Dynamic => dynamic.to_owned(),
        PricingFilter::Fixed => "g.mode = 'fixed'".to_owned(),
        PricingFilter::Custom => "g.mode = 'custom'".to_owned(),
        PricingFilter::None => format!(
            "{dynamic} AND NOT EXISTS (SELECT 1 FROM model_catalog o
                     WHERE o.model_key = c.model_key
                       AND o.input_per_mtok IS NOT NULL AND o.output_per_mtok IS NOT NULL)"
        ),
    }
}

/// `WHERE …` and its arguments for `query`'s filters (not the paging): one row per model, or the
/// offers of `query.model_key`. `in_use` is the JSON array of the keys the user's providers serve.
fn filter(query: &ModelCatalogQuery, in_use: &str) -> (String, Vec<Value>) {
    if let Some(key) = &query.model_key {
        return (
            "WHERE c.model_key = ?1".to_owned(),
            vec![Value::Text(key.clone())],
        );
    }
    let mut clauses: Vec<String> = vec!["c.offer_rank = 1".to_owned()];
    let mut args: Vec<Value> = Vec::new();
    // Binds `value` and returns its placeholder number.
    let mut arg = |value: Value| {
        args.push(value);
        args.len()
    };
    if query.distributor_entries == Some(true) {
        clauses.push("NOT c.is_lab AND c.source <> 'user'".to_owned());
    } else if query.publisher.is_none() && query.family.is_none() {
        // Lab models, the user's own and whatever the user's providers serve.
        let n = arg(Value::Text(in_use.to_owned()));
        clauses.push(format!(
            "(c.is_lab OR c.source = 'user' OR c.model_key IN (SELECT value FROM json_each(?{n})))"
        ));
    }
    if query.in_use == Some(true) {
        let n = arg(Value::Text(in_use.to_owned()));
        clauses.push(format!(
            "c.model_key IN (SELECT value FROM json_each(?{n}))"
        ));
    }
    if let Some(text) = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let escaped = escape_like(&text.to_ascii_lowercase());
        let n = arg(Value::Text(format!("%{escaped}%")));
        clauses.push(search_clause(n));
    }
    if let Some(family) = &query.family {
        let n = arg(Value::Text(family.clone()));
        clauses.push(format!(
            "c.model_key IN (SELECT s.model_key FROM model_catalog s WHERE s.family = ?{n})"
        ));
    }
    if let Some(publisher) = &query.publisher {
        let n = arg(Value::Text(publisher.clone()));
        clauses.push(format!("c.publisher = ?{n}"));
    }
    if let Some(kind) = query.kind {
        let n = arg(Value::Text(kind.as_str().to_owned()));
        clauses.push(format!("c.kind = ?{n}"));
    }
    if let Some(source) = query.source {
        let n = arg(Value::Text(source.as_str().to_owned()));
        clauses.push(format!("c.source = ?{n}"));
    }
    if let Some(mode) = query.mode {
        clauses.push(mode_clause(mode));
    }
    (format!("WHERE {}", clauses.join(" AND ")), args)
}

/// How many [`ranked`] rows match `filter_sql` (the paging aside).
fn match_count(conn: &Connection, filter_sql: &str, args: &[Value]) -> rusqlite::Result<i64> {
    // `query_row` prepares without caching: `filter_sql` varies like the list's.
    conn.query_row(
        &format!(
            "{ranked}
                 SELECT COUNT(*) FROM ranked c
                 {GLOBAL_JOIN}
                 {filter_sql}",
            ranked = ranked()
        ),
        params_from_iter(args),
        |row| row.get(0),
    )
}

/// `(id, name, count[, lab])` facet rows of `sql`.
fn facets(
    conn: &Connection,
    sql: &str,
    with_lab: bool,
) -> rusqlite::Result<Vec<ModelCatalogFamily>> {
    conn.prepare_cached(sql)?
        .query_map([], |row| {
            Ok(ModelCatalogFamily {
                id: row.get(0)?,
                name: row.get(1)?,
                models: to_u32(row.get(2)?),
                lab: with_lab && row.get::<_, bool>(3)?,
            })
        })?
        .collect()
}

/// Every family with its row count, by provider name.
fn family_facets(conn: &Connection) -> rusqlite::Result<Vec<ModelCatalogFamily>> {
    facets(
        conn,
        "SELECT family, MAX(family_name), COUNT(*) FROM model_catalog
             GROUP BY family ORDER BY MAX(family_name) COLLATE NOCASE, family",
        false,
    )
}

/// Every publisher with its model count, labs first.
fn publisher_facets(conn: &Connection) -> rusqlite::Result<Vec<ModelCatalogFamily>> {
    facets(
        conn,
        &format!(
            "WITH {LABS}
                 SELECT publisher, MAX(publisher_name), COUNT(DISTINCT model_key),
                        MAX(publisher IN (SELECT id FROM labs))
                 FROM model_catalog
                 GROUP BY publisher
                 ORDER BY 4 DESC, MAX(publisher_name) COLLATE NOCASE, publisher"
        ),
        true,
    )
}

/// One page of the catalog: one row per model matching `query` in `LIST_ORDER` (or the offers
/// of `query.model_key` in `OFFER_ORDER`), the match count, every family with its row count
/// and every publisher with its model count (labs first). The default list holds the lab models,
/// the user's own and every model in `in_use` (model key → how many of the user's providers serve
/// it); a listed row carries its model's default catalog price (official, else reference).
///
/// # Errors
/// [`AppError::database`] on any `SQLite` failure.
pub fn list_page(
    conn: &Connection,
    query: &ModelCatalogQuery,
    in_use: &HashMap<String, u32>,
) -> Result<ModelCatalogPage, AppError> {
    let keys: Vec<&str> = in_use.keys().map(String::as_str).collect();
    let keys_json = serde_json::to_string(&keys).unwrap_or_else(|_| "[]".to_owned());
    let (filter_sql, args) = filter(query, &keys_json);
    let order = if query.model_key.is_some() {
        OFFER_ORDER
    } else {
        LIST_ORDER
    };
    let limit = query.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let offset = query.offset.unwrap_or(0);
    let run = || -> rusqlite::Result<ModelCatalogPage> {
        Ok(ModelCatalogPage {
            rows: select_rows(
                conn,
                &filter_sql,
                &args,
                order,
                i64::from(limit),
                i64::from(offset),
            )?,
            total: to_u32(match_count(conn, &filter_sql, &args)?),
            families: family_facets(conn)?,
            publishers: publisher_facets(conn)?,
        })
    };
    let mut page = run().map_err(db_error)?;
    if query.model_key.is_none() {
        for row in &mut page.rows {
            stamp_default(row, &offers_of(conn, &row.model_key)?);
            row.in_use = in_use.get(&row.model_key).copied().filter(|n| *n > 0);
        }
    }
    Ok(page)
}

/// The listed row of one model (its default catalog price stamped), `None` when the catalog has
/// no row of `model_key`.
///
/// # Errors
/// [`AppError::database`] on any `SQLite` failure.
pub(crate) fn model_row(
    conn: &Connection,
    model_key: &str,
    in_use: u32,
) -> Result<Option<ModelCatalogRow>, AppError> {
    let row = first_row(
        conn,
        "WHERE c.offer_rank = 1 AND c.model_key = ?1",
        &[Value::Text(model_key.to_owned())],
    )
    .map_err(db_error)?;
    let Some(mut row) = row else {
        return Ok(None);
    };
    stamp_default(&mut row, &offers_of(conn, model_key)?);
    row.in_use = Some(in_use).filter(|n| *n > 0);
    Ok(Some(row))
}
