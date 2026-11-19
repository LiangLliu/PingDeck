//! "Apply results" and its undo as pure logic.
//!
//! Single responsibility: from the decisive check of every tested model (Chat, or Embeddings for
//! an embedding model) and the stored models, compute which models to enable, disable or create,
//! which measured capability facts to record (the only way a probe result reaches the matrix
//! routing filters on), what the preview dialog shows, and how an undo restores the previous
//! flags and states. The store applies the plan.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::ProbeStatus;
use super::capability::CapabilityUpdate;
use crate::llm::capabilities::{Capability, CapabilityState, Measurement, ModelCapabilities};

/// How long "Undo last apply" stays available.
pub const UNDO_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;

/// A run older than this is not applied: its results may no longer hold.
const APPLY_MAX_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Whether a run that started at `started_at_ms` may still be applied at `now_ms`.
#[must_use]
pub const fn run_applicable(started_at_ms: i64, now_ms: i64) -> bool {
    now_ms.saturating_sub(started_at_ms) <= APPLY_MAX_AGE_MS
}

/// The decisive check of one tested model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyRow {
    /// The model tested (stored or not).
    pub model_id: String,
    /// Status of the model's Chat check, or of its Embeddings check for an embedding model;
    /// `None` when the check did not run (Not tested).
    pub status: Option<ProbeStatus>,
    /// HTTP status of that check, for the reason text.
    pub http_status: Option<u16>,
}

/// A stored model as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentModel {
    /// Model id.
    pub model_id: String,
    /// Whether it is enabled.
    pub enabled: bool,
}

/// The models whose fallback chain names a model (the note "Fallback of ...").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackUse {
    /// Model id.
    pub model_id: String,
    /// The primary models, in name form.
    pub primaries: Vec<String>,
}

/// What a change does to the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyAction {
    /// Set `enabled` on a stored, disabled model.
    Enable,
    /// Clear `enabled` on a stored, enabled model.
    Disable,
    /// Store a model that was only listed (`source = live`) and enable it.
    Create,
}

/// One row of the preview dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyChange {
    /// Model id.
    pub model_id: String,
    /// What happens to it.
    pub action: ApplyAction,
    /// The `enabled` value before; `None` for a model that is not stored yet.
    pub previous: Option<bool>,
    /// The decisive status.
    pub status: ProbeStatus,
    /// The reason shown next to the row: the status word and the HTTP status ("Rejected 403").
    pub reason: String,
    /// Whether the row is checked by default: every row except a Disable.
    pub default_checked: bool,
    /// "Used by alias claude-code" for an alias target; the target stays in the alias.
    pub alias_note: Option<String>,
}

/// One capability fact the run measured that differs from the stored matrix ("glm-5 does not
/// support vision"), with the evidence behind it. A change to "supported" is checked by default;
/// a change to "not supported" is not (routing then skips the model for requests that need the
/// capability, so the user confirms it). The apply writes `applied` only while the stored state
/// still equals `previous`, and the undo restores `previous` only while it still equals `applied`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityChange {
    /// Model id (a stored model).
    pub model_id: String,
    /// The capability.
    pub capability: Capability,
    /// The stored state before.
    pub previous: CapabilityState,
    /// The measured state the apply writes.
    pub applied: CapabilityState,
    /// Whether the row is checked as the preview opens: only a change to "supported".
    #[serde(default)]
    pub default_checked: bool,
    /// The keys whose results gave the applied verdict.
    #[serde(default)]
    pub key_ids: Vec<String>,
    /// HTTP status of the newest result behind the verdict.
    #[serde(default)]
    pub http_status: Option<u16>,
    /// The routes the change touches: "Fallback of openai/gpt-5" when a fallback chain names the
    /// model.
    #[serde(default)]
    pub used_by: Option<String>,
}

/// The preview of an apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyPlan {
    /// Passing models that are stored and disabled.
    pub enable: Vec<ApplyChange>,
    /// Failing models that are stored and enabled.
    pub disable: Vec<ApplyChange>,
    /// Passing models that are not stored yet.
    pub create: Vec<ApplyChange>,
    /// Measured capability facts that differ from the stored matrix (a change to "not
    /// supported" unchecked by default).
    #[serde(default)]
    pub capabilities: Vec<CapabilityChange>,
    /// The key itself failed (Connectivity or Auth): nothing changes and the button reads
    /// "Nothing to apply, the key failed".
    pub host_level_failure: bool,
}

