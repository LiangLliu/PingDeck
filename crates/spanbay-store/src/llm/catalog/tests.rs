//! Tests of the model catalog persistence (seeding, sync, list, lookup); one module per area.
//! The models.dev parsing itself is tested in `spanbay_llm_domain::llm::models_dev`.

use std::collections::HashMap;
use std::sync::LazyLock;

use rusqlite::Connection;
use rusqlite::types::Value;

use super::sync_state::{SyncState, sync_state};
use super::*;
use spanbay_model::model_catalog::{
    CatalogOrigin, CatalogSyncErrorKind as K, DefaultBasis, ModelCatalogPage, ModelCatalogQuery,
    PricingFilter, PricingMode, UserModelInput,
};

mod list;
mod lookup;
mod seed;
mod sync;

const SAMPLE: &str = include_str!("../../../../../fixtures/models-dev-sample.json");
const DAY: i64 = 86_400_000;
const HOURS_6: i64 = 6 * 3_600_000;

static NONE_IN_USE: LazyLock<HashMap<String, u32>> = LazyLock::new(HashMap::new);

fn conn() -> Connection {
    crate::storage::migrated_memory_connection()
}

fn record(family: &str, id: &str) -> ModelRecord {
    ModelRecord {
        family: family.to_owned(),
        family_name: family.to_uppercase(),
        model_id: id.to_owned(),
        name: id.to_owned(),
        currency: "USD".to_owned(),
        input_per_mtok: Some(1.0),
        output_per_mtok: Some(2.0),
        synced_at_ms: 5,
        ..ModelRecord::default()
    }
}

fn user_model(family: &str, id: &str) -> UserModelInput {
    UserModelInput {
        family: family.to_owned(),
        model_id: id.to_owned(),
        name: "Mine".to_owned(),
        kind: CatalogKind::Chat,
        context_tokens: Some(4096),
        max_output_tokens: Some(1024),
        reasoning: true,
        vision: false,
        tools: true,
        input_per_mtok: Some(0.5),
        output_per_mtok: Some(1.5),
        cache_read_per_mtok: Some(0.1),
        cache_write_per_mtok: None,
        reasoning_per_mtok: None,
        currency: "usd".to_owned(),
    }
}

fn ids(page: &ModelCatalogPage) -> Vec<String> {
    page.rows
        .iter()
        .map(|r| format!("{}/{}", r.family, r.model_id))
        .collect()
}

fn sample_records() -> Vec<ModelRecord> {
    models_dev::parse(SAMPLE, 100).unwrap().records
}

/// Every row of `table` as raw values, in a stable order.
fn table_rows(c: &Connection, table: &str) -> Vec<Vec<Value>> {
    let mut statement = c
        .prepare(&format!("SELECT * FROM {table} ORDER BY 1, 2, 3"))
        .unwrap();
    let width = statement.column_count();
    statement
        .query_map([], |row| {
            (0..width).map(|i| row.get::<_, Value>(i)).collect()
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}
