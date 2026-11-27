//! The aggregated, zero-secret read behind Settings › AI (`ai_config_get`).
//!
//! Single responsibility: gather what the AI pages list (providers, keys, models, aliases,
//! statuses, gateway clients, MCP servers, skills, tool policies, rules, prices and the last test
//! of each model) in one call, so the UI paints from one snapshot instead of fifteen requests.
//! Every part is read through the use-case that already serves it, so nothing here can show more
//! than that use-case does: keys carry a hint, clients a token tag, MCP servers placeholders.
//! A part whose read fails is reported in `degraded` (its list is empty) instead of failing the
//! whole call.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agent::mcp::AgentMcpServerView;
use crate::agent::model::{AgentSkill, AgentToolPolicy};
use crate::agent::permissions::PermissionRule;
use crate::agent::skills::expand_home;
use crate::error::AppError;
use crate::llm::health::HealthSnapshot;
use crate::llm::status::ProviderStatusDto;
use crate::llm::store::{Alias, GatewayClient, ProbeResult, ProviderKey, ProviderModel};
use crate::llm::usage::PriceOverride;

use crate::agent_service::{
    AgentToolEntry, BlockedRule, agent_blocked_rules_impl, agent_mcp_last_seq,
    agent_mcp_server_list_impl, agent_permission_rule_list_impl, agent_skill_list_impl,
    agent_tool_list_impl, agent_tool_policy_list_impl, tool_entries,
};
use crate::app::AppState;
use crate::app::app_info_service::{SecretStoreState, secret_store_state};
use spanbay_app_llm::{
    ProviderView, llm_gateway_client_list_impl, llm_key_list_impl, llm_model_list_impl,
    llm_price_override_list_impl, llm_probe_results_impl, llm_provider_list_view_impl,
    llm_provider_status_list_impl,
};

/// The outcome of the last test of one model, over all keys of its provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelVerdict {
    /// Every cell that ran succeeded.
    Passed,
    /// At least one cell was refused, unreachable, rate limited or answered nonsense.
    Rejected,
    /// The failures were timeouts only.
    TimedOut,
}

/// The last test verdict of one provider model; a model never tested has no entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTestDto {
    /// The provider.
    pub provider_id: String,
    /// The model id as the provider names it.
    pub model_id: String,
    /// What the latest results say.
    pub verdict: ModelVerdict,
}

/// A skills folder from the settings with what the last scan found there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillFolderDto {
    /// The path as stored (`~` kept).
    pub path: String,
    /// The folder exists, so it was scanned.
    pub scanned: bool,
    /// Skills found under it.
    pub skill_count: u32,
}

/// The parts of [`AiConfigDto`] that are read one by one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiResource {
    /// `providers`.
    Providers,
    /// `keys`.
    Keys,
    /// `models`.
    Models,
    /// `model_tests`.
    ModelTests,
    /// `aliases`.
    Aliases,
    /// `statuses`.
    Statuses,
    /// `clients`.
    Clients,
    /// `mcp_servers`.
    McpServers,
    /// `skills` and `skill_folders`.
    Skills,
    /// `tools`.
    Tools,
    /// `tool_policies`.
    ToolPolicies,
    /// `rules`.
    Rules,
    /// `price_overrides`.
    PriceOverrides,
}

/// Everything Settings › AI lists, in one zero-secret snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiConfigDto {
    /// Providers in display order, with their header-secret hints and compatibility derivations.
    pub providers: Vec<ProviderView>,
    /// Keys of every provider (hint and tag only, never the value).
    pub keys: Vec<ProviderKey>,
    /// Models of every provider.
    pub models: Vec<ProviderModel>,
    /// The last test verdict of each model that was tested.
    pub model_tests: Vec<ModelTestDto>,
    /// Retired with aliases: always empty (the frozen DTO keeps the field).
    pub aliases: Vec<Alias>,
    /// The derived status of each provider.
    pub statuses: Vec<ProviderStatusDto>,
    /// Gateway clients (token prefix and last four characters only).
    pub clients: Vec<GatewayClient>,
    /// MCP servers with their live status and tool count.
    pub mcp_servers: Vec<AgentMcpServerView>,
    /// Skills found in the configured folders.
    pub skills: Vec<AgentSkill>,
    /// The configured skill folders.
    pub skill_folders: Vec<SkillFolderDto>,
    /// Every tool the UI can offer, with its permission class and whether "Always allow" applies
    /// (D-223).
    pub tools: Vec<AgentToolEntry>,
    /// Per-tool policies that differ from the mode default.
    pub tool_policies: Vec<AgentToolPolicy>,
    /// Saved permission rules (global ones).
    pub rules: Vec<PermissionRule>,
    /// The commands the shell tool refuses.
    pub blocked: Vec<BlockedRule>,
    /// Price overrides.
    pub price_overrides: Vec<PriceOverride>,
    /// Whether the secret store can be read and written.
    pub secret_store: SecretStoreState,
    /// Parts whose read failed; their lists above are empty placeholders, not "none".
    pub degraded: Vec<AiResource>,
    /// Sequence number of the LLM event stream this snapshot reflects: subscribe from it.
    pub last_seq: u64,
    /// Sequence number of the MCP status stream: subscribe from it and no status change is lost
    /// (a lost one would arrive as a `Gap`).
    pub mcp_last_seq: u64,
    /// The circuit state of every `(key, model)` the gateway used, `open_until_ms` included: an
    /// alias target reads "Cooling down 8 s" from the entry of its key and model.
    pub health: Vec<HealthSnapshot>,
}

