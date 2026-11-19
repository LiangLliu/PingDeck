//! What an import may change on a provider: one policy for
//! config sync and any later importer. It writes through the store functions that hold the
//! invariants (`save_provider_with`, `save_key`), on the caller's transaction, so a file goes in
//! whole or not at all; the caller sends the events after it commits.
//!
//! Defaults: local keys the file does not list stay (`keep_local_keys`), local models it does not
//! list stay (`apply_removals` off); an empty secret in the file leaves the stored one as it is; a
//! key this machine does not know arrives switched off and after the local ones; a known key keeps
//! its local on/off state (switching a key on sends its secret, a local decision). A provider the
//! file brings in is marked "not reviewed": no fallback target until the user reviews it.

use std::collections::HashMap;

use pingdeck_store::DbConnection as Connection;

use crate::error::AppError;
use crate::llm::store::{
    self, ModelRef, Provider, ProviderKey, ProviderModel, SaveProviderOptions,
};

/// A key as an import carries it: the metadata and the secret text of the file (empty: none).
pub struct ImportedKey {
    pub key: ProviderKey,
    pub secret: String,
}

/// One provider document of an import.
pub struct ImportedProvider<'a> {
    pub provider: &'a Provider,
    pub keys: &'a [ImportedKey],
    pub models: &'a [ProviderModel],
}

/// The user's choices for one provider.
#[derive(Debug, Clone, Copy)]
pub struct ImportOptions {
    /// Local keys the file does not list stay (default on).
    pub keep_local_keys: bool,
    /// Local models the file does not list are deleted (default off).
    pub apply_removals: bool,
    /// The provider moves to another host or proxy and the user did not keep the keys for it:
    /// every key ends switched off (values kept).
    pub detach_keys: bool,
    /// The user chose "Keep my key for the new host": a move keeps the keys' on/off state.
    pub keys_kept_for_new_host: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            keep_local_keys: true,
            apply_removals: false,
            detach_keys: false,
            keys_kept_for_new_host: false,
        }
    }
}

/// What [`apply_imported_provider`] did, for the events and the notes the caller reports.
#[derive(Debug, Default)]
pub struct ImportOutcome {
    pub provider_id: String,
    /// Keys this machine did not know, written switched off.
    pub new_keys_off: usize,
}

/// Writes one imported provider with its keys and models on `tx` (see the module notes). The
/// provider's and keys' own stamps are used as the write time, so a re-export reproduces them.
///
/// # Errors
/// What the store refuses: an invalid provider, a taken name, a key that belongs to another
/// provider.
pub fn apply_imported_provider(
    tx: &Connection,
    incoming: &ImportedProvider<'_>,
    local: Option<&Provider>,
    options: ImportOptions,
) -> Result<ImportOutcome, AppError> {
    let id = incoming.provider.id.as_str();
    let before: HashMap<String, ProviderKey> = store::list_keys(tx, Some(id))?
        .into_iter()
        .map(|key| (key.id.clone(), key))
        .collect();
    let moved = local.is_some_and(|local| {
        let hosts = store::outbound_hosts(local);
        store::outbound_hosts(incoming.provider)
            .iter()
            .any(|host| !hosts.contains(host))
    });
    // Approving the row in the preview approves its host: the store then switches the keys off.
    let options_store = SaveProviderOptions {
        expected_updated_at_ms: None,
        confirm_host_change: true,
        keep_former_name: true,
    };
    let provider = Provider {
        key_strategy: Some(incoming.provider.key_strategy.unwrap_or_default()),
        key_cooldown: Some(incoming.provider.key_cooldown.unwrap_or(true)),
        // A provider the import brings in waits for the user's review (the column is written on
        // insert only, so a known provider keeps its own mark).
        review_pending_since_ms: local.is_none().then(crate::app::current_time_ms),
        ..incoming.provider.clone()
    };
    store::save_provider_with(tx, &provider, options_store, provider.updated_at_ms)?;
    store::stamp_llm_created_at(tx, store::StampTable::Providers, id, provider.created_at_ms)?;

    let mut outcome = ImportOutcome {
        provider_id: id.to_owned(),
        ..ImportOutcome::default()
    };
    // Keys end switched off when the provider moves somewhere the user did not keep them for.
    let off = options.detach_keys || (moved && !options.keys_kept_for_new_host);
    let mut next_position = before.values().map(|key| key.position).max().unwrap_or(-1) + 1;
    for imported in incoming.keys {
        let known = before.get(&imported.key.id);
        let (enabled, position) = match known {
            Some(local) => (local.enabled && !off, imported.key.position),
            None => {
                outcome.new_keys_off += 1;
                next_position += 1;
                (false, next_position - 1)
            }
        };
        let key = ProviderKey {
            enabled,
            position,
            ..imported.key.clone()
        };
        let secret = Some(imported.secret.as_str()).filter(|secret| !secret.trim().is_empty());
        store::save_key(tx, &key, secret, key.updated_at_ms)?;
        store::stamp_llm_created_at(tx, store::StampTable::Keys, &key.id, key.created_at_ms)?;
    }
    for local_key in before.values() {
        if incoming.keys.iter().any(|k| k.key.id == local_key.id) {
            continue;
        }
        if !options.keep_local_keys {
            store::delete_key(tx, &local_key.id)?;
        } else if moved && !off && local_key.enabled {
            // The user kept the keys for the new host: the store's switch-off is undone.
            let restored = ProviderKey {
                enabled: true,
                ..store::get_key(tx, &local_key.id)?.unwrap_or_else(|| local_key.clone())
            };
            store::save_key(tx, &restored, None, provider.updated_at_ms)?;
        }
    }
    if off {
        for key in store::list_keys(tx, Some(id))?
            .into_iter()
            .filter(|k| k.enabled)
        {
            let off = ProviderKey {
                enabled: false,
                ..key
            };
            store::save_key(tx, &off, None, provider.updated_at_ms)?;
        }
    }

    if options.apply_removals {
        for model in store::list_models(tx, Some(id))? {
            if !incoming.models.iter().any(|m| m.model_id == model.model_id) {
                store::delete_model(tx, &model.provider_id, &model.model_id)?;
            }
        }
    }
    for model in incoming.models {
        store::save_model(tx, model)?;
        // A fallback on a provider that arrives later (or never) is skipped at routing time, so
        // the chain is stored as the file has it.
        let primary = ModelRef {
            provider_id: model.provider_id.clone(),
            model: model.model_id.clone(),
        };
        store::set_model_fallbacks(tx, &primary, &model.fallbacks)?;
    }
    Ok(outcome)
}