impl ApplyPlan {
    /// Whether applying would change anything (the header button is primary only then).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.enable.is_empty()
            && self.disable.is_empty()
            && self.create.is_empty()
            && self.capabilities.is_empty()
    }

    /// The report of applying the rows the user left checked. Ids that are not in the plan are
    /// ignored.
    #[must_use]
    pub fn report(&self, selection: &ApplySelection) -> ApplyReport {
        fn picked<'a>(changes: &'a [ApplyChange], chosen: &[String]) -> Vec<&'a ApplyChange> {
            changes
                .iter()
                .filter(|change| chosen.contains(&change.model_id))
                .collect()
        }
        let enable = picked(&self.enable, &selection.enable);
        let disable = picked(&self.disable, &selection.disable);
        let create = picked(&self.create, &selection.create);
        let previous = enable
            .iter()
            .chain(&disable)
            .chain(&create)
            .map(|change| PreviousState {
                model_id: change.model_id.clone(),
                previous_enabled: change.previous.unwrap_or(false),
                applied_enabled: change.action != ApplyAction::Disable,
            })
            .collect();
        let ids = |changes: &[&ApplyChange]| changes.iter().map(|c| c.model_id.clone()).collect();
        let capabilities = self
            .capabilities
            .iter()
            .filter(|change| {
                selection.capabilities.iter().any(|pick| {
                    pick.model_id == change.model_id && pick.capability == change.capability
                })
            })
            .cloned()
            .collect();
        ApplyReport {
            enabled: ids(&enable),
            disabled: ids(&disable),
            created: ids(&create),
            previous,
            capabilities,
        }
    }

    /// The selection as the preview dialog opens it: every row whose `default_checked` is set.
    #[must_use]
    pub fn default_selection(&self) -> ApplySelection {
        let checked = |changes: &[ApplyChange]| {
            changes
                .iter()
                .filter(|change| change.default_checked)
                .map(|change| change.model_id.clone())
                .collect()
        };
        ApplySelection {
            enable: checked(&self.enable),
            disable: checked(&self.disable),
            create: checked(&self.create),
            capabilities: self
                .capabilities
                .iter()
                .filter(|change| change.default_checked)
                .map(|change| CapabilityPick {
                    model_id: change.model_id.clone(),
                    capability: change.capability,
                })
                .collect(),
        }
    }
}

/// The rows the user left checked in the preview dialog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplySelection {
    /// Models to enable.
    pub enable: Vec<String>,
    /// Models to disable.
    pub disable: Vec<String>,
    /// Models to create.
    pub create: Vec<String>,
    /// Capability facts to record.
    #[serde(default)]
    pub capabilities: Vec<CapabilityPick>,
}

/// One checked capability row of the preview dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityPick {
    /// Model id.
    pub model_id: String,
    /// The capability.
    pub capability: Capability,
}

/// The flag of one changed model before and after an apply (a row of `llm_probe_apply_log`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviousState {
    /// Model id.
    pub model_id: String,
    /// The `enabled` value before the apply (a created model counts as disabled).
    pub previous_enabled: bool,
    /// The `enabled` value the apply wrote.
    pub applied_enabled: bool,
}

/// What an apply did; the previous values are what the undo restores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyReport {
    /// Models enabled.
    pub enabled: Vec<String>,
    /// Models disabled.
    pub disabled: Vec<String>,
    /// Models created (and enabled).
    pub created: Vec<String>,
    /// The flag of every changed model before and after.
    pub previous: Vec<PreviousState>,
    /// The capability states written, with what they replaced (logs from older versions have none).
    #[serde(default)]
    pub capabilities: Vec<CapabilityChange>,
}

/// A failure that may recover: never suggested for disabling.
fn is_transient(status: ProbeStatus) -> bool {
    status == ProbeStatus::TimedOut
}

/// Whether the decisive check was a refused request (HTTP 400 or 422): often a
/// parameter the model does not take, so the model is neither passing nor failing. Unlike
/// [`super::status::is_request_refused`], this looks only at the computed status and the HTTP
/// code of the row.
fn refused_request_row(row: &ApplyRow, status: ProbeStatus) -> bool {
    status == ProbeStatus::InvalidResponse && matches!(row.http_status, Some(400 | 422))
}

fn reason(row: &ApplyRow, status: ProbeStatus) -> String {
    match row.http_status {
        Some(code) if status != ProbeStatus::Ready => format!("{} {code}", status.label()),
        _ => status.label().to_owned(),
    }
}

