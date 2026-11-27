//! MCP servers, tool pins, review baselines, and skill switches.

use std::collections::{BTreeMap, HashMap};

use super::*;
use crate::storage::MCP_REVIEW_PENDING;
use spanbay_model::agent::mcp::McpTransportConfig;
use spanbay_model::agent::{AgentMcpServer, AgentToolPolicy};

#[test]
fn mcp_servers_list_by_name_and_skill_switches_round_trip() {
    let store = WorkspaceStore::open_in_memory().unwrap();

    let mcp = AgentMcpServer {
        id: "1".to_owned(),
        name: "alpha".to_owned(),
        transport: McpTransportConfig::Http {
            url: "https://example.com/mcp".to_owned(),
            headers: BTreeMap::new(),
        },
        enabled: true,
        trusted: false,
        updated_at_ms: 2,
    };
    store.save_agent_mcp_server(&mcp).unwrap();
    assert_eq!(store.list_agent_mcp_servers().unwrap(), vec![mcp]);
    let stdio = AgentMcpServer {
        id: "2".to_owned(),
        name: "zeta".to_owned(),
        transport: McpTransportConfig::Stdio {
            command: "cmd".to_owned(),
            args: vec!["a".to_owned()],
            env: BTreeMap::from([("K".to_owned(), "V".to_owned())]),
            env_allow: Vec::new(),
            cwd: None,
        },
        enabled: true,
        trusted: true,
        updated_at_ms: 1,
    };
    store.save_agent_mcp_server(&stdio).unwrap();
    assert_eq!(store.list_agent_mcp_servers().unwrap()[1], stdio);
    assert!(store.delete_agent_mcp_server("1").unwrap());

    store.set_agent_skill_enabled("a", true).unwrap();
    assert!(store.list_agent_skill_flags().unwrap()["a"]);
}

/// An enabled HTTP server `id` named `name`.
fn server(id: &str, name: &str) -> AgentMcpServer {
    AgentMcpServer {
        id: id.to_owned(),
        name: name.to_owned(),
        transport: McpTransportConfig::Http {
            url: "https://example.com/mcp".to_owned(),
            headers: BTreeMap::new(),
        },
        enabled: true,
        trusted: true,
        updated_at_ms: 1,
    }
}

fn pins(entries: &[(&str, &str)]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|(tool, sha)| ((*tool).to_owned(), (*sha).to_owned()))
        .collect()
}

#[test]
fn tool_pins_re_pin_in_place_and_go_with_their_server() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    store
        .save_agent_mcp_server(&server("srv", "alpha"))
        .unwrap();

    store
        .pin_mcp_tools("srv", &pins(&[("read", "h1"), ("write", "h2")]))
        .unwrap();
    store
        .pin_mcp_tools("srv", &pins(&[("read", "h3")]))
        .unwrap();
    assert_eq!(
        store.mcp_tool_pins("srv").unwrap(),
        HashMap::from([
            ("read".to_owned(), "h3".to_owned()),
            ("write".to_owned(), "h2".to_owned())
        ])
    );
    assert_eq!(store.clear_mcp_tool_pins("srv").unwrap(), 2);
    assert!(store.mcp_tool_pins("srv").unwrap().is_empty());

    store
        .pin_mcp_tools("srv", &pins(&[("read", "h1")]))
        .unwrap();
    assert!(store.delete_agent_mcp_server("srv").unwrap());
    assert!(store.mcp_tool_pins("srv").unwrap().is_empty());
}

#[test]
fn a_failed_pin_batch_leaves_no_pin() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    store
        .save_agent_mcp_server(&server("srv", "alpha"))
        .unwrap();
    store
        .connection
        .execute_batch(
            "CREATE TRIGGER fail_pin BEFORE INSERT ON agent_mcp_tool_pins
             WHEN NEW.tool_name = 'bad'
             BEGIN SELECT RAISE(ABORT, 'boom'); END;",
        )
        .unwrap();

    assert!(
        store
            .pin_mcp_tools("srv", &pins(&[("ok", "h1"), ("bad", "h2")]))
            .is_err()
    );
    assert!(store.mcp_tool_pins("srv").unwrap().is_empty());
}

#[test]
fn the_review_baseline_is_absent_pending_or_a_fingerprint() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    store
        .save_agent_mcp_server(&server("srv", "alpha"))
        .unwrap();

    assert_eq!(store.mcp_review_baseline("srv").unwrap(), None);
    assert!(
        store
            .set_mcp_review_baseline("srv", MCP_REVIEW_PENDING)
            .unwrap()
    );
    assert_eq!(
        store.mcp_review_baseline("srv").unwrap().as_deref(),
        Some(MCP_REVIEW_PENDING)
    );
    assert!(store.set_mcp_review_baseline("srv", "fp").unwrap());
    assert_eq!(
        store.mcp_review_baseline("srv").unwrap().as_deref(),
        Some("fp")
    );
    assert!(!store.set_mcp_review_baseline("ghost", "fp").unwrap());
    assert_eq!(store.mcp_review_baseline("ghost").unwrap(), None);
}

#[test]
fn deleting_a_servers_tool_policies_matches_its_prefix_literally() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    for key in ["mcp:a_b:t", "mcp:axb:t", "shell"] {
        store
            .set_tool_policy(&AgentToolPolicy {
                tool_key: key.to_owned(),
                policy: PermissionAction::Ask,
            })
            .unwrap();
    }

    // `%` and `_` are not wildcards: nothing but the server's own prefix matches.
    assert_eq!(store.delete_mcp_tool_policies("%").unwrap(), 0);
    assert_eq!(store.delete_mcp_tool_policies("a_b").unwrap(), 1);
    let left: Vec<String> = store
        .list_tool_policy()
        .unwrap()
        .into_iter()
        .map(|policy| policy.tool_key)
        .collect();
    assert_eq!(left, ["mcp:axb:t", "shell"]);
}
