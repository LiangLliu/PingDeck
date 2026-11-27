//! Tests of the per-client token use cases against a real gateway.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::json;

use super::uc_support::{Fx, chat, post, upstream_saying, watch_rows, watch_status};
use super::{
    llm_client_first_request_impl, llm_gateway_client_create_impl, llm_gateway_client_delete_impl,
    llm_gateway_client_list_impl, llm_gateway_client_revoke_impl, llm_gateway_client_rotate_impl,
    llm_gateway_client_set_policy_impl, llm_gateway_client_update_impl, llm_gateway_status_impl,
};
use crate::app::settings_update_impl;
use crate::error::AppError;
use crate::llm::store::{ClientInput, ClientLimits, ClientPolicyInput};
use crate::settings::SettingsSection;

fn all(name: &str) -> ClientInput {
    ClientInput {
        name: name.to_owned(),
        allow_all: true,
        allowed_aliases: Vec::new(),
        allowed_models: Vec::new(),
    }
}

fn only(name: &str, aliases: &[&str]) -> ClientInput {
    ClientInput {
        name: name.to_owned(),
        allow_all: false,
        allowed_aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        allowed_models: Vec::new(),
    }
}

#[test]
fn create_returns_the_token_once_and_nothing_else_does() {
    let fx = Fx::new();
    let created = llm_gateway_client_create_impl(&fx.state, all("Claude Code")).unwrap();
    assert!(created.token.starts_with("pd_cc_"), "{}", created.token);
    assert_eq!(created.token.len(), "pd_cc_".len() + 32);
    assert_eq!(created.client.token_prefix, "pd_cc_");
    assert_eq!(
        created.client.token_last4,
        created.token[created.token.len() - 4..]
    );
    let listed = llm_gateway_client_list_impl(&fx.state).unwrap();
    assert_eq!(listed, vec![created.client.clone()]);
    let json = serde_json::to_string(&listed).unwrap();
    assert!(!json.contains(&created.token), "{json}");
    assert!(!format!("{created:?}").contains(&created.token));
}

#[test]
fn bad_names_duplicates_and_empty_allow_lists_are_refused() {
    let fx = Fx::new();
    llm_gateway_client_create_impl(&fx.state, all("Codex")).unwrap();
    for input in [
        all("codex"),
        all("  "),
        only("Zed", &[]),
        all(&"n".repeat(41)),
    ] {
        let error = llm_gateway_client_create_impl(&fx.state, input).unwrap_err();
        assert!(matches!(error, AppError::Validation { .. }), "{error:?}");
    }
    assert_eq!(llm_gateway_client_list_impl(&fx.state).unwrap().len(), 1);
}