/// An index by id in which the first entry of an id wins, as a linear `find` would.
fn first_by_id<'a, T: 'a>(
    entries: impl IntoIterator<Item = &'a T>,
    id: impl Fn(&'a T) -> &'a String,
) -> HashMap<&'a str, &'a T> {
    let mut index = HashMap::new();
    for entry in entries {
        index.entry(id(entry).as_str()).or_insert(entry);
    }
    index
}

/// Plans an apply: a model whose decisive check is Ready is enabled (created as
/// `source = live` when not stored); one whose check is Rejected, Unreachable, Invalid response
/// or Unsupported is offered for disabling, unchecked by default (a probe failure alone does not
/// take a model out of routing); Skipped, Rate limited, Not observed, Timed out and Not tested
/// rows are left alone, and so is a model whose decisive check was a refused request (HTTP 400
/// or 422: often a parameter the model does not accept). `host_failed` says the key's Connectivity or Auth
/// failed: nothing changes then. Capability facts are planned by [`plan_capabilities`].
#[must_use]
pub fn plan_apply(
    rows: &[ApplyRow],
    current: &[CurrentModel],
    used_by: &[FallbackUse],
    host_failed: bool,
) -> ApplyPlan {
    let mut plan = ApplyPlan {
        enable: Vec::new(),
        disable: Vec::new(),
        create: Vec::new(),
        capabilities: Vec::new(),
        host_level_failure: host_failed,
    };
    if host_failed {
        return plan;
    }
    let stored_by_id = first_by_id(current, |model| &model.model_id);
    let fallback_of = first_by_id(
        used_by.iter().filter(|used| !used.primaries.is_empty()),
        |used| &used.model_id,
    );
    for row in rows {
        let Some(status) = row.status else { continue };
        let stored = stored_by_id.get(row.model_id.as_str()).copied();
        let (action, target_list) = match (status, stored) {
            // Neither a pass nor a failure: Skipped, Rate limited, Not observed, Timed out, a
            // refused request.
            (ProbeStatus::Skipped | ProbeStatus::RateLimited | ProbeStatus::NotObserved, _) => {
                continue;
            }
            (status, _) if is_transient(status) => continue,
            (status, _) if refused_request_row(row, status) => continue,
            (ProbeStatus::Ready, Some(model)) if !model.enabled => {
                (ApplyAction::Enable, &mut plan.enable)
            }
            (ProbeStatus::Ready, None) => (ApplyAction::Create, &mut plan.create),
            (ProbeStatus::Ready, Some(_)) | (_, None) => continue,
            (_, Some(model)) if model.enabled => (ApplyAction::Disable, &mut plan.disable),
            (_, Some(_)) => continue,
        };
        let alias_note = fallback_of
            .get(row.model_id.as_str())
            .map(|used| format!("Fallback of {}", used.primaries.join(", ")));
        target_list.push(ApplyChange {
            model_id: row.model_id.clone(),
            action,
            previous: stored.map(|model| model.enabled),
            status,
            reason: reason(row, status),
            default_checked: action != ApplyAction::Disable,
            alias_note,
        });
    }
    plan
}

/// One capability a run measured on one model (`derive_capability` of a result), with which key,
/// when and the HTTP status of the check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Measured<'a> {
    /// Model id.
    pub model_id: &'a str,
    /// The key the check ran with.
    pub key_id: &'a str,
    /// What the check measured.
    pub update: CapabilityUpdate,
    /// When the check ran, Unix ms.
    pub at_ms: i64,
    /// HTTP status of the check.
    pub http_status: Option<u16>,
}

/// The verdict of one model and capability over every key (D-276).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedMeasurement<'a> {
    /// Model id.
    pub model_id: &'a str,
    /// The merged measurement and, for a passing Embeddings check, its dimension.
    pub update: CapabilityUpdate,
    /// The newest time among the results behind the verdict.
    pub at_ms: i64,
    /// The keys whose results gave the verdict, in first-seen order.
    pub key_ids: Vec<&'a str>,
    /// HTTP status of the newest result behind the verdict.
    pub http_status: Option<u16>,
}

/// How strongly a measurement speaks: a pass with any key beats an absence, and an absence beats
/// "not observed".
const fn rank(measurement: Measurement) -> u8 {
    match measurement {
        Measurement::Yes => 2,
        Measurement::No => 1,
        Measurement::NotObserved => 0,
    }
}

