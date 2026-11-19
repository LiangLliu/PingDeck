use super::*;
use ProbeStatus as S;

fn row(id: &str, status: Option<ProbeStatus>, http: Option<u16>) -> ApplyRow {
    ApplyRow {
        model_id: id.to_owned(),
        status,
        http_status: http,
    }
}

fn model(id: &str, enabled: bool) -> CurrentModel {
    CurrentModel {
        model_id: id.to_owned(),
        enabled,
    }
}

fn ids(changes: &[ApplyChange]) -> Vec<&str> {
    changes
        .iter()
        .map(|change| change.model_id.as_str())
        .collect()
}

/// The documented sample: 36 pass, `gemini-3.2-pro` is Rejected 403, `glm-5` timed out.
fn sample() -> (Vec<ApplyRow>, Vec<CurrentModel>) {
    let mut rows: Vec<ApplyRow> = (0..36)
        .map(|n| row(&format!("ok-{n}"), Some(S::Ready), Some(200)))
        .collect();
    rows.push(row("gemini-3.2-pro", Some(S::Rejected), Some(403)));
    rows.push(row("glm-5", Some(S::TimedOut), None));
    let mut current: Vec<CurrentModel> =
        (0..36).map(|n| model(&format!("ok-{n}"), false)).collect();
    current.push(model("gemini-3.2-pro", true));
    current.push(model("glm-5", true));
    (rows, current)
}

#[test]
fn the_sample_enables_36_and_offers_one_disable_unchecked() {
    let (rows, current) = sample();
    let plan = plan_apply(&rows, &current, &[], false);
    assert_eq!(plan.enable.len(), 36);
    // glm-5 timed out: a transient failure is never suggested for disabling.
    assert_eq!(ids(&plan.disable), ["gemini-3.2-pro"]);
    assert!(plan.create.is_empty() && !plan.host_level_failure && !plan.is_empty());
    assert!(!plan.disable[0].default_checked);
    assert_eq!(plan.disable[0].reason, "Rejected 403");
    assert_eq!(plan.enable[0].reason, "Ready");
    assert_eq!(plan.disable[0].previous, Some(true));
    assert!(plan.enable.iter().all(|change| change.default_checked));
}

#[test]
fn only_a_timeout_is_a_transient_failure_and_disables_start_unchecked() {
    let rows = [
        row("a", Some(S::TimedOut), None),
        row("b", Some(S::Rejected), Some(402)),
        row("c", Some(S::Unreachable), None),
        row("d", Some(S::InvalidResponse), Some(200)),
        row("e", Some(S::Unsupported), Some(400)),
    ];
    let current: Vec<_> = ["a", "b", "c", "d", "e"]
        .iter()
        .map(|id| model(id, true))
        .collect();
    let plan = plan_apply(&rows, &current, &[], false);
    assert_eq!(ids(&plan.disable), ["b", "c", "d", "e"]);
    assert!(plan.disable.iter().all(|change| !change.default_checked));
}

#[test]
fn rate_limited_and_refused_chat_results_leave_the_model_unchanged() {
    let rows = [
        row("limited", Some(S::RateLimited), Some(429)),
        row("refused-400", Some(S::InvalidResponse), Some(400)),
        row("refused-422", Some(S::InvalidResponse), Some(422)),
        // An invalid response that was an HTTP 200 still fails the model.
        row("empty", Some(S::InvalidResponse), Some(200)),
    ];
    let current: Vec<_> = ["limited", "refused-400", "refused-422", "empty"]
        .iter()
        .map(|id| model(id, true))
        .collect();
    let plan = plan_apply(&rows, &current, &[], false);
    assert_eq!(ids(&plan.disable), ["empty"]);
    assert!(plan.enable.is_empty() && plan.create.is_empty());
    // The same rows on disabled models stay disabled, and a ghost row is not created.
    let off: Vec<_> = ["limited", "refused-400"]
        .iter()
        .map(|id| model(id, false))
        .collect();
    assert!(plan_apply(&rows[..2], &off, &[], false).is_empty());
    assert!(plan_apply(&rows[..3], &[], &[], false).is_empty());
}

#[test]
fn skipped_and_untested_rows_are_untouched() {
    let rows = [row("a", Some(S::Skipped), None), row("b", None, None)];
    let current = [model("a", true), model("b", false)];
    assert!(plan_apply(&rows, &current, &[], false).is_empty());
}

#[test]
fn rows_already_in_the_target_state_are_not_listed() {
    let rows = [
        row("on-pass", Some(S::Ready), None),
        row("off-fail", Some(S::Rejected), Some(403)),
    ];
    let current = [model("on-pass", true), model("off-fail", false)];
    assert!(plan_apply(&rows, &current, &[], false).is_empty());
}

