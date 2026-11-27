//! Mock hits as history rows: row content, the writer thread and its back-pressure rules.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::workspace::models::{
    ExecutionHistoryEntry, HistoryOriginRef, HistorySource, KeyValue, Workspace, WorkspaceId,
};
use crate::workspace::{HistoryFilter, WorkspaceStore};

use super::super::history::{MockHistorySink, MockHistoryWriter, hit_row};
use super::super::http::ParsedRequest;
use super::super::reply::MockReply;
use super::super::server::MockServerManager;
use super::support::*;

fn store_with_workspace(id: &str) -> Arc<Mutex<WorkspaceStore>> {
    let store = WorkspaceStore::open_in_memory().expect("store should open");
    store
        .save_workspace(&Workspace::new(id, "Workspace").expect("workspace"))
        .expect("workspace should save");
    Arc::new(Mutex::new(store))
}

fn rows_of(store: &Mutex<WorkspaceStore>, workspace: &str) -> Vec<ExecutionHistoryEntry> {
    store
        .lock()
        .unwrap()
        .query_execution_history(workspace, &HistoryFilter::default(), 200, None)
        .expect("history should load")
        .entries
}

fn request(method: &str, path: &str, query: &str) -> ParsedRequest {
    ParsedRequest {
        method: method.into(),
        path: path.into(),
        query: query.into(),
        headers: Vec::new(),
        ..ParsedRequest::default()
    }
}

fn reply(status: u16, body: &str, route_id: Option<&str>) -> MockReply {
    let mut reply = MockReply::json(
        status,
        &serde_json::Value::Null,
        route_id.map(str::to_owned),
    );
    reply.headers = vec![("content-type".into(), "text/plain".into())];
    reply.body = bytes::Bytes::copy_from_slice(body.as_bytes());
    reply
}

/// A row for the `mock_flow` server of workspace `ws_mock`, distinguishable by `number`.
fn row(number: usize) -> ExecutionHistoryEntry {
    hit_row(
        &server(Vec::new()),
        "127.0.0.1:4010",
        "log_1",
        &request("GET", &format!("/n{number}"), ""),
        &reply(200, "ok", None),
        1,
    )
    .expect("a GET is recordable")
}

#[test]
fn served_requests_record_history_rows_with_redacted_auth_and_skip_unsupported_methods() {
    let store = store_with_workspace("ws_mock");
    let writer = Arc::new(MockHistoryWriter::with_store(Arc::clone(&store)).unwrap());
    let manager = MockServerManager::with_history(Arc::clone(&writer) as Arc<dyn MockHistorySink>);
    let mut ok_route = route(
        "route_ok",
        "GET",
        "/ok/:id",
        vec![response("resp_ok", 200, "fine", vec![])],
    );
    ok_route.responses[0].headers = vec![KeyValue {
        key: "X-Mock".into(),
        value: "1".into(),
        enabled: true,
        description: None,
    }];
    let mut config = server(vec![
        ok_route,
        route(
            "route_refund",
            "POST",
            "/refunds",
            vec![response("resp_refund", 201, "created", vec![])],
        ),
    ]);
    config.name = "Orders mock".into();
    let bound = manager.start(config).unwrap();

    assert_eq!(get(bound, "/ok/7?x=1").0, 200);
    assert_eq!(get(bound, "/missing").0, 404);
    send(bound, "PURGE /x HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    let body = r#"{"amount":123}"#;
    send(
        bound,
        &format!(
            "POST /refunds?x=1 HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer super-secret-token-value\r\nX-Team: billing\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    );
    writer.flush();

    assert_eq!(manager.logs("mock_flow").len(), 4);
    let rows = rows_of(&store, "ws_mock");
    assert_eq!(rows.len(), 3, "PURGE is not recorded in history");

    let hit = rows.iter().find(|r| r.status_code == 200).unwrap();
    assert_eq!(
        (
            &hit.source,
            hit.source_name.as_str(),
            hit.request_name.as_str(),
            hit.request_url.as_str()
        ),
        (
            &HistorySource::Mock,
            "Orders mock",
            "GET /ok/:id",
            format!("http://{bound}/ok/7?x=1").as_str()
        )
    );
    // The row points at its traffic row and the traffic row back at it.
    let traffic = manager.logs("mock_flow");
    let log = traffic
        .iter()
        .find(|log| log.history_entry_id.as_deref() == Some(hit.id.as_str()))
        .expect("the traffic row names the history row");
    assert_eq!(
        hit.origin_ref,
        Some(HistoryOriginRef::MockTxn { id: log.id.clone() })
    );
    let detail = store
        .lock()
        .unwrap()
        .get_execution_history("ws_mock", &hit.id)
        .unwrap()
        .unwrap();
    assert!(
        detail
            .response_headers
            .iter()
            .any(|h| h.key.eq_ignore_ascii_case("x-mock") && h.value == "1")
    );

    let miss = rows.iter().find(|r| r.status_code == 404).unwrap();
    assert_eq!(miss.request_name, "/missing");

    let refund_id = &rows.iter().find(|r| r.status_code == 201).unwrap().id;
    let refund = store
        .lock()
        .unwrap()
        .get_execution_history("ws_mock", refund_id)
        .unwrap()
        .unwrap();
    let sent = refund.sent_request.expect("sent request stored");
    assert_eq!(
        (
            sent.method.as_str(),
            sent.url.as_str(),
            sent.body_preview.as_str()
        ),
        ("POST", format!("http://{bound}/refunds?x=1").as_str(), body)
    );
    let auth = sent
        .headers
        .iter()
        .find(|h| h.key.eq_ignore_ascii_case("authorization"))
        .unwrap();
    assert!(auth.value.starts_with("Bearer ") && !auth.value.contains("super-secret-token-value"));
}

#[test]
fn writer_skips_deleted_workspaces_drops_when_full_and_flushes_on_drop() {
    let store = store_with_workspace("ws_mock");
    let writer = MockHistoryWriter::with_store(Arc::clone(&store)).unwrap();
    let mut orphan = row(1);
    orphan.workspace_id = WorkspaceId::new("ws_gone").unwrap();
    writer.submit(orphan);
    for number in 2..=151 {
        writer.submit(row(number));
    }
    writer.flush();
    assert_eq!(rows_of(&store, "ws_mock").len(), 150);
    assert!(rows_of(&store, "ws_gone").is_empty());

    let (entered_tx, entered_rx) = mpsc::channel();
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let persisted = Arc::new(Mutex::new(Vec::<String>::new()));
    let writer = {
        let persisted = Arc::clone(&persisted);
        MockHistoryWriter::spawn(2, move |rows| {
            if persisted.lock().unwrap().is_empty() {
                entered_tx.send(()).unwrap();
                gate_rx.recv().unwrap();
            }
            persisted
                .lock()
                .unwrap()
                .extend(rows.iter().map(|r| r.request_url.clone()));
        })
        .unwrap()
    };
    writer.submit(row(1));
    entered_rx.recv().unwrap();
    for number in 2..=6 {
        writer.submit(row(number));
    }
    assert_eq!(writer.dropped(), 3);
    gate_tx.send(()).unwrap();
    drop(writer);
    assert_eq!(
        *persisted.lock().unwrap(),
        [
            "http://127.0.0.1:4010/n1",
            "http://127.0.0.1:4010/n2",
            "http://127.0.0.1:4010/n3"
        ]
    );
}