/// Per (model, capability), in first-seen order: the strongest measurement any key gave (a pass
/// with any key means the model works for that key group, so routing must not skip it), dated by
/// the newest result behind it, with the keys and the HTTP status of that evidence.
#[must_use]
pub fn merge_measurements<'a>(measured: &[Measured<'a>]) -> Vec<MergedMeasurement<'a>> {
    let mut merged: Vec<MergedMeasurement<'a>> = Vec::new();
    let mut slot: HashMap<(&str, Capability), usize> = HashMap::new();
    for item in measured {
        let capability = item.update.capability;
        let Some(&index) = slot.get(&(item.model_id, capability)) else {
            slot.insert((item.model_id, capability), merged.len());
            merged.push(MergedMeasurement {
                model_id: item.model_id,
                update: item.update,
                at_ms: item.at_ms,
                key_ids: vec![item.key_id],
                http_status: item.http_status,
            });
            continue;
        };
        let entry = &mut merged[index];
        let (have, new) = (
            rank(entry.update.measurement),
            rank(item.update.measurement),
        );
        if new > have {
            entry.update = item.update;
            entry.at_ms = item.at_ms;
            entry.key_ids = vec![item.key_id];
            entry.http_status = item.http_status;
        } else if new == have {
            if item.at_ms >= entry.at_ms {
                entry.at_ms = item.at_ms;
                entry.http_status = item.http_status;
                entry.update.dimension = item.update.dimension.or(entry.update.dimension);
            }
            if !entry.key_ids.contains(&item.key_id) {
                entry.key_ids.push(item.key_id);
            }
        }
    }
    merged
}

/// The capability facts of a run that would change the stored matrix, merged over the keys by
/// [`merge_measurements`]. `NotObserved` is never offered (it filters nothing). Left out: a model
/// that is not stored, a fact the matrix already holds, a state the user set, and a result older
/// than the stored measurement. `used_by` names the fallback chains a change touches.
#[must_use]
pub fn plan_capabilities(
    measured: &[Measured<'_>],
    current: &[(String, ModelCapabilities)],
    used_by: &[FallbackUse],
) -> Vec<CapabilityChange> {
    let matrix_of = first_by_id(current, |(id, _)| id);
    let fallback_of = first_by_id(
        used_by.iter().filter(|used| !used.primaries.is_empty()),
        |used| &used.model_id,
    );
    merge_measurements(measured)
        .into_iter()
        .filter(|merged| merged.update.measurement != Measurement::NotObserved)
        .filter_map(|merged| {
            let (_, matrix) = *matrix_of.get(merged.model_id)?;
            let capability = merged.update.capability;
            let mut next = *matrix;
            if !next.apply_measurement(capability, merged.update.measurement, merged.at_ms) {
                return None;
            }
            let (previous, applied) = (matrix.get(capability), next.get(capability));
            let same_fact = previous.source == applied.source
                && previous.supported == applied.supported
                && previous.not_observed == applied.not_observed;
            (!same_fact).then(|| CapabilityChange {
                model_id: merged.model_id.to_owned(),
                capability,
                previous,
                applied,
                default_checked: applied.supported,
                key_ids: merged.key_ids.iter().map(|id| (*id).to_owned()).collect(),
                http_status: merged.http_status,
                used_by: fallback_of
                    .get(merged.model_id)
                    .map(|used| format!("Fallback of {}", used.primaries.join(", "))),
            })
        })
        .collect()
}

/// One flag to restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restore {
    /// Model id.
    pub model_id: String,
    /// The `enabled` value to write.
    pub enabled: bool,
}

/// What an undo does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoPlan {
    /// Flags whose current value still equals the applied one: restored.
    pub restore: Vec<Restore>,
    /// Models edited since (or deleted): left as they are, so later manual edits survive.
    pub kept: Vec<String>,
}

/// The undo of `report` against the models as they are now: a flag is restored only
/// where its current value still equals the value the apply wrote. A model created by the apply
/// is kept, disabled (its record may be referenced by now) rather than deleted.
#[must_use]
pub fn plan_undo(report: &ApplyReport, current: &[CurrentModel]) -> UndoPlan {
    let mut plan = UndoPlan {
        restore: Vec::new(),
        kept: Vec::new(),
    };
    for state in &report.previous {
        let now = current
            .iter()
            .find(|model| model.model_id == state.model_id)
            .map(|model| model.enabled);
        if now == Some(state.applied_enabled) {
            plan.restore.push(Restore {
                model_id: state.model_id.clone(),
                enabled: state.previous_enabled,
            });
        } else {
            plan.kept.push(state.model_id.clone());
        }
    }
    plan
}

/// Whether "Undo last apply" is still offered: within 24 hours of the apply and until the next
/// apply.
#[must_use]
pub const fn undo_available(applied_at_ms: i64, now_ms: i64, newer_apply_exists: bool) -> bool {
    !newer_apply_exists && now_ms.saturating_sub(applied_at_ms) < UNDO_WINDOW_MS
}

#[cfg(test)]
mod tests;
