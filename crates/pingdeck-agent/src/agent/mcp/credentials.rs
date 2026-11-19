//! Keeps MCP credentials out of stored configuration.
//!
//! Single responsibility: move the environment values of stdio servers and the header values of
//! http servers between the stored form (`{{secret:mcp.<server id>.env.<KEY>}}` placeholders) and
//! the connectable form (real values). The values live in the [`SecretStore`]; this module never
//! logs or returns one except inside the resolved config handed to the transport.
//!
//! Locking rule: the store behind [`SecretStore`] may be the application database, so none of
//! these functions may be called while holding `AppState::store()`.

use std::collections::BTreeMap;

use super::McpTransportConfig;
use pingdeck_base::error::AppError;
use pingdeck_base::secrets::{SecretHint, SecretRef, SecretStore, resolve_placeholders};

/// Names the secret of one environment/header entry of a server.
type SecretRefFn = fn(&str, &str) -> SecretRef;

/// A map of values plus the function naming the secret of one of its entries.
type Slots<'a> = (&'a mut BTreeMap<String, String>, SecretRefFn);

fn slots(config: &mut McpTransportConfig) -> Slots<'_> {
    match config {
        McpTransportConfig::Stdio { env, .. } => (env, SecretRef::mcp_env),
        McpTransportConfig::Http { headers, .. } => (headers, SecretRef::mcp_header),
    }
}

/// Only the `mcp.` secrets: a synced or edited config must not be able to send a provider key or
/// a search key to an MCP server by naming it in a placeholder.
struct McpSecrets<'a>(&'a dyn SecretStore);

impl SecretStore for McpSecrets<'_> {
    fn get(&self, secret_ref: &str) -> Result<Option<String>, AppError> {
        if secret_ref.starts_with("mcp.") {
            self.0.get(secret_ref)
        } else {
            Err(AppError::validation(format!(
                "secret `{secret_ref}` cannot be used in an MCP server configuration"
            )))
        }
    }

    fn put(&self, secret_ref: &str, plaintext: &str) -> Result<SecretHint, AppError> {
        self.0.put(secret_ref, plaintext)
    }

    fn delete(&self, secret_ref: &str) -> Result<bool, AppError> {
        self.0.delete(secret_ref)
    }

    fn hint(&self, secret_ref: &str) -> Result<SecretHint, AppError> {
        self.0.hint(secret_ref)
    }

    fn active_redaction_needles(&self) -> Vec<String> {
        self.0.active_redaction_needles()
    }
}

/// `config` with every `{{secret:KEY}}` placeholder in environment values, header values, arguments
/// and the URL replaced by the stored value. Only `mcp.*` secrets resolve.
///
/// # Errors
/// [`AppError::Validation`] naming the secret reference (never a value) when a placeholder has no
/// stored secret or names a secret outside `mcp.*`; [`AppError::Database`] when the store cannot be
/// read.
pub fn resolve_transport(
    store: &dyn SecretStore,
    config: &McpTransportConfig,
) -> Result<McpTransportConfig, AppError> {
    let store = &McpSecrets(store);
    let resolve_all = |values: &BTreeMap<String, String>| {
        values
            .iter()
            .map(|(name, value)| Ok((name.clone(), resolve_placeholders(store, value)?)))
            .collect::<Result<BTreeMap<_, _>, AppError>>()
    };
    Ok(match config {
        McpTransportConfig::Stdio {
            command,
            args,
            env,
            env_allow,
            cwd,
        } => McpTransportConfig::Stdio {
            command: command.clone(),
            args: args
                .iter()
                .map(|arg| resolve_placeholders(store, arg))
                .collect::<Result<_, _>>()?,
            env: resolve_all(env)?,
            env_allow: env_allow.clone(),
            cwd: cwd.clone(),
        },
        McpTransportConfig::Http { url, headers } => McpTransportConfig::Http {
            url: resolve_placeholders(store, url)?,
            headers: resolve_all(headers)?,
        },
    })
}

