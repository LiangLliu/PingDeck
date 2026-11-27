//! Per-check timeouts derived from the `ai.providers.probe_timeout_s` setting.
//!
//! Single responsibility: turn the one setting into the budget of every check.

use std::time::Duration;

use super::ProbeTest;

/// The default of `ai.providers.probe_timeout_s`.
pub(crate) const DEFAULT_SETTING_S: u32 = 30;
/// Smallest accepted `probe_timeout_s`.
const MIN_SETTING_S: u32 = 5;
/// Largest accepted `probe_timeout_s`.
const MAX_SETTING_S: u32 = 300;
/// Longest the connectivity check waits whatever the setting is.
const CONNECTIVITY_CAP_S: u64 = 10;
/// One context-ladder step (huge prompts are slow to ingest).
const CONTEXT_STEP_S: u64 = 300;

/// How long each check may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckTimeouts {
    /// The base setting in seconds (clamped to 5..=300).
    pub setting_s: u32,
    /// Connectivity: `min(setting, 10)` s.
    pub connectivity: Duration,
    /// Auth: the setting.
    pub auth: Duration,
    /// Models: the setting.
    pub models: Duration,
    /// Chat, JSON schema, Vision, Search and PDF: the setting for the whole response.
    pub chat: Duration,
    /// Stream: the setting for the first content event.
    pub stream_first: Duration,
    /// Stream: the longest silence between events (the setting).
    pub stream_idle: Duration,
    /// Stream: ceiling for the whole call (4 x the setting).
    pub stream_ceiling: Duration,
    /// Tools: the setting per request (the check sends two).
    pub tools_request: Duration,
    /// Reasoning: 3 x the setting for the whole response.
    pub reasoning: Duration,
    /// Cache: the setting per request (the check sends two).
    pub cache_request: Duration,
    /// Embeddings: the setting per request.
    pub embeddings_request: Duration,
    /// One context step: 300 s.
    pub context_step: Duration,
}

impl Default for CheckTimeouts {
    /// The setting's default of 30 s.
    fn default() -> Self {
        Self::from_setting(DEFAULT_SETTING_S)
    }
}

impl CheckTimeouts {
    /// The timeouts for a `probe_timeout_s` value; values outside 5..=300 are clamped.
    #[must_use]
    pub fn from_setting(seconds: u32) -> Self {
        let setting_s = seconds.clamp(MIN_SETTING_S, MAX_SETTING_S);
        let base = u64::from(setting_s);
        let secs = Duration::from_secs;
        Self {
            setting_s,
            connectivity: secs(base.min(CONNECTIVITY_CAP_S)),
            auth: secs(base),
            models: secs(base),
            chat: secs(base),
            stream_first: secs(base),
            stream_idle: secs(base),
            stream_ceiling: secs(base * 4),
            tools_request: secs(base),
            reasoning: secs(base * 3),
            cache_request: secs(base),
            embeddings_request: secs(base),
            context_step: secs(CONTEXT_STEP_S),
        }
    }

    /// The timeout whose expiry a "Timed out N s" message names: the budget of the first
    /// response (stream: the first content event; tools and cache: one request).
    #[must_use]
    pub const fn headline(&self, test: ProbeTest) -> Duration {
        match test {
            ProbeTest::Connectivity => self.connectivity,
            ProbeTest::Auth => self.auth,
            ProbeTest::Models => self.models,
            ProbeTest::Stream => self.stream_first,
            ProbeTest::Tools => self.tools_request,
            ProbeTest::Reasoning => self.reasoning,
            ProbeTest::Cache => self.cache_request,
            ProbeTest::Embeddings => self.embeddings_request,
            ProbeTest::Context => self.context_step,
            ProbeTest::Chat
            | ProbeTest::JsonSchema
            | ProbeTest::Vision
            | ProbeTest::Search
            | ProbeTest::Pdf
            | ProbeTest::Decision => self.chat,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_for_the_default_setting() {
        let t = CheckTimeouts::default();
        assert_eq!(t.setting_s, 30);
        assert_eq!(t.connectivity, Duration::from_secs(10));
        assert_eq!(t.auth, Duration::from_secs(30));
        assert_eq!(t.stream_first, Duration::from_secs(30));
        assert_eq!(t.stream_idle, Duration::from_secs(30));
        assert_eq!(t.stream_ceiling, Duration::from_secs(120));
        assert_eq!(t.reasoning, Duration::from_secs(90));
        assert_eq!(t.context_step, Duration::from_secs(300));
    }

    #[test]
    fn connectivity_follows_a_small_setting_and_the_rest_scales() {
        let t = CheckTimeouts::from_setting(5);
        assert_eq!(t.connectivity, Duration::from_secs(5));
        let t = CheckTimeouts::from_setting(60);
        assert_eq!(t.connectivity, Duration::from_secs(10));
        assert_eq!(t.chat, Duration::from_secs(60));
        assert_eq!(t.reasoning, Duration::from_secs(180));
        assert_eq!(t.headline(ProbeTest::Stream), Duration::from_secs(60));
    }

    #[test]
    fn the_default_follows_the_setting_default() {
        assert_eq!(
            CheckTimeouts::default(),
            CheckTimeouts::from_setting(
                spanbay_model::settings::AiProvidersSettings::default().probe_timeout_s
            )
        );
    }

    #[test]
    fn the_setting_is_clamped_to_its_range() {
        assert_eq!(CheckTimeouts::from_setting(1).setting_s, 5);
        assert_eq!(CheckTimeouts::from_setting(9_999).setting_s, 300);
    }
}
