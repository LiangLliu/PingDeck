//! Global application settings: typed, versioned, sectioned, with an `Arc` snapshot for readers.
//!
//! Single responsibility: the settings model, merge-patching with validation, the change event
//! and the in-memory hub core code reads through. Rows are stored by
//! [`crate::workspace::storage`]; the use-cases that tie both together live in
//! `app::settings_service`.
//!
//! Core consumers call `AppState::settings()` and keep the returned `Arc<Settings>` for the
//! duration of one operation: reading it is a read-lock plus an `Arc` clone, never a database
//! read, and an update swaps in a new `Arc` without disturbing operations already running.

mod legacy;
pub mod limits;
pub mod policy;
pub mod registry;
mod sections;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::events::EventSink;
use crate::stored_setting::StoredSetting;
use spanbay_base::error::AppError;

pub use legacy::{LegacyAi, ai_from_v1_sections};
use sections::Validate;
pub use sections::{
    AdvancedSettings, AgentFallbacks, AiAgentSettings, AiGatewaySettings, AiProvidersSettings,
    AiSearchSettings, AiSettings, AppearanceSettings, DataSettings, Density, EditorSettings,
    GeneralSettings, Language, MockSettings, NetworkSettings, NotifyWhenAway, PromptCacheMode,
    ReasoningEffortOrAuto, ReduceMotion, RoleModels, SearchBackend, ShortcutsSettings, SidebarMode,
    SyncSettings, Theme, ToolApproval,
};

/// Schema version written with every settings row. Bump it, and add a step to
/// [`migrate_section`], when a section changes shape incompatibly.
pub const SETTINGS_VERSION: u32 = 2;

/// Brings a stored section of `version` up to [`SETTINGS_VERSION`]. Schema 2 only regrouped the
/// AI settings into the `ai` section, which database migration v22 did on the rows
/// (see [`ai_from_v1_sections`]); every other section is unchanged, so nothing is left to
/// translate here.
fn migrate_section(_section: SettingsSection, _version: u32, value: Value) -> Value {
    value
}

/// A registered settings subscriber and its id.
type Subscriber = (u64, Arc<dyn EventSink<SettingsEvent>>);

/// Subscribers a [`SettingsHub`] serves at once.
const MAX_SUBSCRIBERS: usize = 16;