/// A short stable fingerprint (16 hex digits of a SHA-256) of a stored configuration, for
/// detecting that it changed since the user last saved it.
#[must_use]
pub fn config_fingerprint(config: &McpTransportConfig) -> String {
    // A `BTreeMap`-backed struct serialises deterministically; serialising cannot fail.
    let json = serde_json::to_vec(config).unwrap_or_default();
    pingdeck_base::crypto::sha256_hex(&json)[..16].to_owned()
}

/// Stores every non-empty environment/header value of `config` that is not yet its own placeholder
/// as the secret of `server_id` and replaces it with the placeholder. A value that is already the
/// placeholder of this very entry stays (the UI sends stored configs back unchanged); an emptied
/// value deletes its secret.
///
/// # Errors
/// [`AppError::Validation`] for a name that would corrupt a placeholder; [`AppError::Database`]
/// when the store fails.
pub fn externalize_transport(
    store: &dyn SecretStore,
    server_id: &str,
    config: &mut McpTransportConfig,
) -> Result<(), AppError> {
    let (values, secret_ref) = slots(config);
    if let Some(name) = values
        .keys()
        .find(|name| name.contains("{{") || name.contains("}}"))
    {
        return Err(AppError::validation(format!(
            "`{name}` must not contain braces"
        )));
    }
    for (name, value) in values.iter_mut() {
        let reference = secret_ref(server_id, name);
        let placeholder = reference.placeholder();
        if *value == placeholder {
            continue;
        }
        if value.is_empty() {
            store.delete(reference.as_str())?;
            continue;
        }
        store.put(reference.as_str(), value)?;
        *value = placeholder;
    }
    Ok(())
}

/// Deletes the secrets of every environment/header entry of `config` (the server is gone).
///
/// # Errors
/// [`AppError::Database`] when the store cannot be written.
pub fn delete_server_secrets(
    store: &dyn SecretStore,
    server_id: &str,
    config: &McpTransportConfig,
) -> Result<(), AppError> {
    prune_server_secrets(store, server_id, config, None)
}