#[test]
fn a_listed_model_is_created_when_passing_and_ignored_when_failing() {
    let rows = [
        row("ghost-ok", Some(S::Ready), None),
        row("ghost-bad", Some(S::Rejected), Some(403)),
    ];
    let plan = plan_apply(&rows, &[], &[], false);
    assert_eq!(ids(&plan.create), ["ghost-ok"]);
    assert_eq!(plan.create[0].previous, None);
    assert_eq!(plan.create[0].action, ApplyAction::Create);
    assert!(plan.enable.is_empty() && plan.disable.is_empty());
}

#[test]
fn a_host_level_failure_changes_nothing() {
    let (rows, current) = sample();
    let plan = plan_apply(&rows, &current, &[], true);
    assert!(plan.host_level_failure && plan.is_empty());
}

#[test]
fn fallbacks_carry_the_note() {
    let (rows, current) = sample();
    let used_by = [
        FallbackUse {
            model_id: "gemini-3.2-pro".to_owned(),
            primaries: vec!["Hub/claude".to_owned()],
        },
        FallbackUse {
            model_id: "ok-0".to_owned(),
            primaries: vec!["Hub/a".to_owned(), "Hub/b".to_owned()],
        },
        FallbackUse {
            model_id: "glm-5".to_owned(),
            primaries: Vec::new(),
        },
    ];
    let plan = plan_apply(&rows, &current, &used_by, false);
    assert_eq!(
        plan.disable[0].alias_note.as_deref(),
        Some("Fallback of Hub/claude")
    );
    assert_eq!(
        plan.enable[0].alias_note.as_deref(),
        Some("Fallback of Hub/a, Hub/b")
    );
    assert_eq!(plan.enable[1].alias_note, None);
}

#[test]
fn the_report_holds_only_the_checked_rows_and_their_previous_flags() {
    let (rows, current) = sample();
    let plan = plan_apply(&rows, &current, &[], false);
    let mut selection = plan.default_selection();
    assert_eq!(selection.enable.len(), 36);
    assert!(selection.disable.is_empty());
    selection.disable = vec!["gemini-3.2-pro".to_owned()];
    let report = plan.report(&selection);
    assert_eq!(report.enabled.len(), 36);
    assert_eq!(report.disabled, ["gemini-3.2-pro"]);
    assert!(report.created.is_empty());
    assert_eq!(report.previous.len(), 37);
    let gemini = report
        .previous
        .iter()
        .find(|state| state.model_id == "gemini-3.2-pro")
        .unwrap();
    assert!(gemini.previous_enabled && !gemini.applied_enabled);
    // Unknown ids in a selection are ignored.
    let report = plan.report(&ApplySelection {
        enable: vec!["nope".to_owned()],
        ..ApplySelection::default()
    });
    assert!(report.previous.is_empty());
}

#[test]
fn undo_restores_all_flags_when_nothing_changed_since() {
    let (rows, mut current) = sample();
    let plan = plan_apply(&rows, &current, &[], false);
    let mut selection = plan.default_selection();
    selection.disable = vec!["gemini-3.2-pro".to_owned()];
    let report = plan.report(&selection);
    // Apply.
    for state in &report.previous {
        let model = current
            .iter_mut()
            .find(|m| m.model_id == state.model_id)
            .unwrap();
        model.enabled = state.applied_enabled;
    }
    let undo = plan_undo(&report, &current);
    assert_eq!(undo.restore.len(), 37);
    assert!(undo.kept.is_empty());
    for restore in &undo.restore {
        let state = report
            .previous
            .iter()
            .find(|s| s.model_id == restore.model_id)
            .unwrap();
        assert_eq!(restore.enabled, state.previous_enabled);
    }
}

#[test]
fn undo_leaves_models_edited_since_the_apply() {
    let rows = [row("glm-5", Some(S::Rejected), Some(403))];
    let current = [model("glm-5", true)];
    let plan = plan_apply(&rows, &current, &[], false);
    let report = plan.report(&ApplySelection {
        disable: vec!["glm-5".to_owned()],
        ..ApplySelection::default()
    });
    // Settings re-enabled glm-5 after the apply.
    let after = [model("glm-5", true)];
    let undo = plan_undo(&report, &after);
    assert!(undo.restore.is_empty());
    assert_eq!(undo.kept, ["glm-5"]);
    // A deleted model is kept out too.
    assert_eq!(plan_undo(&report, &[]).kept, ["glm-5"]);
}

#[test]
fn undo_of_a_created_model_disables_it_while_it_is_still_enabled() {
    let plan = plan_apply(&[row("new", Some(S::Ready), None)], &[], &[], false);
    let report = plan.report(&plan.default_selection());
    assert_eq!(report.created, ["new"]);
    let undo = plan_undo(&report, &[model("new", true)]);
    assert_eq!(
        undo.restore,
        [Restore {
            model_id: "new".to_owned(),
            enabled: false
        }]
    );
}