macro_rules! settings_sections {
    ($($variant:ident => $field:ident: $ty:ty),+ $(,)?) => {
        /// One independently stored, validated and reset block of settings.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum SettingsSection {
            $(#[doc = concat!("The `", stringify!($field), "` section.")] $variant),+
        }

        impl SettingsSection {
            /// Every section in display order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The stable name used as the storage key and in field paths.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => stringify!($field)),+
                }
            }
        }

        /// Every global setting, grouped by section. Sections missing from storage (or from an
        /// imported file) read as their defaults.
        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        #[serde(default)]
        pub struct Settings {
            $(#[doc = concat!("The `", stringify!($field), "` section.")] pub $field: $ty),+
        }

        impl Settings {
            /// One section as a JSON object.
            #[must_use]
            pub fn section_value(&self, section: SettingsSection) -> Value {
                match section {
                    $(SettingsSection::$variant => to_value(&self.$field)),+
                }
            }

            /// A copy with `section` replaced by `value`, validated like any update. Fields
            /// missing from `value` take their defaults.
            pub fn with_section_value(
                &self,
                section: SettingsSection,
                value: &Value,
            ) -> Result<Self, AppError> {
                let mut next = self.clone();
                match section {
                    $(SettingsSection::$variant => {
                        next.$field = parse_section(section, &Value::Null, value)?;
                    }),+
                }
                next.check_read_only(self)?;
                next.check_cross(section)?;
                Ok(next)
            }

            /// A copy with `section` merge-patched by `patch` and validated.
            pub fn patched(&self, section: SettingsSection, patch: &Value) -> Result<Self, AppError> {
                let mut next = self.clone();
                match section {
                    $(SettingsSection::$variant => {
                        next.$field = parse_section(section, &self.section_value(section), patch)?;
                    }),+
                }
                next.check_read_only(self)?;
                next.check_cross(section)?;
                Ok(next)
            }

            /// A copy with `section` back at its defaults. The read-only hints of the `ai`
            /// section stay: they describe stored secrets, which a reset does not touch.
            ///
            /// # Errors
            /// A validation error when the defaults clash with another section (the gateway
            /// port against the first mock port).
            pub fn reset(&self, section: SettingsSection) -> Result<Self, AppError> {
                let mut next = self.clone();
                match section {
                    $(SettingsSection::$variant => next.$field = Default::default()),+
                }
                next.ai.keep_hints_of(&self.ai);
                next.check_cross(section)?;
                Ok(next)
            }

            /// A copy with only the dotted `keys` of `section` (relative to it, for example
            /// `gateway.port`; a key naming an object resets everything inside) back at their
            /// defaults. Other fields keep their values.
            ///
            /// # Errors
            /// A validation error for an empty list, a key the section does not have, a
            /// read-only hint, or defaults that clash with another section.
            pub fn reset_keys(&self, section: SettingsSection, keys: &[String]) -> Result<Self, AppError> {
                if keys.is_empty() {
                    return Err(AppError::validation("keys must not be empty"));
                }
                let defaults = Self::default().section_value(section);
                let mut value = self.section_value(section);
                for key in keys {
                    let default = defaults
                        .pointer(&format!("/{}", key.replace('.', "/")))
                        .ok_or_else(|| {
                            AppError::validation(format!("unknown setting {}.{key}", section.as_str()))
                        })?;
                    put_path(&mut value, key, default);
                }
                self.with_section_value(section, &value)
            }

            /// Builds settings from stored rows. A row that is unreadable, newer than this
            /// build, or no longer valid yields that section's defaults instead of an error, so
            /// a bad row can never keep the app from starting.
            #[must_use]
            pub fn from_rows(rows: &[StoredSetting]) -> Self {
                let mut settings = Self::default();
                for row in rows.iter().filter(|row| row.version <= SETTINGS_VERSION) {
                    $(if row.section == stringify!($field) {
                        let value = serde_json::from_str(&row.value_json)
                            .map(|value| migrate_section(SettingsSection::$variant, row.version, value));
                        if let Ok(mut section) = value.and_then(serde_json::from_value::<$ty>) {
                            section.normalize_stored();
                            if section.check().is_ok() {
                                settings.$field = section;
                            }
                        }
                    })+
                }
                settings.seed_appearance(rows);
                settings
            }
        }
    };
}

impl Settings {
    /// Schema-2 stored the theme and font sizes in `general`; a database that has no
    /// `appearance` row yet takes them from there, so the upgrade keeps the user's look.
    fn seed_appearance(&mut self, rows: &[StoredSetting]) {
        if rows.iter().any(|row| row.section == "appearance") {
            return;
        }
        let Some(Value::Object(general)) = rows
            .iter()
            .find(|row| row.section == "general" && row.version <= SETTINGS_VERSION)
            .and_then(|row| serde_json::from_str(&row.value_json).ok())
        else {
            return;
        };
        let moved: serde_json::Map<String, Value> = ["theme", "ui_font_size", "mono_font_size"]
            .into_iter()
            .filter_map(|key| Some((key.to_owned(), general.get(key)?.clone())))
            .collect();
        if let Ok(appearance) = serde_json::from_value::<AppearanceSettings>(Value::Object(moved))
            && appearance.check().is_ok()
        {
            self.appearance = appearance;
        }
    }
}

impl Settings {
    /// Rules spanning sections, which each section's own validation cannot see; only checked
    /// when `section` is one of the sections involved so an unrelated update never trips on them.
    fn check_cross(&self, section: SettingsSection) -> Result<(), AppError> {
        if matches!(section, SettingsSection::Ai | SettingsSection::Mock)
            && self.ai.gateway.port == self.mock.first_port
        {
            return Err(AppError::validation(
                "ai.gateway.port must differ from mock.first_port",
            ));
        }
        Ok(())
    }

