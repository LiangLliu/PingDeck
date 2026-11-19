//! The destination and the credential of a call: provider targets are fixed by the core, custom
//! targets follow the network guard, prod guard and `ensure_sendable` send nothing, attachments
//! resolve at send time.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::support::{Fx, KEY, WS, body_for, pong, upstream};
use crate::app::{ExecuteDraftRequestInput, request_execute_draft_impl};
use crate::error::AppError;
use crate::llm::ai_run::AiRunStatus;
use crate::llm::gateway::tests::{Fake, Reply};
use crate::llm::wire::{AuthStyle, Protocol, Quirks};
use crate::workspace::{AuthConfig, Environment, KeyValue};

fn header(name: &str, value: &str) -> KeyValue {
    KeyValue {
        key: name.to_owned(),
        value: value.to_owned(),
        enabled: true,
        description: None,
    }
}

/// What a raw server received: head and body of each request.
type Recorded = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// A server that records the raw bytes of every request (head and body) and answers a fixed
/// Chat Completions JSON.
async fn raw_server() -> (u16, Recorded) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Recorded = Arc::default();
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let log = Arc::clone(&log);
            tokio::spawn(async move {
                let mut buffer = Vec::new();
                let mut chunk = [0_u8; 4096];
                let (head, body) = loop {
                    let read = socket.read(&mut chunk).await.unwrap_or(0);
                    if read == 0 {
                        return;
                    }
                    buffer.extend_from_slice(&chunk[..read]);
                    if let Some(at) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buffer[..at]).into_owned();
                        let length: usize = head
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|value| value.trim().parse().ok())
                            })
                            .unwrap_or(0);
                        if buffer.len() >= at + 4 + length {
                            break (head, buffer[at + 4..at + 4 + length].to_vec());
                        }
                    }
                };
                log.lock().unwrap().push((head, body));
                let answer =
                    crate::llm::gateway::tests::canned_json(Protocol::OpenaiChat, "m", &pong())
                        .to_string();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (port, seen)
}

#[tokio::test]
async fn the_request_body_reaches_the_provider_byte_for_byte() {
    let (port, seen) = raw_server().await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", port);
    // Odd spacing, unsorted keys, an unknown field and a number that does not survive a float.
    let body = "{ \"zeta\":1,\"model\":\"m\",  \"stream\" : false,\n\t\"big\": 12345678901234567890,\"messages\":[{\"role\":\"user\",\"content\":\"h\\u00e9llo\"}] }";
    fx.provider_request("r1", &provider, Protocol::OpenaiChat, "m", body);
    let run = fx.run("r1").await;
    assert_eq!(run.ok().status, AiRunStatus::Done);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1, body.as_bytes());
}

#[tokio::test]
async fn only_the_injected_credential_goes_out_and_the_dropped_names_are_listed() {
    let fake = upstream(Protocol::OpenaiChat, pong()).await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    let mut request = fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    request.headers = vec![
        header("Authorization", "Bearer user-token"),
        header("x-api-key", "user-key"),
        header("Host", "evil.example"),
        header("X-Goog-Api-Key", "user-goog"),
        header("X-Trace-Id", "t-1"),
    ];
    fx.save(&request);
    let run = fx.run("r1").await;
    let dto = run.ok();
    assert_eq!(dto.status, AiRunStatus::Done);
    assert_eq!(
        dto.dropped_headers,
        ["Authorization", "x-api-key", "Host", "X-Goog-Api-Key"]
    );
    let seen = &fake.requests()[0];
    assert_eq!(
        seen.header("authorization"),
        Some(&*format!("Bearer {KEY}"))
    );
    assert_eq!(seen.header("x-api-key"), None);
    assert_eq!(seen.header("x-goog-api-key"), None);
    assert_eq!(
        seen.header("host"),
        Some(&*format!("127.0.0.1:{}", fake.port))
    );
    assert_eq!(seen.header("x-trace-id"), Some("t-1"));
    let wire: Vec<&str> = dto
        .request
        .headers
        .iter()
        .map(|h| h.name.as_str())
        .collect();
    assert!(
        !wire
            .iter()
            .any(|name| name.eq_ignore_ascii_case("x-api-key")),
        "{wire:?}"
    );
}

