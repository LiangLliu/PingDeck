//! Tests of the models & pricing use-cases against temp states and a local HTTP server standing
//! in for models.dev: seeding, the manual sync and its failure modes, the automatic sync's
//! schedule, and that a sync never touches the user's own rows or pricing.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::super::uc_support::Fx;
use super::*;
use crate::app::settings_update_impl;
use crate::settings::SettingsSection;
use spanbay_model::model_catalog::{
    CatalogKind, CatalogOrigin, CatalogSource, CatalogSyncErrorKind, PricingMode, UserModelInput,
};

const DAY: i64 = 86_400_000;

fn body(count: usize) -> String {
    let models: Vec<String> = (0..count)
        .map(|n| {
            format!(
                "\"relay-m{n}\":{{\"name\":\"Relay {n}\",\"limit\":{{\"context\":8192,\"output\":1024}},\"cost\":{{\"input\":1,\"output\":2}}}}"
            )
        })
        .collect();
    format!(
        "{{\"relayco\":{{\"name\":\"Relay Co\",\"models\":{{{}}}}}}}",
        models.join(",")
    )
}

fn http(status: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

async fn server(response: Option<Vec<u8>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api.json", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let response = response.clone();
            tokio::spawn(async move {
                let mut buf = [0_u8; 4096];
                let _ = stream.read(&mut buf).await;
                match response {
                    Some(bytes) => {
                        let _ = stream.write_all(&bytes).await;
                        let _ = stream.shutdown().await;
                    }
                    None => tokio::time::sleep(Duration::from_secs(60)).await,
                }
            });
        }
    });
    url
}

async fn sync(
    fx: &Fx,
    url: &str,
    cancel: Option<&CancelToken>,
) -> Result<ModelCatalogSyncReport, AppError> {
    let client = fx.state.llm.http_for(url, "").unwrap();
    sync_from(&fx.state, &client, url, cancel).await
}

fn set_auto_sync(fx: &Fx, on: bool) {
    settings_update_impl(
        &fx.state,
        SettingsSection::Ai,
        &json!({"providers": {"catalog_auto_refresh": on}}),
    )
    .unwrap();
}

fn status(fx: &Fx) -> ModelCatalogStatus {
    llm_model_catalog_status_impl(&fx.state).unwrap()
}

fn user_model(family: &str, id: &str) -> UserModelInput {
    UserModelInput {
        family: family.to_owned(),
        model_id: id.to_owned(),
        name: "Mine".to_owned(),
        kind: CatalogKind::Chat,
        context_tokens: Some(4096),
        max_output_tokens: None,
        reasoning: false,
        vision: false,
        tools: true,
        input_per_mtok: Some(0.5),
        output_per_mtok: Some(1.5),
        cache_read_per_mtok: None,
        cache_write_per_mtok: None,
        reasoning_per_mtok: None,
        currency: "usd".to_owned(),
    }
}

#[tokio::test]
async fn catalog_seeds_syncs_and_preserves_user_models_and_pricing_on_failure_or_success() {
    let fx = Fx::new();
    let first = status(&fx);
    assert_eq!(first.origin, CatalogOrigin::Builtin);
    assert!(first.models_dev_count > 500);

    set_auto_sync(&fx, true);
    let saved = llm_model_catalog_config_save_impl(&fx.state, 30).unwrap();
    assert_eq!(
        (saved.auto_interval_days, saved.next_due_ms),
        (30, Some(first.snapshot_at_ms + 30 * DAY))
    );
    assert!(llm_model_catalog_config_save_impl(&fx.state, 2).is_err());

    llm_model_catalog_user_save_impl(&fx.state, user_model("relayco", "relay-m1")).unwrap();
    let wired = fx.provider("Relay", 1, &["relay-m2"]);
    let scoped = ModelPricing {
        mode: PricingMode::Custom,
        input_per_mtok: Some(3.0),
        output_per_mtok: Some(4.0),
        ..ModelPricing::dynamic(&wired.provider_id, "relay-m2")
    };
    llm_model_pricing_save_impl(&fx.state, scoped).unwrap();

    // Failed sync records error and keeps catalog intact.
    let bad_url = server(Some(http("503 Service Unavailable", "down"))).await;
    assert!(sync(&fx, &bad_url, None).await.is_err());
    assert_eq!(
        status(&fx).last_error_kind,
        Some(CatalogSyncErrorKind::Http)
    );

    // Manual sync applies remote rows without overwriting user model or custom pricing.
    let ok_url = server(Some(http("200 OK", &body(60)))).await;
    let report = sync(&fx, &ok_url, None).await.unwrap();
    assert_eq!((report.added, report.unchanged), (59, 1));
    assert_eq!(status(&fx).origin, CatalogOrigin::ModelsDev);
    assert_eq!(status(&fx).last_error, None);

    let page = llm_model_catalog_list_impl(
        &fx.state,
        ModelCatalogQuery {
            source: Some(CatalogSource::User),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert!(llm_model_pricing_delete_impl(&fx.state, &wired.provider_id, "relay-m2").unwrap());
    assert!(llm_model_catalog_user_delete_impl(&fx.state, "relayco", "relay-m1").unwrap());
}

#[tokio::test]
async fn automatic_tick_and_start_routine_respect_schedule_and_backoff() {
    let fx = Fx::new();
    let url = server(Some(http("200 OK", &body(55)))).await;
    status(&fx);
    set_auto_sync(&fx, true);
    fx.state
        .store()
        .connection()
        .execute("UPDATE model_catalog_sync SET snapshot_at_ms = 0", [])
        .unwrap();
    let cancel = CancelToken::new();
    assert!(auto_sync_tick(&fx.state, &url, &cancel).await);
    assert!(!auto_sync_tick(&fx.state, &url, &cancel).await);

    let report = crate::llm_start_routine_impl(
        &fx.state,
        crate::StartOptions {
            gateway_start_delay: None,
            skip_catalog_refresh: false,
        },
    )
    .await;
    assert!(report.catalog_ready && report.catalog_refresh_started);
    crate::stop_balance_scheduler(&fx.state);
}

#[tokio::test]
async fn manual_sync_refuses_while_another_sync_holds_the_flight() {
    let fx = Fx::new();
    let running = begin_flight(&fx.state, CATALOG_FLIGHT.to_owned()).unwrap();
    let refused = llm_model_catalog_sync_impl(&fx.state, None).await;
    assert!(
        matches!(refused, Err(AppError::Conflict { .. })),
        "{refused:?}"
    );
    drop(running);
    assert!(begin_flight(&fx.state, CATALOG_FLIGHT.to_owned()).is_some());
}

#[tokio::test]
#[ignore = "network: downloads https://models.dev/api.json"]
async fn live_sync_against_models_dev() {
    let fx = Fx::new();
    let report = llm_model_catalog_sync_impl(&fx.state, None).await.unwrap();
    assert!(report.total_remote > 5_000);
}
