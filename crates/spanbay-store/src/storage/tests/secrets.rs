//! The secret CRUD edges and the pruning of orphaned transport secrets.

use spanbay_model::models::{TlsCertificate, TlsCertificateId, TlsCertificateKind};
use spanbay_model::{Request, Workspace};

use crate::storage::WorkspaceStore;

#[test]
fn the_hint_of_a_missing_secret_is_empty() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let hint = store.secret_hint("request.none.proxy_password").unwrap();
    assert!(!hint.has_secret);
    assert_eq!(hint.updated_at_ms, 0);
    assert!(hint.hint.is_empty());
}

#[test]
fn redaction_values_are_selected_by_characters_not_bytes() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    // Three characters, nine bytes.
    store.secret_put("llm.p1.header.x", "密码锁", 1).unwrap();
    assert!(store.secret_values_at_least(4).unwrap().is_empty());
    assert_eq!(store.secret_values_at_least(3).unwrap(), ["密码锁"]);
}

#[test]
fn pruning_drops_only_transport_secrets_whose_owner_is_gone() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let workspace = Workspace::new("ws", "Workspace").unwrap();
    store.save_workspace(&workspace).unwrap();
    store
        .save_request(&Request::new("r1", "ws", "R1", "https://a.test").unwrap())
        .unwrap();
    store
        .save_tls_certificate(&TlsCertificate {
            id: TlsCertificateId::new("cert").unwrap(),
            workspace_id: workspace.id.clone(),
            kind: TlsCertificateKind::Client,
            host_pattern: "a.test".into(),
            cert_path: "/tmp/client.pem".into(),
            key_path: None,
            passphrase_ref: None,
            enabled: true,
        })
        .unwrap();
    let refs = [
        "request.r1.proxy_password",
        "request.gone.proxy_password",
        "tls.ws.cert.passphrase",
        "tls.ws.gone.passphrase",
        "llm.p1.header.x",
        "oauth.ws.0123456789abcdef.refresh",
        "oauth.gone.0123456789abcdef.refresh",
        // Workspace `ws` does not own the tokens of a workspace `wsx` that is gone.
        "oauth.wsx.0123456789abcdef.access",
    ];
    for secret_ref in refs {
        store.secret_put(secret_ref, "value", 1).unwrap();
    }

    assert_eq!(store.prune_orphan_transport_secrets().unwrap(), 4);
    let kept: Vec<_> = refs
        .into_iter()
        .filter(|secret_ref| store.secret_get(secret_ref).unwrap().is_some())
        .collect();
    assert_eq!(
        kept,
        [
            "request.r1.proxy_password",
            "tls.ws.cert.passphrase",
            "llm.p1.header.x",
            "oauth.ws.0123456789abcdef.refresh"
        ]
    );
}

#[test]
fn deleting_a_workspace_deletes_its_oauth_tokens_only() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    for id in ["ws_1", "wsx1", "ws_1.dev"] {
        store
            .save_workspace(&Workspace::new(id, id).unwrap())
            .unwrap();
    }
    let refs = [
        "oauth.ws_1.0123456789abcdef.access",
        "oauth.ws_1.0123456789abcdef.refresh",
        // `_` would match any character under LIKE.
        "oauth.wsx1.0123456789abcdef.refresh",
        // `oauth.ws_1.` also prefixes the tokens of a workspace whose id continues with a dot.
        "oauth.ws_1.dev.0123456789abcdef.access",
    ];
    for secret_ref in refs {
        store.secret_put(secret_ref, "value", 1).unwrap();
    }
    assert!(
        store
            .delete_workspace(&spanbay_model::WorkspaceId::new("ws_1").unwrap())
            .unwrap()
    );
    let kept: Vec<_> = refs
        .into_iter()
        .filter(|secret_ref| store.secret_get(secret_ref).unwrap().is_some())
        .collect();
    assert_eq!(
        kept,
        [
            "oauth.wsx1.0123456789abcdef.refresh",
            "oauth.ws_1.dev.0123456789abcdef.access"
        ]
    );
}