/// The verdict of each (provider, model) from the latest stored results of every key.
///
/// A skipped or unsupported cell says nothing. Of the others, all `ready` is [`Passed`]; a
/// timeout alone is [`TimedOut`]; any other failure is [`Rejected`]. A result of a key that no
/// longer exists is dropped.
///
/// [`Passed`]: ModelVerdict::Passed
/// [`TimedOut`]: ModelVerdict::TimedOut
/// [`Rejected`]: ModelVerdict::Rejected
fn model_tests(results: &[ProbeResult], keys: &[ProviderKey]) -> Vec<ModelTestDto> {
    let provider_of: BTreeMap<&str, &str> = keys
        .iter()
        .map(|key| (key.id.as_str(), key.provider_id.as_str()))
        .collect();
    let mut verdicts: BTreeMap<(&str, &str), ModelVerdict> = BTreeMap::new();
    for result in results {
        let Some(provider_id) = provider_of.get(result.key_id.as_str()) else {
            continue;
        };
        let cell = match result.status_word.as_str() {
            "ready" => ModelVerdict::Passed,
            "timed_out" => ModelVerdict::TimedOut,
            "rejected" | "rate_limited" | "unreachable" | "invalid_response" => {
                ModelVerdict::Rejected
            }
            _ => continue,
        };
        let entry = verdicts
            .entry((provider_id, result.model_id.as_str()))
            .or_insert(ModelVerdict::Passed);
        *entry = match (*entry, cell) {
            (ModelVerdict::Rejected, _) | (_, ModelVerdict::Rejected) => ModelVerdict::Rejected,
            (ModelVerdict::TimedOut, _) | (_, ModelVerdict::TimedOut) => ModelVerdict::TimedOut,
            _ => ModelVerdict::Passed,
        };
    }
    verdicts
        .into_iter()
        .map(|((provider_id, model_id), verdict)| ModelTestDto {
            provider_id: provider_id.to_owned(),
            model_id: model_id.to_owned(),
            verdict,
        })
        .collect()
}

/// The configured skill folders, each with whether it exists and how many skills live under it.
pub(crate) fn skill_folders(dirs: &[String], skills: &[AgentSkill]) -> Vec<SkillFolderDto> {
    dirs.iter()
        .map(|dir| {
            let root = expand_home(dir);
            let skill_count = skills
                .iter()
                .filter(|skill| Path::new(&skill.path).starts_with(&root))
                .count();
            SkillFolderDto {
                path: dir.clone(),
                scanned: root.is_dir(),
                skill_count: u32::try_from(skill_count).unwrap_or(u32::MAX),
            }
        })
        .collect()
}

/// Unwraps one part, noting it in `degraded` and using an empty value when its read failed.
fn part<T: Default>(
    result: Result<T, AppError>,
    which: AiResource,
    degraded: &mut BTreeSet<AiResource>,
) -> T {
    result.unwrap_or_else(|_| {
        degraded.insert(which);
        T::default()
    })
}

/// Everything except the provider statuses (those are computed asynchronously), read on the
/// calling thread: short store reads only.
fn read_stored(state: &AppState, degraded: &mut BTreeSet<AiResource>) -> AiConfigDto {
    let keys = part(llm_key_list_impl(state, None), AiResource::Keys, degraded);
    let results = part(
        llm_probe_results_impl(state, None),
        AiResource::ModelTests,
        degraded,
    );
    let skills = part(
        agent_skill_list_impl(state, None),
        AiResource::Skills,
        degraded,
    );
    let skill_folders = skill_folders(&state.settings().ai.agent.skill_dirs, &skills);
    AiConfigDto {
        providers: part(
            llm_provider_list_view_impl(state),
            AiResource::Providers,
            degraded,
        ),
        models: part(
            llm_model_list_impl(state, None),
            AiResource::Models,
            degraded,
        ),
        model_tests: model_tests(&results, &keys),
        keys,
        aliases: Vec::new(),
        statuses: Vec::new(),
        clients: part(
            llm_gateway_client_list_impl(state),
            AiResource::Clients,
            degraded,
        ),
        mcp_servers: part(
            agent_mcp_server_list_impl(state),
            AiResource::McpServers,
            degraded,
        ),
        skills,
        skill_folders,
        tools: Vec::new(),
        tool_policies: part(
            agent_tool_policy_list_impl(state),
            AiResource::ToolPolicies,
            degraded,
        ),
        rules: part(
            agent_permission_rule_list_impl(state, None, None),
            AiResource::Rules,
            degraded,
        ),
        blocked: agent_blocked_rules_impl(state),
        price_overrides: part(
            llm_price_override_list_impl(state),
            AiResource::PriceOverrides,
            degraded,
        ),
        secret_store: secret_store_state(state),
        degraded: Vec::new(),
        last_seq: 0,
        mcp_last_seq: 0,
        health: state.llm.health.snapshot(),
    }
}