#[test]
fn clients_list_oldest_first_with_their_allow_lists() {
    let fx = Fx::new();
    llm_gateway_client_create_impl(&fx.state, all("First")).unwrap();
    llm_gateway_client_create_impl(&fx.state, only("Second", &["fast"])).unwrap();
    let listed = llm_gateway_client_list_impl(&fx.state).unwrap();
    assert_eq!(
        listed.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["First", "Second"]
    );
    assert!(listed[0].allow_all);
    assert_eq!(listed[1].allowed_aliases, ["fast"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_token_is_401_at_the_real_gateway_at_once() {
    let fx = Fx::new();
    let upstream = upstream_saying("pong").await;
    fx.wire("smart", &upstream, &["m"]);
    let port = fx.start();
    let created = llm_gateway_client_create_impl(&fx.state, all("Codex")).unwrap();
    let ok = post(
        port,
        "/v1/chat/completions",
        Some(&created.token),
        &chat("smart"),
    )
    .await;
    assert_eq!(ok.status, 200, "{}", ok.body);
    llm_gateway_client_revoke_impl(&fx.state, &created.client.id).unwrap();
    let refused = post(
        port,
        "/v1/chat/completions",
        Some(&created.token),
        &chat("smart"),
    )
    .await;
    assert_eq!(refused.status, 401, "{}", refused.body);
    assert!(!refused.body.contains(&created.token));
}

#[tokio::test(flavor = "multi_thread")]
async fn rotating_makes_the_old_token_401_and_the_new_one_work() {
    let fx = Fx::new();
    let upstream = upstream_saying("pong").await;
    fx.wire("smart", &upstream, &["m"]);
    let port = fx.start();
    let created = llm_gateway_client_create_impl(&fx.state, all("Codex")).unwrap();
    let rotated = llm_gateway_client_rotate_impl(&fx.state, &created.client.id).unwrap();
    assert_ne!(rotated.token, created.token);
    assert_eq!(rotated.client.id, created.client.id);
    assert_ne!(rotated.client.token_last4, created.client.token_last4);
    let old = post(
        port,
        "/v1/chat/completions",
        Some(&created.token),
        &chat("smart"),
    )
    .await;
    assert_eq!(old.status, 401, "{}", old.body);
    let new = post(
        port,
        "/v1/chat/completions",
        Some(&rotated.token),
        &chat("smart"),
    )
    .await;
    assert_eq!(new.status, 200, "{}", new.body);
    let json = serde_json::to_string(&llm_gateway_client_list_impl(&fx.state).unwrap()).unwrap();
    assert!(!json.contains(&rotated.token));
}

#[test]
fn a_revoked_client_cannot_be_rotated_and_unknown_ids_are_not_found() {
    let fx = Fx::new();
    let created = llm_gateway_client_create_impl(&fx.state, all("Codex")).unwrap();
    llm_gateway_client_revoke_impl(&fx.state, &created.client.id).unwrap();
    assert!(matches!(
        llm_gateway_client_rotate_impl(&fx.state, &created.client.id),
        Err(AppError::Validation { .. })
    ));
    assert!(matches!(
        llm_gateway_client_rotate_impl(&fx.state, "cl_missing"),
        Err(AppError::NotFound { .. })
    ));
    assert!(matches!(
        llm_gateway_client_revoke_impl(&fx.state, "cl_missing"),
        Err(AppError::NotFound { .. })
    ));
}

#[test]
fn only_a_revoked_client_can_be_deleted_and_its_record_goes() {
    let fx = Fx::new();
    let created = llm_gateway_client_create_impl(&fx.state, all("Codex")).unwrap();
    assert!(matches!(
        llm_gateway_client_delete_impl(&fx.state, &created.client.id),
        Err(AppError::Validation { .. })
    ));
    let revoked = llm_gateway_client_revoke_impl(&fx.state, &created.client.id).unwrap();
    assert!(revoked.revoked_at_ms.is_some());
    // Revoking again keeps the first time.
    let again = llm_gateway_client_revoke_impl(&fx.state, &created.client.id).unwrap();
    assert_eq!(again.revoked_at_ms, revoked.revoked_at_ms);
    llm_gateway_client_delete_impl(&fx.state, &created.client.id).unwrap();
    assert!(llm_gateway_client_list_impl(&fx.state).unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_updated_allow_list_applies_to_the_next_request() {
    let fx = Fx::new();
    let upstream = upstream_saying("pong").await;
    let wired = fx.wire("fast", &upstream, &["m", "big"]);
    fx.alias("deep", &[(&wired.provider_id, Some(&wired.key_id), "big")]);
    let port = fx.start();
    let created = llm_gateway_client_create_impl(&fx.state, only("Zed", &["fast"])).unwrap();
    let denied = post(
        port,
        "/v1/chat/completions",
        Some(&created.token),
        &chat("deep"),
    )
    .await;
    assert_eq!(denied.status, 403, "{}", denied.body);
    let updated = llm_gateway_client_update_impl(
        &fx.state,
        &created.client.id,
        only("Zed", &["fast", "deep"]),
    )
    .unwrap();
    assert_eq!(updated.allowed_aliases, ["fast", "deep"]);
    assert_eq!(updated.token_last4, created.client.token_last4);
    let allowed = post(
        port,
        "/v1/chat/completions",
        Some(&created.token),
        &chat("deep"),
    )
    .await;
    assert_eq!(allowed.status, 200, "{}", allowed.body);
}

#[test]
fn updating_to_a_taken_name_or_an_unknown_client_fails() {
    let fx = Fx::new();
    llm_gateway_client_create_impl(&fx.state, all("One")).unwrap();
    let two = llm_gateway_client_create_impl(&fx.state, all("Two")).unwrap();
    assert!(matches!(
        llm_gateway_client_update_impl(&fx.state, &two.client.id, all("one")),
        Err(AppError::Validation { .. })
    ));
    assert!(matches!(
        llm_gateway_client_update_impl(&fx.state, "cl_missing", all("Three")),
        Err(AppError::NotFound { .. })
    ));
}

#[test]
fn every_change_publishes_status_counts_and_clients_changed() {
    use crate::events::Sequenced;
    use crate::llm::events::LlmEvent;
    let fx = Fx::new();
    let seen = watch_status(&fx);
    let (sender, receiver) = std::sync::mpsc::channel();
    super::llm_subscribe_impl(
        &fx.state,
        std::sync::Arc::new(move |event: Sequenced<LlmEvent>| {
            if event.event == LlmEvent::ClientsChanged {
                return sender.send(()).is_ok();
            }
            true
        }),
        None,
    )
    .unwrap();
    let created = llm_gateway_client_create_impl(&fx.state, all("Codex")).unwrap();
    llm_gateway_client_update_impl(&fx.state, &created.client.id, all("Codex 2")).unwrap();
    llm_gateway_client_revoke_impl(&fx.state, &created.client.id).unwrap();
    llm_gateway_client_delete_impl(&fx.state, &created.client.id).unwrap();
    let counts: Vec<u32> = seen.try_iter().map(|s| s.clients).collect();
    assert_eq!(counts, [1, 1, 0, 0]);
    assert_eq!(llm_gateway_status_impl(&fx.state).clients, 0);
    assert_eq!(receiver.try_iter().count(), 4);
}

#[test]
fn revoking_the_last_client_turns_the_local_network_off_and_stores_it() {
    let fx = Fx::new();
    let created = llm_gateway_client_create_impl(&fx.state, all("Codex")).unwrap();
    settings_update_impl(
        &fx.state,
        SettingsSection::Ai,
        &json!({"gateway": {"serve_lan": true}}),
    )
    .unwrap();
    assert!(fx.state.settings().ai.gateway.serve_lan);
    let seen = watch_status(&fx);
    llm_gateway_client_revoke_impl(&fx.state, &created.client.id).unwrap();
    assert!(!fx.state.settings().ai.gateway.serve_lan, "stored as off");
    assert!(!fx.state.llm().settings().ai.gateway.serve_lan);
    let notices: Vec<Option<String>> = seen.try_iter().map(|s| s.notice).collect();
    assert!(
        notices.iter().any(|n| n
            .as_deref()
            .is_some_and(|n| n.contains("Local network turned off"))),
        "{notices:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_first_request_of_a_new_client_is_reported_with_its_log_row() {
    let fx = Fx::new();
    let upstream = upstream_saying("pong").await;
    fx.wire("smart", &upstream, &["m"]);
    let port = fx.start();
    let created = llm_gateway_client_create_impl(&fx.state, all("Codex")).unwrap();
    assert_eq!(
        llm_client_first_request_impl(&fx.state, &created.client.id).unwrap(),
        None
    );
    let rows = watch_rows(&fx);
    let ok = post(
        port,
        "/v1/chat/completions",
        Some(&created.token),
        &chat("smart"),
    )
    .await;
    assert_eq!(ok.status, 200, "{}", ok.body);
    let stored = rows.rows(1);
    let first = llm_client_first_request_impl(&fx.state, &created.client.id)
        .unwrap()
        .expect("seen");
    assert_eq!(Some(first.request_id), stored[0].request_id);
    assert_eq!(first.log_id, Some(stored[0].id));
    // `last_used` was written by the same request.
    let client = &llm_gateway_client_list_impl(&fx.state).unwrap()[0];
    assert!(client.last_used_ms.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_was_never_armed_is_not_watched() {
    let fx = Fx::new();
    assert_eq!(
        llm_client_first_request_impl(&fx.state, "cl_unknown").unwrap(),
        None
    );
}

fn connect_codex(model: &str, extend: Option<bool>) -> super::ClientConnectInput {
    super::ClientConnectInput {
        app: crate::client_config::ClientApp::Codex,
        alias: Some(model.to_owned()),
        fast_alias: None,
        client_name: None,
        on_existing: super::OnExisting::Rotate,
        reviewed: None,
        extend_allowlist: extend,
        unknown_model_to_default: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnecting_with_a_new_model_joins_it_into_the_allow_list_unless_declined() {
    // A reconnect that changes the model used to leave the tool 403.
    let fx = Fx::new();
    let upstream = upstream_saying("pong").await;
    fx.provider("Relay", upstream.port, &["m1", "m2", "m3"]);
    let port = fx.start();
    let token = || {
        crate::client_config::read_written_token(&fx.home, crate::client_config::ClientApp::Codex)
            .unwrap()
            .unwrap()
    };
    let first = super::client_writers::llm_client_connect_in(
        &fx.state,
        connect_codex("Relay/m1", None),
        &fx.home,
    )
    .unwrap();
    assert!(!first.client.allow_all);
    let second = super::client_writers::llm_client_connect_in(
        &fx.state,
        connect_codex("Relay/m2", None),
        &fx.home,
    )
    .unwrap();
    let models = &second.client.allowed_models;
    assert!(models.iter().any(|m| m.ends_with("/m1")) && models.iter().any(|m| m.ends_with("/m2")));
    assert!(
        second
            .client
            .default_model
            .as_deref()
            .is_some_and(|m| m.ends_with("/m2"))
    );
    let ok = post(
        port,
        "/v1/chat/completions",
        Some(&token()),
        &chat("Relay/m2"),
    )
    .await;
    assert_eq!(ok.status, 200, "{}", ok.body);
    super::client_writers::llm_client_connect_in(
        &fx.state,
        connect_codex("Relay/m3", Some(false)),
        &fx.home,
    )
    .unwrap();
    let refused = post(
        port,
        "/v1/chat/completions",
        Some(&token()),
        &chat("Relay/m3"),
    )
    .await;
    assert_eq!(refused.status, 403, "{}", refused.body);
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_maps_unknown_names_only_when_asked_and_the_first_one_starts_on_launch() {
    // Mapping a bare unknown name to the default model is an explicit choice, never implied.
    let fx = Fx::new();
    let upstream = upstream_saying("pong").await;
    fx.provider("Relay", upstream.port, &["m1"]);
    assert!(!fx.state.settings().ai.gateway.enabled_on_start);
    let claude = super::ClientConnectInput {
        app: crate::client_config::ClientApp::ClaudeCode,
        ..connect_codex("Relay/m1", None)
    };
    let connected =
        super::client_writers::llm_client_connect_in(&fx.state, claude, &fx.home).unwrap();
    assert!(!connected.client.unknown_model_to_default);
    assert!(
        fx.state.settings().ai.gateway.enabled_on_start,
        "the first Connect keeps the written address reachable after a restart"
    );
    let asked = super::ClientConnectInput {
        unknown_model_to_default: Some(true),
        ..connect_codex("Relay/m1", None)
    };
    let codex = super::client_writers::llm_client_connect_in(&fx.state, asked, &fx.home).unwrap();
    assert!(codex.client.unknown_model_to_default);
}

#[tokio::test(flavor = "multi_thread")]
async fn update_address_rewrites_the_port_and_keeps_the_token() {
    let fx = Fx::new();
    let upstream = upstream_saying("pong").await;
    fx.provider("Relay", upstream.port, &["m1"]);
    let connected = super::client_writers::llm_client_connect_in(
        &fx.state,
        connect_codex("Relay/m1", None),
        &fx.home,
    )
    .unwrap();
    let app = crate::client_config::ClientApp::Codex;
    let before = crate::client_config::read_written_token(&fx.home, app)
        .unwrap()
        .unwrap();
    // The port moves after the tool was written.
    let moved = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    settings_update_impl(
        &fx.state,
        SettingsSection::Ai,
        &json!({"gateway": {"port": moved}}),
    )
    .unwrap();
    let port = fx.start();
    assert_eq!(port, moved);
    let applied =
        super::client_writers::llm_client_rewrite_address_in(&fx.state, app, &fx.home).unwrap();
    assert!(!applied.files.is_empty());
    let after = crate::client_config::read_written_token(&fx.home, app)
        .unwrap()
        .unwrap();
    assert_eq!(before, after, "the token is not rotated");
    let written = std::fs::read_to_string(&applied.files[0].path).unwrap();
    assert!(written.contains(&format!("127.0.0.1:{port}")), "{written}");
    let clients = llm_gateway_client_list_impl(&fx.state).unwrap();
    assert_eq!(clients[0].token_last4, connected.client.token_last4);
    let ok = post(
        port,
        "/v1/chat/completions",
        Some(&after),
        &chat("Relay/m1"),
    )
    .await;
    assert_eq!(ok.status, 200, "{}", ok.body);
}

/// A Connect of `app` for the client named `name` with model `Relay/m1`.
fn connect_as(app: crate::client_config::ClientApp, name: &str) -> super::ClientConnectInput {
    super::ClientConnectInput {
        app,
        client_name: Some(name.to_owned()),
        ..connect_codex("Relay/m1", None)
    }
}

#[test]
fn a_rotation_whose_write_fails_puts_the_other_tools_back_and_keeps_the_old_token() {
    use crate::client_config::{ClientApp, read_written_token};
    let fx = Fx::new();
    fx.provider("Relay", 1, &["m1"]);
    let first = super::client_writers::llm_client_connect_in(
        &fx.state,
        connect_as(ClientApp::Codex, "Shared"),
        &fx.home,
    )
    .unwrap();
    let codex_token = read_written_token(&fx.home, ClientApp::Codex).unwrap();
    let failing: super::client_rotation::Applier<'_> =
        &|_, _, _, _, _| Err(AppError::io("disk full"));

    // Connecting Claude Code for the same client rotates it: Codex is re-tokened, then the
    // Claude Code write fails.
    let error = super::client_writers::llm_client_connect_with(
        &fx.state,
        connect_as(ClientApp::ClaudeCode, "Shared"),
        &fx.home,
        failing,
    )
    .unwrap_err();

    assert!(error.message().contains("disk full"), "{error:?}");
    assert_eq!(
        read_written_token(&fx.home, ClientApp::Codex).unwrap(),
        codex_token
    );
    let after = llm_gateway_client_list_impl(&fx.state).unwrap();
    assert_eq!(after[0].token_last4, first.client.token_last4);
}

#[test]
fn a_file_another_client_took_over_is_skipped_and_leaves_the_ledger() {
    use crate::client_config::ClientApp;
    let fx = Fx::new();
    fx.provider("Relay", 1, &["m1"]);
    let a = super::client_writers::llm_client_connect_in(
        &fx.state,
        connect_as(ClientApp::Codex, "A"),
        &fx.home,
    )
    .unwrap();
    super::client_writers::llm_client_connect_in(
        &fx.state,
        connect_as(ClientApp::Codex, "B"),
        &fx.home,
    )
    .unwrap();

    let rotated =
        super::client_writers::llm_client_rotate_and_update_in(&fx.state, &a.client.id, &fx.home)
            .unwrap();

    assert!(rotated.files.updated.is_empty());
    assert_eq!(rotated.files.skipped.len(), 1, "{:?}", rotated.files);
    assert!(
        super::client_writers::llm_client_applied_list_impl(&fx.state, &a.client.id)
            .unwrap()
            .is_empty()
    );
    // B's file still holds B's token.
    let inspected = super::client_writers::llm_client_inspect_in(
        &fx.state,
        ClientApp::Codex,
        &crate::client_config::DetectEnv::system(&fx.home),
    )
    .unwrap();
    assert_eq!(inspected.client_name.as_deref(), Some("B"));
}

#[test]
fn restore_original_asks_first_then_disconnects_the_tool_for_every_client() {
    use crate::client_config::{ClientApp, RestoreOutcome};
    let fx = Fx::new();
    fx.provider("Relay", 1, &["m1"]);
    std::fs::create_dir_all(fx.home.join(".codex")).unwrap();
    std::fs::write(fx.home.join(".codex/config.toml"), "model = \"mine\"\n").unwrap();
    for name in ["A", "B"] {
        super::client_writers::llm_client_connect_in(
            &fx.state,
            connect_as(ClientApp::Codex, name),
            &fx.home,
        )
        .unwrap();
    }

    let asked = super::client_writers::llm_client_restore_original_in(
        &fx.state,
        ClientApp::Codex,
        false,
        &fx.home,
    )
    .unwrap();
    assert!(matches!(
        asked.files[0].outcome,
        RestoreOutcome::NeedsConfirmation { .. }
    ));
    let managed = super::client_writers::llm_client_managed_list_in(&fx.state, &fx.home).unwrap();
    assert_eq!(managed.len(), 1);
    assert!(managed[0].has_original && managed[0].files.len() == 1);

    super::client_writers::llm_client_restore_original_in(
        &fx.state,
        ClientApp::Codex,
        true,
        &fx.home,
    )
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(fx.home.join(".codex/config.toml")).unwrap(),
        "model = \"mine\"\n"
    );
    let managed = super::client_writers::llm_client_managed_list_in(&fx.state, &fx.home).unwrap();
    assert!(managed[0].files.is_empty(), "{managed:?}");
    // Purging forgets the original too.
    super::client_writers::llm_client_kept_purge_in(&fx.state, ClientApp::Codex, &fx.home).unwrap();
    let managed = super::client_writers::llm_client_managed_list_in(&fx.state, &fx.home).unwrap();
    assert!(managed.is_empty(), "{managed:?}");
}

#[test]
fn a_write_after_purging_keeps_no_spanbay_content_as_the_original() {
    use crate::client_config::ClientApp;
    let fx = Fx::new();
    fx.provider("Relay", 1, &["m1"]);
    let config = fx.home.join(".codex/config.toml");
    std::fs::create_dir_all(fx.home.join(".codex")).unwrap();
    std::fs::write(&config, "model = \"mine\"\n").unwrap();
    let connect = |name: &str| {
        super::client_writers::llm_client_connect_in(
            &fx.state,
            connect_as(ClientApp::Codex, name),
            &fx.home,
        )
        .unwrap();
    };
    let has_original = || {
        super::client_writers::llm_client_managed_list_in(&fx.state, &fx.home)
            .unwrap()
            .iter()
            .any(|tool| tool.app == ClientApp::Codex && tool.has_original)
    };
    connect("A");
    super::client_writers::llm_client_kept_purge_in(&fx.state, ClientApp::Codex, &fx.home).unwrap();
    assert!(!has_original());

    // The config still holds A's connection: B's write must not keep it as the original.
    connect("B");
    assert!(!has_original());
    let restored = super::client_writers::llm_client_restore_original_in(
        &fx.state,
        ClientApp::Codex,
        true,
        &fx.home,
    )
    .unwrap();
    assert!(restored.files.is_empty(), "{restored:?}");

    // Once the config is the user's own again, the next connect keeps it as the original.
    std::fs::write(&config, "model = \"again\"\n").unwrap();
    connect("C");
    assert!(has_original());
    super::client_writers::llm_client_restore_original_in(
        &fx.state,
        ClientApp::Codex,
        true,
        &fx.home,
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        "model = \"again\"\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn client_limits_refuse_over_the_minute_and_after_expiry() {
    let fx = Fx::new();
    let upstream = upstream_saying("pong").await;
    fx.provider("Relay", upstream.port, &["m1"]);
    let port = fx.start();
    let created = llm_gateway_client_create_impl(&fx.state, all("Zed")).unwrap();
    let limited = ClientPolicyInput {
        limits: ClientLimits {
            rpm: Some(1),
            ..ClientLimits::default()
        },
        unknown_model_to_default: None,
    };
    llm_gateway_client_set_policy_impl(&fx.state, &created.client.id, limited).unwrap();
    let token = Some(created.token.as_str());
    let first = post(port, "/v1/chat/completions", token, &chat("Relay/m1")).await;
    assert_eq!(first.status, 200, "{}", first.body);
    let second = post(port, "/v1/chat/completions", token, &chat("Relay/m1")).await;
    assert_eq!(second.status, 429, "{}", second.body);
    assert!(
        second.body.contains("client_rate_limited"),
        "{}",
        second.body
    );
    let expired = ClientPolicyInput {
        limits: ClientLimits {
            expires_at_ms: Some(1),
            ..ClientLimits::default()
        },
        unknown_model_to_default: None,
    };
    llm_gateway_client_set_policy_impl(&fx.state, &created.client.id, expired).unwrap();
    let late = post(port, "/v1/chat/completions", token, &chat("Relay/m1")).await;
    assert_eq!(late.status, 401, "{}", late.body);
    // An expired token is dead on the routes that spend nothing too.
    let count = post(port, "/v1/messages/count_tokens", token, &chat("Relay/m1")).await;
    assert_eq!(count.status, 401, "{}", count.body);
    let listed = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .bearer_auth(&created.token)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status().as_u16(), 401);
    // Clearing the limits lets the client through again.
    llm_gateway_client_set_policy_impl(&fx.state, &created.client.id, ClientPolicyInput::default())
        .unwrap();
    let again = post(port, "/v1/chat/completions", token, &chat("Relay/m1")).await;
    assert_eq!(again.status, 200, "{}", again.body);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_place_in_flight_lasts_until_the_body_ends_and_a_spent_budget_is_402() {
    use crate::llm::gateway::tests::{Canned, Fake, Reply, Step, canned_json, canned_sse};
    use crate::llm::wire::Protocol;
    let mut steps = canned_sse(Protocol::OpenaiChat, "m", &Canned::text(&["one", "two"]));
    // Every content frame first, then the stream stays open a while before it ends.
    let end = steps.len() - 1;
    steps.insert(end, Step::Pause(std::time::Duration::from_secs(1)));
    let upstream = Fake::start(move |_, request| {
        if request.body["stream"] == json!(true) {
            Reply::Sse(steps.clone())
        } else {
            Reply::Json(
                200,
                canned_json(Protocol::OpenaiChat, "m", &Canned::text(&["ok"])),
            )
        }
    })
    .await;
    let fx = Fx::new();
    fx.provider("Relay", upstream.port, &["m1"]);
    let port = fx.start();
    let created = llm_gateway_client_create_impl(&fx.state, all("Zed")).unwrap();
    let token = Some(created.token.as_str());
    let one_at_a_time = ClientPolicyInput {
        limits: ClientLimits {
            max_concurrent: Some(1),
            ..ClientLimits::default()
        },
        unknown_model_to_default: None,
    };
    llm_gateway_client_set_policy_impl(&fx.state, &created.client.id, one_at_a_time).unwrap();
    let mut streamed = chat("Relay/m1");
    streamed["stream"] = json!(true);
    let open = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .bearer_auth(&created.token)
        .header("content-type", "application/json")
        .body(streamed.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(open.status().as_u16(), 200);
    let busy = post(port, "/v1/chat/completions", token, &chat("Relay/m1")).await;
    assert_eq!(busy.status, 429, "{}", busy.body);
    assert!(
        busy.body.contains("client_concurrency_limited"),
        "{}",
        busy.body
    );
    let _ = open.text().await.unwrap();
    // The server lets the place go once it has sent the last frame.
    let mut status = 0;
    for _ in 0..50 {
        status = post(port, "/v1/chat/completions", token, &chat("Relay/m1"))
            .await
            .status;
        if status == 200 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(status, 200, "the place was freed when the stream ended");

    // Spend of the client's gateway rows today, priced in the budget's currency, is over it.
    fx.state.llm().set_tz_offset_minutes(0);
    let day = crate::llm::usage::local_day(crate::app::current_time_ms(), 0);
    let row = |client_id: &str, currency: &str, cost_micro: i64| crate::llm::usage::RollupDelta {
        day: day.clone(),
        source: "gateway".to_owned(),
        provider_id: "relay".to_owned(),
        key_id: "k".to_owned(),
        model: "m1".to_owned(),
        client_id: client_id.to_owned(),
        alias: String::new(),
        currency: currency.to_owned(),
        requests: 1,
        ok_requests: 1,
        input_tokens: 1,
        output_tokens: 1,
        cached_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        cost_micro,
        failure_kind: None,
    };
    {
        let store = fx.state.store();
        let id = created.client.id.as_str();
        for (client_id, currency, cost) in [
            (id, "USD", 5_000),
            (id, "EUR", 9_000),
            ("other", "USD", 9_000),
        ] {
            crate::llm::store::upsert_rollup(store.connection(), &row(client_id, currency, cost))
                .unwrap();
        }
    }
    let budget = |daily_budget_micro| ClientPolicyInput {
        limits: ClientLimits {
            daily_budget_micro: Some(daily_budget_micro),
            ..ClientLimits::default()
        },
        unknown_model_to_default: None,
    };
    // Another client's spend and spend in another currency do not count: 5000 of 6000 used.
    llm_gateway_client_set_policy_impl(&fx.state, &created.client.id, budget(6_000)).unwrap();
    let under = post(port, "/v1/chat/completions", token, &chat("Relay/m1")).await;
    assert_eq!(under.status, 200, "{}", under.body);
    // At the limit the client is refused (the reading of a moment ago is reused).
    llm_gateway_client_set_policy_impl(&fx.state, &created.client.id, budget(5_000)).unwrap();
    let over = post(port, "/v1/chat/completions", token, &chat("Relay/m1")).await;
    assert_eq!(over.status, 402, "{}", over.body);
    assert!(
        over.body.contains("client_budget_exceeded"),
        "{}",
        over.body
    );
}