#[tokio::test]
async fn the_providers_extra_headers_apply_and_the_user_cannot_override_them() {
    let fake = upstream(Protocol::OpenaiChat, pong()).await;
    let fx = Fx::new();
    let mut quirks = Quirks::default();
    quirks
        .extra_headers
        .insert("x-org".to_owned(), "org-1".to_owned());
    let provider = fx.provider_with(
        "Relay",
        fake.port,
        AuthStyle::Bearer,
        &[(Protocol::OpenaiChat, "/v1", None)],
        quirks,
    );
    let mut request = fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    request.headers = vec![header("X-Org", "mine")];
    fx.save(&request);
    let run = fx.run("r1").await;
    let dto = run.ok();
    assert_eq!(dto.status, AiRunStatus::Done);
    assert_eq!(fake.requests()[0].header("x-org"), Some("org-1"));
    let shown = dto
        .request
        .headers
        .iter()
        .find(|h| h.name == "x-org")
        .unwrap();
    assert_ne!(
        shown.value, "org-1",
        "injected header values are masked in the Raw view"
    );
}

#[tokio::test]
async fn a_script_cannot_move_the_destination_or_the_credential_of_a_provider_target() {
    let fake = upstream(Protocol::OpenaiChat, pong()).await;
    let evil = Fake::always(Reply::Json(200, json!({}))).await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    let mut request = fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    request.pre_request_script = format!(
        r#"
        try {{ pm.request.url = "http://127.0.0.1:{evil}/steal"; }} catch (e) {{}}
        pm.request.url.addQueryParams([{{ key: "leak", value: "1" }}]);
        pm.request.headers.add({{ key: "Authorization", value: "Bearer script-token" }});
        pm.request.headers.add({{ key: "Host", value: "127.0.0.1:{evil}" }});
        pm.request.headers.add({{ key: "X-From-Script", value: "yes" }});
        "#,
        evil = evil.port
    );
    fx.save(&request);
    let run = fx.run("r1").await;
    let dto = run.ok();
    assert_eq!(dto.status, AiRunStatus::Done, "{:?}", dto.message);
    assert_eq!(evil.count(), 0, "the second host receives nothing");
    assert_eq!(fake.count(), 1);
    let seen = &fake.requests()[0];
    assert_eq!(seen.path, "/v1/chat/completions", "no query was added");
    assert_eq!(
        seen.header("authorization"),
        Some(&*format!("Bearer {KEY}"))
    );
    assert_eq!(
        seen.header("host"),
        Some(&*format!("127.0.0.1:{}", fake.port))
    );
    assert_eq!(
        seen.header("x-from-script"),
        Some("yes"),
        "other script headers stay"
    );
    assert_eq!(dto.dropped_headers, ["Authorization", "Host"]);
}

#[tokio::test]
async fn an_imported_request_carrying_a_foreign_url_still_goes_to_the_provider_only() {
    let fake = upstream(Protocol::OpenaiChat, pong()).await;
    let evil = Fake::always(Reply::Json(200, json!({}))).await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    let mut request = fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    request.url = format!("http://127.0.0.1:{}/steal", evil.port);
    request.auth = AuthConfig::Bearer {
        token_ref: "imported-token".to_owned(),
    };
    fx.save(&request);
    let run = fx.run("r1").await;
    assert_eq!(run.ok().status, AiRunStatus::Done);
    assert_eq!(evil.count(), 0);
    let seen = &fake.requests()[0];
    assert_eq!(
        seen.header("authorization"),
        Some(&*format!("Bearer {KEY}"))
    );
}

#[tokio::test]
async fn a_provider_key_is_not_a_variable() {
    let fake = upstream(Protocol::OpenaiChat, pong()).await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    let mut request = fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    request.headers = vec![header("X-Key", "{{provider_key}}")];
    fx.save(&request);
    let run = fx.run("r1").await;
    let error = run.result.unwrap_err();
    assert!(error.message().contains("provider key"), "{error}");
    assert_eq!(fake.count(), 0);
}