/// The aggregated, zero-secret snapshot of the AI configuration (Settings › AI).
///
/// `last_seq` is read before any part, so an event emitted while the parts are gathered is
/// replayed to a subscriber that starts from it and is never lost.
///
/// # Errors
/// [`AppError::Internal`] when the reading thread dies; a failing part is reported in
/// `degraded` instead.
pub async fn ai_config_get_impl(state: &AppState) -> Result<AiConfigDto, AppError> {
    let last_seq = state.llm.events().last_seq();
    let mcp_last_seq = agent_mcp_last_seq(state);
    let stored_state = state.clone();
    let (mut config, mut degraded) = tokio::task::spawn_blocking(move || {
        let mut degraded = BTreeSet::new();
        let config = read_stored(&stored_state, &mut degraded);
        (config, degraded)
    })
    .await
    .map_err(|_| AppError::internal("reading the AI configuration was interrupted"))?;
    config.statuses = part(
        llm_provider_status_list_impl(state).await,
        AiResource::Statuses,
        &mut degraded,
    );
    // The class and the Allow rule only; the "new since last read" baseline belongs to the Tools
    // page, whose own read marks it.
    config.tools = part(
        agent_tool_list_impl(state, None)
            .await
            .map(|tools| tool_entries(tools, &HashSet::new())),
        AiResource::Tools,
        &mut degraded,
    );
    config.degraded = degraded.into_iter().collect();
    config.last_seq = last_seq;
    config.mcp_last_seq = mcp_last_seq;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(key_id: &str, model_id: &str, status_word: &str) -> ProbeResult {
        ProbeResult {
            key_id: key_id.to_owned(),
            model_id: model_id.to_owned(),
            status_word: status_word.to_owned(),
            ..ProbeResult::default()
        }
    }

    fn key(id: &str, provider_id: &str) -> ProviderKey {
        ProviderKey {
            id: id.to_owned(),
            provider_id: provider_id.to_owned(),
            ..ProviderKey::default()
        }
    }

    #[test]
    fn a_models_verdict_folds_its_cells_and_ignores_the_silent_ones() {
        let keys = [key("k1", "p1"), key("k2", "p1"), key("k3", "p2")];
        let results = [
            result("k1", "m-ok", "ready"),
            result("k2", "m-ok", "ready"),
            result("k1", "m-slow", "ready"),
            result("k2", "m-slow", "timed_out"),
            result("k1", "m-bad", "timed_out"),
            result("k2", "m-bad", "rejected"),
            result("k1", "m-silent", "skipped"),
            result("k1", "m-silent", "unsupported"),
            result("k3", "m-ok", "rejected"),
            result("gone", "m-ghost", "ready"),
        ];
        let verdicts: Vec<_> = model_tests(&results, &keys)
            .into_iter()
            .map(|test| (test.provider_id, test.model_id, test.verdict))
            .collect();
        let expect = |p: &str, m: &str, v| (p.to_owned(), m.to_owned(), v);
        assert_eq!(
            verdicts,
            vec![
                expect("p1", "m-bad", ModelVerdict::Rejected),
                expect("p1", "m-ok", ModelVerdict::Passed),
                expect("p1", "m-slow", ModelVerdict::TimedOut),
                expect("p2", "m-ok", ModelVerdict::Rejected),
            ]
        );
    }

    #[test]
    fn skill_folders_count_their_skills_and_report_missing_ones() {
        let dir = std::env::temp_dir().join(format!("pd-skills-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let skill = |name: &str, path: String| AgentSkill {
            name: name.to_owned(),
            description: String::new(),
            path,
            origin: String::new(),
            enabled: true,
        };
        let skills = [
            skill("a", dir.join("a").to_string_lossy().into_owned()),
            skill("b", dir.join("b").to_string_lossy().into_owned()),
            skill("c", "/elsewhere/c".to_owned()),
        ];
        let listed = skill_folders(
            &[
                dir.to_string_lossy().into_owned(),
                "/no/such/folder/for/spanbay".to_owned(),
            ],
            &skills,
        );
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            (listed[0].scanned, listed[0].skill_count),
            (true, 2),
            "{listed:?}"
        );
        assert_eq!((listed[1].scanned, listed[1].skill_count), (false, 0));
    }
}
