//! One `app_settings` row as the store hands it to [`crate::settings`].

/// One stored settings section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSetting {
    /// Section name (`network`, `agent`, ...).
    pub section: String,
    /// Schema version of `value_json`.
    pub version: u32,
    /// The section as JSON.
    pub value_json: String,
}