    /// Rejects a change to a field that only the secret use-cases may write (the masked hints),
    /// naming it. `before` is the state the change started from.
    fn check_read_only(&self, before: &Settings) -> Result<(), AppError> {
        for ((name, now), (_, was)) in self
            .ai
            .read_only_hints()
            .into_iter()
            .zip(before.ai.read_only_hints())
        {
            if now != was {
                return Err(AppError::validation(format!("{name} is read-only")));
            }
        }
        Ok(())
    }
}

settings_sections! {
    General => general: GeneralSettings,
    Appearance => appearance: AppearanceSettings,
    Editor => editor: EditorSettings,
    Network => network: NetworkSettings,
    Data => data: DataSettings,
    Mock => mock: MockSettings,
    Ai => ai: AiSettings,
    Shortcuts => shortcuts: ShortcutsSettings,
    Sync => sync: SyncSettings,
    Advanced => advanced: AdvancedSettings,
}

fn to_value<T: Serialize>(section: &T) -> Value {
    // Section types hold plain data, so serialization cannot fail; null keeps the signature total.
    serde_json::to_value(section).unwrap_or(Value::Null)
}

/// Sets the dotted `path` inside the JSON object `target` to `own`, creating intermediate objects.
pub fn put_path(target: &mut Value, path: &str, own: &Value) {
    let Value::Object(fields) = target else {
        return;
    };
    match path.split_once('.') {
        None => {
            fields.insert(path.to_owned(), own.clone());
        }
        Some((head, rest)) => put_path(
            fields
                .entry(head.to_owned())
                .or_insert_with(|| Value::Object(serde_json::Map::new())),
            rest,
            own,
        ),
    }
}

/// Removes the dotted `path` from the JSON object `target` (nothing when it is not there).
pub fn remove_path(target: &mut Value, path: &str) {
    match path.split_once('.') {
        None => {
            if let Value::Object(fields) = target {
                fields.remove(path);
            }
        }
        Some((head, rest)) => {
            if let Some(inner) = target.get_mut(head) {
                remove_path(inner, rest);
            }
        }
    }
}