fn vision(model_id: &str, measurement: Measurement, at_ms: i64) -> Measured<'_> {
    vision_with(model_id, "k1", measurement, at_ms)
}

fn vision_with<'a>(
    model_id: &'a str,
    key_id: &'a str,
    measurement: Measurement,
    at_ms: i64,
) -> Measured<'a> {
    Measured {
        model_id,
        key_id,
        update: CapabilityUpdate {
            capability: Capability::Vision,
            measurement,
            dimension: None,
        },
        at_ms,
        http_status: Some(200),
    }
}

#[test]
fn a_pass_with_one_key_group_wins_and_known_or_stale_facts_are_left_out() {
    use crate::llm::capabilities::CapabilitySource;
    let mut newer = ModelCapabilities::default();
    newer.apply_measurement(Capability::Vision, Measurement::Yes, 900);
    let mut overridden = ModelCapabilities::default();
    overridden.apply(Capability::Vision, true, CapabilitySource::UserOverride);
    let current = vec![
        ("split".to_owned(), ModelCapabilities::default()),
        ("blind".to_owned(), ModelCapabilities::default()),
        ("newer".to_owned(), newer),
        ("mine".to_owned(), overridden),
    ];
    let measured = [
        // Key group A cannot send images to `split`, key group B can.
        vision_with("split", "kA", Measurement::No, 100),
        vision_with("split", "kB", Measurement::Yes, 120),
        vision("blind", Measurement::No, 100),
        vision("newer", Measurement::No, 100),
        vision("mine", Measurement::No, 100),
        vision("ghost", Measurement::No, 100),
    ];
    let used_by = [FallbackUse {
        model_id: "blind".to_owned(),
        primaries: vec!["openai/gpt-5".to_owned()],
    }];
    let plan = plan_capabilities(&measured, &current, &used_by);
    let facts: Vec<(&str, bool)> = plan
        .iter()
        .map(|change| (change.model_id.as_str(), change.applied.supported))
        .collect();
    assert_eq!(facts, [("split", true), ("blind", false)]);
    assert_eq!(plan[1].previous, CapabilityState::default());
    // The evidence: the key behind the verdict, its HTTP status and the routes it touches.
    assert_eq!(plan[0].key_ids, ["kB"]);
    assert_eq!(plan[0].http_status, Some(200));
    assert_eq!(plan[1].used_by.as_deref(), Some("Fallback of openai/gpt-5"));
    // A change to "supported" is checked by default, a change to "not supported" is not; only
    // checked facts reach the report.
    let apply = ApplyPlan {
        enable: Vec::new(),
        disable: Vec::new(),
        create: Vec::new(),
        capabilities: plan,
        host_level_failure: false,
    };
    let mut selection = apply.default_selection();
    let picked: Vec<&str> = selection
        .capabilities
        .iter()
        .map(|pick| pick.model_id.as_str())
        .collect();
    assert_eq!(picked, ["split"]);
    selection.capabilities.push(CapabilityPick {
        model_id: "blind".to_owned(),
        capability: Capability::Vision,
    });
    selection
        .capabilities
        .retain(|pick| pick.model_id == "blind");
    assert_eq!(apply.report(&selection).capabilities.len(), 1);
}

#[test]
fn merged_measurements_let_any_pass_win_whatever_the_order() {
    // The last result is an absence, yet the pass of another key decides (D-276).
    let merged = merge_measurements(&[
        vision_with("m", "kA", Measurement::Yes, 100),
        vision_with("m", "kB", Measurement::No, 200),
        vision_with("m", "kC", Measurement::Yes, 150),
    ]);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].update.measurement, Measurement::Yes);
    assert_eq!(
        (merged[0].at_ms, merged[0].key_ids.clone()),
        (150, vec!["kA", "kC"])
    );
    // An absence beats "not observed".
    let merged = merge_measurements(&[
        vision_with("m", "kA", Measurement::NotObserved, 100),
        vision_with("m", "kB", Measurement::No, 50),
    ]);
    assert_eq!(merged[0].update.measurement, Measurement::No);
}

#[test]
fn a_run_older_than_seven_days_is_not_applied() {
    assert!(run_applicable(0, APPLY_MAX_AGE_MS) && !run_applicable(0, APPLY_MAX_AGE_MS + 1));
}

#[test]
fn undo_is_offered_for_24_hours_and_until_the_next_apply() {
    assert!(undo_available(1_000, 1_000 + UNDO_WINDOW_MS - 1, false));
    assert!(!undo_available(1_000, 1_000 + UNDO_WINDOW_MS, false));
    assert!(!undo_available(1_000, 2_000, true));
}