#[tokio::test]
async fn the_raw_view_and_the_whole_result_never_hold_the_key() {
    let fake = upstream(Protocol::OpenaiChat, pong()).await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", true),
    );
    let run = fx.run("r1").await;
    let dto = run.ok();
    let auth = dto
        .request
        .headers
        .iter()
        .find(|h| h.name == "authorization")
        .unwrap();
    assert_eq!(auth.value, "Bearer [redacted]");
    let everything = serde_json::to_string(dto).unwrap();
    assert!(!everything.contains(KEY));
    for event in run.events.all() {
        assert!(!serde_json::to_string(&event).unwrap().contains(KEY));
    }
    assert!(
        !dto.request.url.contains("127.0.0.1"),
        "the label hides the provider's host: {}",
        dto.request.url
    );
}

#[tokio::test]
async fn the_database_file_never_holds_the_key_even_when_the_upstream_echoes_it() {
    let fake = Fake::always(Reply::Json(
        401,
        json!({"error": {"message": format!("Incorrect API key provided: {KEY}")}}),
    ))
    .await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    let held = |root: &std::path::Path| {
        let mut bytes = std::fs::read(root.join("pingdeck.sqlite3")).unwrap();
        for suffix in ["-wal", "-shm"] {
            if let Ok(more) = std::fs::read(root.join(format!("pingdeck.sqlite3{suffix}"))) {
                bytes.extend(more);
            }
        }
        bytes
            .windows(KEY.len())
            .filter(|window| *window == KEY.as_bytes())
            .count()
    };
    // The key lives in the secret store; a run must not add a second copy anywhere.
    let before = held(&fx.root);
    let run = fx.run("r1").await;
    let dto = run.ok();
    // The injected credential never appears in the result, even when the upstream echoes it.
    let message = dto.message.as_deref().unwrap();
    assert!(!message.contains(KEY), "{message}");
    assert!(
        message.contains("Incorrect API key provided: "),
        "{message}"
    );
    assert!(!serde_json::to_string(dto).unwrap().contains(KEY));
    assert!(
        !run.events
            .all()
            .iter()
            .any(|e| serde_json::to_string(e).unwrap().contains(KEY))
    );
    let log = fx.log(dto);
    assert!(!log.entry.error.clone().unwrap().contains(KEY));
    let capture = log.capture.unwrap();
    assert!(!serde_json::to_string(&capture).unwrap().contains(KEY));
    assert_eq!(
        held(&fx.root),
        before,
        "no copy of the key reached the database file"
    );
}

#[tokio::test]
async fn a_guarded_environment_sends_nothing_without_confirmation_and_one_confirmation_covers_one_call()
 {
    let fake = upstream(Protocol::OpenaiChat, pong()).await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    let mut environment = Environment::new("env_prod", WS, "Production").unwrap();
    environment.confirm_before_send = true;
    fx.state.store().save_environment(&environment).unwrap();

    let run = fx.run_with("r1", Some("env_prod"), None, true).await;
    assert!(matches!(
        run.result,
        Err(AppError::ConfirmationRequired { .. })
    ));
    assert_eq!(fake.count(), 0);
    let run = fx.run_with("r1", Some("env_prod"), Some(true), true).await;
    assert_eq!(run.ok().status, AiRunStatus::Done);
    assert_eq!(fake.count(), 1);
    let run = fx.run_with("r1", Some("env_prod"), None, true).await;
    assert!(
        matches!(run.result, Err(AppError::ConfirmationRequired { .. })),
        "a repeat asks again"
    );
    assert_eq!(fake.count(), 1);
    // Another environment is not guarded.
    let run = fx.run("r1").await;
    assert_eq!(run.ok().status, AiRunStatus::Done);
}