/// RFC 7396 merge-patch: objects merge recursively, `null` removes a key, anything else replaces.
fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(patch) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(serde_json::Map::new());
    }
    if let Value::Object(target) = target {
        for (key, value) in patch {
            if value.is_null() {
                target.remove(key);
            } else {
                merge_patch(target.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
    }
}

/// The first dotted key of `patch` that `defaults` has no counterpart for. Nested objects are
/// compared too, except empty default objects, which stand for free-form maps.
fn unknown_key(defaults: &Value, patch: &serde_json::Map<String, Value>) -> Option<String> {
    patch.iter().find_map(|(key, value)| {
        let Some(known) = defaults.get(key.as_str()) else {
            return Some(key.clone());
        };
        match (known, value) {
            (Value::Object(map), Value::Object(inner)) if !map.is_empty() => {
                unknown_key(known, inner).map(|nested| format!("{key}.{nested}"))
            }
            _ => None,
        }
    })
}

/// The dotted path of the first patched field that makes `T` fail to deserialize when applied
/// alone to the defaults, descending into nested objects.
fn wrong_type_key<T: serde::de::DeserializeOwned>(
    defaults: &Value,
    path: &mut Vec<String>,
    patch: &serde_json::Map<String, Value>,
) -> Option<String> {
    for (key, value) in patch {
        path.push(key.clone());
        let mut probe = defaults.clone();
        let nested = path.iter().rev().fold(
            value.clone(),
            |inner, key| serde_json::json!({ key: inner }),
        );
        merge_patch(&mut probe, &nested);
        let found = if serde_json::from_value::<T>(probe).is_err() {
            match value {
                Value::Object(inner) => wrong_type_key::<T>(defaults, path, inner),
                _ => None,
            }
            .or_else(|| Some(path.join(".")))
        } else {
            None
        };
        path.pop();
        if found.is_some() {
            return found;
        }
    }
    None
}

/// Merges `patch` into `current`, deserializes the result as `T` and validates it. A type error
/// is attributed to the first patched field that causes it, so the message names the field.
fn parse_section<T>(section: SettingsSection, current: &Value, patch: &Value) -> Result<T, AppError>
where
    T: Default + Serialize + serde::de::DeserializeOwned + Validate,
{
    let name = section.as_str();
    let Value::Object(patch_fields) = patch else {
        return Err(AppError::validation(format!(
            "{name} settings must be a JSON object"
        )));
    };
    let mut defaults = to_value(&T::default());
    T::extend_patch_schema(&mut defaults);
    if let Some(unknown) = unknown_key(&defaults, patch_fields) {
        return Err(AppError::validation(format!(
            "unknown setting {name}.{unknown}"
        )));
    }
    let mut merged = current.clone();
    merge_patch(&mut merged, patch);
    let parsed: T = serde_json::from_value(merged).map_err(|error| {
        AppError::validation(
            match wrong_type_key::<T>(&defaults, &mut Vec::new(), patch_fields) {
                Some(key) => {
                    format!("{name}.{key} has the wrong type or an unknown value: {error}")
                }
                None => format!("invalid {name} settings: {error}"),
            },
        )
    })?;
    parsed.check()?;
    Ok(parsed)
}

/// A change pushed to settings subscribers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SettingsEvent {
    /// `section` changed; `settings` is the complete new state.
    Changed {
        /// The section that was updated or reset.
        section: SettingsSection,
        /// All settings after the change.
        settings: Box<Settings>,
    },
}

/// The live settings: a swap-on-update `Arc` snapshot plus the change subscribers.
pub struct SettingsHub {
    current: RwLock<Arc<Settings>>,
    /// Serializes read-modify-write updates so two patches never overwrite each other.
    update_lock: Mutex<()>,
    subscribers: Mutex<Vec<Subscriber>>,
    next_subscription: AtomicU64,
}

impl SettingsHub {
    pub fn new(settings: Settings) -> Self {
        Self {
            current: RwLock::new(Arc::new(settings)),
            update_lock: Mutex::new(()),
            subscribers: Mutex::new(Vec::new()),
            next_subscription: AtomicU64::new(1),
        }
    }

    /// The current settings. Cheap: a read lock and an `Arc` clone.
    #[must_use]
    pub fn snapshot(&self) -> Arc<Settings> {
        Arc::clone(&self.current.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Holds off other updates until dropped.
    pub fn lock_updates(&self) -> MutexGuard<'_, ()> {
        self.update_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Swaps in `settings` and tells subscribers; subscribers whose consumer is gone are dropped.
    pub fn publish(&self, section: SettingsSection, settings: Settings) -> Arc<Settings> {
        let next = Arc::new(settings);
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = Arc::clone(&next);
        let mut subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        subscribers.retain(|(_, sink)| {
            sink.emit(SettingsEvent::Changed {
                section,
                settings: Box::new((*next).clone()),
            })
        });
        next
    }

    /// Registers `sink` for every later change; returns the id for [`Self::unsubscribe`].
    pub fn subscribe(&self, sink: Arc<dyn EventSink<SettingsEvent>>) -> Result<u64, AppError> {
        let mut subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if subscribers.len() >= MAX_SUBSCRIBERS {
            return Err(AppError::conflict("too many settings subscribers"));
        }
        let id = self.next_subscription.fetch_add(1, Ordering::Relaxed);
        subscribers.push((id, sink));
        Ok(id)
    }

    /// Ends a subscription; returns whether it was still active.
    pub fn unsubscribe(&self, id: u64) -> bool {
        let mut subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let before = subscribers.len();
        subscribers.retain(|(candidate, _)| *candidate != id);
        subscribers.len() != before
    }
}

#[cfg(test)]
mod tests;