/// Deletes the secrets of the entries `previous` had and `current` no longer has (all of them when
/// `current` is `None` or of the other transport kind).
///
/// # Errors
/// [`AppError::Database`] when the store cannot be written.
pub fn prune_server_secrets(
    store: &dyn SecretStore,
    server_id: &str,
    previous: &McpTransportConfig,
    current: Option<&McpTransportConfig>,
) -> Result<(), AppError> {
    let (names, secret_ref): (Vec<&String>, SecretRefFn) = match previous {
        McpTransportConfig::Stdio { env, .. } => (env.keys().collect(), SecretRef::mcp_env),
        McpTransportConfig::Http { headers, .. } => {
            (headers.keys().collect(), SecretRef::mcp_header)
        }
    };
    let kept = |name: &String| match (previous, current) {
        (McpTransportConfig::Stdio { .. }, Some(McpTransportConfig::Stdio { env, .. })) => {
            env.contains_key(name)
        }
        (McpTransportConfig::Http { .. }, Some(McpTransportConfig::Http { headers, .. })) => {
            headers.contains_key(name)
        }
        _ => false,
    };
    for name in names.into_iter().filter(|name| !kept(name)) {
        store.delete(secret_ref(server_id, name).as_str())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use pingdeck_base::secrets::{SecretHint, mask_secret_hint};

    #[derive(Default)]
    struct MapStore(Mutex<HashMap<String, String>>);

    impl SecretStore for MapStore {
        fn get(&self, secret_ref: &str) -> Result<Option<String>, AppError> {
            Ok(self.0.lock().unwrap().get(secret_ref).cloned())
        }
        fn put(&self, secret_ref: &str, plaintext: &str) -> Result<SecretHint, AppError> {
            self.0
                .lock()
                .unwrap()
                .insert(secret_ref.to_owned(), plaintext.to_owned());
            self.hint(secret_ref)
        }
        fn delete(&self, secret_ref: &str) -> Result<bool, AppError> {
            Ok(self.0.lock().unwrap().remove(secret_ref).is_some())
        }
        fn hint(&self, secret_ref: &str) -> Result<SecretHint, AppError> {
            let value = self.get(secret_ref)?;
            Ok(SecretHint {
                secret_ref: secret_ref.to_owned(),
                has_secret: value.is_some(),
                hint: value.as_deref().map(mask_secret_hint).unwrap_or_default(),
                updated_at_ms: 0,
            })
        }
        fn active_redaction_needles(&self) -> Vec<String> {
            self.0.lock().unwrap().values().cloned().collect()
        }
    }

    fn map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn stdio(env: &[(&str, &str)]) -> McpTransportConfig {
        McpTransportConfig::Stdio {
            command: "srv".to_owned(),
            args: vec!["--token={{secret:mcp.s1.env.TOKEN}}".to_owned()],
            env: map(env),
            env_allow: Vec::new(),
            cwd: None,
        }
    }

    fn http(headers: &[(&str, &str)]) -> McpTransportConfig {
        McpTransportConfig::Http {
            url: "https://example.com/mcp".to_owned(),
            headers: map(headers),
        }
    }

    #[test]
    fn externalize_resolve_validate_and_prune_server_secrets() {
        let store = MapStore::default();
        let mut config = stdio(&[("TOKEN", "tok-123456"), ("MODE", "fast"), ("EMPTY", "")]);
        externalize_transport(&store, "s1", &mut config).unwrap();
        assert_eq!(
            config,
            stdio(&[
                ("TOKEN", "{{secret:mcp.s1.env.TOKEN}}"),
                ("MODE", "{{secret:mcp.s1.env.MODE}}"),
                ("EMPTY", ""),
            ])
        );
        // Re-externalizing existing placeholder is a no-op
        externalize_transport(&store, "s1", &mut config).unwrap();
        assert_eq!(
            resolve_transport(&store, &config).unwrap(),
            McpTransportConfig::Stdio {
                command: "srv".to_owned(),
                args: vec!["--token=tok-123456".to_owned()],
                env: map(&[("TOKEN", "tok-123456"), ("MODE", "fast"), ("EMPTY", "")]),
                env_allow: Vec::new(),
                cwd: None,
            }
        );

        // Non-mcp secrets and missing secrets are rejected; braces in keys are rejected
        store.put("llm.key.k1", "sk-provider-key-123").unwrap();
        assert!(
            resolve_transport(&store, &http(&[("Authorization", "{{secret:llm.key.k1}}")]))
                .is_err()
        );
        assert!(
            resolve_transport(
                &store,
                &stdio(&[("TOKEN", "{{secret:mcp.s1.env.MISSING}}")])
            )
            .is_err()
        );
        let before = store.0.lock().unwrap().clone();
        assert!(
            externalize_transport(
                &store,
                "s1",
                &mut http(&[("Good", "value-good-1"), ("Bad}}", "value-bad-1")])
            )
            .is_err()
        );
        assert_eq!(*store.0.lock().unwrap(), before);

        // Emptying value or pruning removed keys deletes secrets
        let mut slim = stdio(&[("TOKEN", "")]);
        externalize_transport(&store, "s1", &mut slim).unwrap();
        prune_server_secrets(&store, "s1", &config, Some(&slim)).unwrap();
        assert_eq!(store.get("mcp.s1.env.TOKEN").unwrap(), None);
        assert_eq!(store.get("mcp.s1.env.MODE").unwrap(), None);
        delete_server_secrets(&store, "s1", &slim).unwrap();

        let mut remote = http(&[("Authorization", "Bearer abc-123456")]);
        externalize_transport(&store, "s2", &mut remote).unwrap();
        assert_eq!(
            resolve_transport(&store, &remote).unwrap(),
            http(&[("Authorization", "Bearer abc-123456")])
        );
        if let McpTransportConfig::Http { url, .. } = &mut remote {
            *url = "https://example.com/mcp?key={{secret:mcp.s2.header.Authorization}}".to_owned();
        }
        let resolved = resolve_transport(&store, &remote).unwrap();
        assert!(
            matches!(resolved, McpTransportConfig::Http { url, .. } if url.ends_with("key=Bearer abc-123456"))
        );
        prune_server_secrets(&store, "s2", &remote, Some(&slim)).unwrap();
        assert_eq!(store.get("mcp.s2.header.Authorization").unwrap(), None);
    }
}