#[tokio::test]
async fn the_provider_key_placeholder_blocks_sending_in_both_modes() {
    let fake = upstream(Protocol::OpenaiChat, pong()).await;
    let fx = Fx::new();
    let url = format!("http://127.0.0.1:{}/v1/chat/completions", fake.port);
    let mut custom = fx.custom_request(
        "r1",
        &url,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    custom.headers = vec![header("Authorization", "Bearer {{provider key}}")];
    fx.save(&custom);
    let run = fx.run("r1").await;
    let error = run.result.unwrap_err();
    assert!(error.message().contains("Add authentication"), "{error}");

    // The HTTP pipeline refuses the same text in an HTTP-mode request.
    let mut http = custom.clone();
    http.mode = crate::workspace::ai_request::RequestMode::Http;
    http.ai = None;
    http.id = crate::workspace::RequestId::new("r2").unwrap();
    let result = request_execute_draft_impl(
        &fx.state,
        ExecuteDraftRequestInput {
            request: crate::app::SaveRequestInput {
                id: "r2".into(),
                workspace_id: WS.into(),
                name: "Plain".into(),
                protocol: http.protocol,
                method: http.method,
                url: http.url.clone(),
                headers: http.headers.clone(),
                body: http.body.clone(),
                network_options: http.network_options.clone(),
                ..Default::default()
            },
            ..Default::default()
        },
        None,
    )
    .await;
    assert!(result.unwrap_err().message().contains("Add authentication"));
    assert_eq!(fake.count(), 0, "no request reached any host");
}

/// A Send of a provider request runs it as an AI request; the agent's sends answer to
/// host rules a provider target cannot meet, so the pipeline refuses one from the agent.
#[tokio::test]
async fn the_http_pipeline_runs_a_provider_request_but_refuses_it_from_the_agent() {
    let fake = upstream(Protocol::OpenaiChat, pong()).await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    let request = fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    let sent = crate::app::request_execute_impl(
        &fx.state,
        crate::app::ExecuteRequestInput {
            workspace_id: WS.into(),
            request_id: "r1".into(),
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(sent.status_code, 200);
    assert_eq!(fake.count(), 1);

    let context = {
        let store = fx.state.store();
        crate::app::execution_context_for(&store, WS, None, None, None).unwrap()
    };
    let origin = crate::workspace::HistoryOrigin {
        source: crate::workspace::HistorySource::Agent,
        source_name: "Agent chat".to_owned(),
        source_id: "s1".to_owned(),
        run_id: Some("run_1".to_owned()),
    };
    let error = crate::app::execute_request_pipeline(
        &fx.state,
        request,
        context,
        crate::app::ExecutionMeta::default(),
        None,
        crate::app::Persistence::Record(origin),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AppError::Unsupported { .. }));
    assert_eq!(fake.count(), 1, "the agent's send reached no host");
}

#[tokio::test]
async fn a_custom_target_uses_its_own_url_headers_and_auth() {
    let fake = upstream(Protocol::OpenaiChat, pong()).await;
    let fx = Fx::new();
    let mut request = fx.custom_request(
        "r1",
        &format!("http://127.0.0.1:{}/relay/chat", fake.port),
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", true),
    );
    request.auth = AuthConfig::Bearer {
        token_ref: "custom-token".to_owned(),
    };
    request.headers = vec![header("X-Trace-Id", "t-9")];
    fx.save(&request);
    let run = fx.run("r1").await;
    let dto = run.ok();
    assert_eq!(dto.status, AiRunStatus::Done, "{:?}", dto.message);
    assert_eq!(dto.reply, "pong");
    assert!(dto.streamed && dto.ttft_ms.is_some());
    let seen = &fake.requests()[0];
    assert_eq!(seen.path, "/relay/chat");
    assert_eq!(seen.header("authorization"), Some("Bearer custom-token"));
    assert_eq!(seen.header("x-trace-id"), Some("t-9"));
    assert!(dto.dropped_headers.is_empty());
    assert!(dto.request.url.starts_with("http://127.0.0.1:"));
    let masked = dto
        .request
        .headers
        .iter()
        .find(|h| h.name == "Authorization")
        .unwrap();
    assert_eq!(masked.value, "Bearer [redacted]");
    assert!(!dto.timing.attempts[0].reused);
}

#[tokio::test]
async fn a_metadata_address_is_refused_with_blocked_address_and_a_loopback_one_is_allowed() {
    let fx = Fx::new();
    fx.custom_request(
        "r1",
        "http://169.254.169.254/latest/meta-data",
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    let run = fx.run("r1").await;
    let dto = run.ok();
    assert_eq!(dto.status, AiRunStatus::Unsupported);
    assert!(
        dto.message.as_deref().unwrap().contains("blocked_address"),
        "{:?}",
        dto.message
    );
    assert_eq!(dto.http_status, None);

    let fake = upstream(Protocol::OpenaiChat, pong()).await;
    fx.custom_request(
        "r2",
        &format!("http://127.0.0.1:{}/v1/chat/completions", fake.port),
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    assert_eq!(fx.run("r2").await.ok().status, AiRunStatus::Done);
}

#[tokio::test]
async fn a_redirect_is_shown_and_never_followed_and_the_credential_never_goes_to_the_location() {
    let target = Fake::always(Reply::Json(200, json!({}))).await;
    let location = format!("http://127.0.0.1:{}/elsewhere", target.port);
    let fake = Fake::always(Reply::WithHeaders(
        vec![("location", location.clone())],
        Box::new(Reply::Text(302, "text/plain", String::new())),
    ))
    .await;
    let fx = Fx::new();
    let mut request = fx.custom_request(
        "r1",
        &format!("http://127.0.0.1:{}/v1/chat/completions", fake.port),
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    request.auth = AuthConfig::Bearer {
        token_ref: "custom-token".to_owned(),
    };
    fx.save(&request);
    let run = fx.run("r1").await;
    let dto = run.ok();
    assert_eq!(dto.status, AiRunStatus::Rejected);
    assert_eq!(dto.http_status, Some(302));
    assert!(
        dto.message.as_deref().unwrap().contains(&location),
        "{:?}",
        dto.message
    );
    assert_eq!(target.count(), 0, "the redirect target receives nothing");
    assert_eq!(fake.count(), 1);
}

#[tokio::test]
async fn an_attachment_placeholder_is_replaced_by_the_blobs_base64_at_send_time() {
    let fake = upstream(Protocol::OpenaiChat, pong()).await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    let png: Vec<u8> = b"\x89PNG\r\n\x1a\n"
        .iter()
        .copied()
        .chain((0..2000_u32).map(|n| (n % 251) as u8))
        .collect();
    let sha = "ab12cd34ef56a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f607182939";
    let folder = fx.root.join("attachments").join(&sha[..2]);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join(sha), &png).unwrap();
    let body = format!(
        r#"{{"model":"m","stream":false,"messages":[{{"role":"user","content":[{{"type":"image_url","image_url":{{"url":"data:image/png;base64,{{{{attachment:{} 2 KB}}}}"}}}}]}}]}}"#,
        &sha[..12]
    );
    fx.provider_request("r1", &provider, Protocol::OpenaiChat, "m", &body);
    let run = fx.run("r1").await;
    let dto = run.ok();
    assert_eq!(dto.status, AiRunStatus::Done, "{:?}", dto.message);
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let expected = format!("data:image/png;base64,{}", STANDARD.encode(&png));
    let seen = &fake.requests()[0];
    assert_eq!(
        seen.body["messages"][0]["content"][0]["image_url"]["url"],
        expected.as_str()
    );
    // The Raw view and the stored capture keep the placeholder, never the data.
    assert!(dto.request.body.contains("{{attachment:ab12cd34ef56"));
    assert!(!dto.request.body.contains(&STANDARD.encode(&png)[..40]));
    assert!(dto.request.body_bytes > body.len());
    let capture = fx.log(dto).capture.unwrap();
    let stored = capture.client_req_body.unwrap().text.unwrap();
    assert!(stored.contains("{{attachment:ab12cd34ef56"));
    assert!(!stored.contains(&STANDARD.encode(&png)[..40]));
}

#[tokio::test]
async fn a_bad_attachment_reference_is_refused_and_nothing_is_sent() {
    let fake = upstream(Protocol::OpenaiChat, pong()).await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    for (index, reference) in [
        "../../../../etc/passwd",
        "ab12cd34ef56/../../x",
        "ab12",
        "0123456789ab",
    ]
    .iter()
    .enumerate()
    {
        let body = format!(
            r#"{{"model":"m","stream":false,"messages":[{{"role":"user","content":"{{{{attachment:{reference}}}}}"}}]}}"#
        );
        let id = format!("r{index}");
        fx.provider_request(&id, &provider, Protocol::OpenaiChat, "m", &body);
        let run = fx.run(&id).await;
        let dto = run.ok();
        assert_eq!(dto.status, AiRunStatus::Unsupported, "{reference}");
        assert_eq!(dto.http_status, None);
    }
    assert_eq!(fake.count(), 0);
}
