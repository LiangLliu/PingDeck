//! "Copy as curl", "Open in API client", export and the context-test warning.

use serde_json::Value;

use super::support::{
    add_model, attach, custom, empty_input, failing, healthy, open_state, run_ok, standard,
    upstream,
};
use crate::error::AppError;
use crate::llm::probe::export::CSV_COLUMNS;
use crate::llm::probe::test_support::spawn_fake;
use crate::llm::probe::{ContextProbeInfo, ProbeStatus, ProbeTest};
use crate::llm::store::ProviderModel;
use crate::llm::wire::Quirks;
use crate::{
    ProbeExportFilter, ProbeExportFormat, llm_context_probe_info_impl, llm_model_save_impl,
    llm_probe_export_impl, llm_probe_request_preview_impl,
};

#[tokio::test(flavor = "multi_thread")]
async fn request_preview_builds_redacted_curl_and_matching_api_client_draft() {
    let state = open_state();
    let mut quirks = Quirks::default();
    quirks
        .extra_headers
        .insert("x-team".to_owned(), "team-secret-123".to_owned());
    let a = attach(&state, "a", spawn_fake(healthy).await, quirks);

    let preview = llm_probe_request_preview_impl(&state, None, &a.key_id, "a1", ProbeTest::Chat)
        .await
        .unwrap();
    assert!(preview.curl.contains("Bearer $PROVIDER_API_KEY"));
    assert!(!preview.curl.contains(&a.secret) && !preview.curl.contains("team-secret-123"));
    assert!(preview.warnings.iter().any(|w| w.contains("x-team")));

    let draft = preview.api_client.expect("draft");
    run_ok(&state, custom(a.items(&["a1"]), &[ProbeTest::Chat])).await;
    let sent: Value = serde_json::from_str(&a.posts()[0].body).unwrap();
    let drafted: Value = serde_json::from_str(&draft.body).unwrap();
    assert_eq!(sent, drafted);

    assert!(matches!(
        llm_probe_request_preview_impl(&state, None, &a.key_id, "a1", ProbeTest::Context).await,
        Err(AppError::Unsupported { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn csv_and_json_exports_respect_filters_and_never_leak_secrets() {
    let state = open_state();
    let a = upstream(&state, "a", failing("a2", 403)).await;
    let (summary, events) = run_ok(&state, standard(a.items(&["a1", "a2"]))).await;

    let csv = llm_probe_export_impl(
        &state,
        &summary.run_id,
        &ProbeExportFilter::default(),
        ProbeExportFormat::Csv,
    )
    .unwrap();
    let text = String::from_utf8(csv.bytes).unwrap();
    assert!(text.starts_with(&CSV_COLUMNS.join(",")));
    assert_eq!(text.split("\r\n").count(), 1 + events.cells().len() + 1);
    assert!(!text.contains(&a.secret));

    let filtered = llm_probe_export_impl(
        &state,
        &summary.run_id,
        &ProbeExportFilter {
            statuses: Some(vec![ProbeStatus::Rejected]),
            ..ProbeExportFilter::default()
        },
        ProbeExportFormat::Csv,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(filtered.bytes)
            .unwrap()
            .split("\r\n")
            .count()
            - 2,
        1
    );

    let json_file = llm_probe_export_impl(
        &state,
        &summary.run_id,
        &ProbeExportFilter::default(),
        ProbeExportFormat::Json,
    )
    .unwrap();
    let doc: Value = serde_json::from_slice(&json_file.bytes).unwrap();
    assert_eq!(doc["run"]["run_id"], summary.run_id.as_str());
    assert_eq!(doc["rows"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn context_warning_and_wire_tags_keep_expected_shape() {
    let state = open_state();
    upstream(&state, "a", healthy).await;
    llm_model_save_impl(
        &state,
        ProviderModel {
            provider_id: "a".to_owned(),
            model_id: "big".to_owned(),
            context: Some(200_000),
            ..ProviderModel::default()
        },
    )
    .unwrap();
    add_model(&state, "a", "plain", true);
    assert_eq!(
        llm_context_probe_info_impl(&state, "a", "big").unwrap(),
        ContextProbeInfo::for_window(Some(200_000))
    );

    let input: crate::ProbeRunInput = serde_json::from_str(r#"{"items": []}"#).unwrap();
    assert_eq!(input, empty_input());
}
