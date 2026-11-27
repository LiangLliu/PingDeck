//! Delete, rename and rule state: what a deleted provider leaves behind, the impact preview, the
//! orphan rule.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::llm::store::ClientInput;
use crate::uc_support::Fx;
use crate::{llm_gateway_client_create_impl, llm_model_delete_impl, llm_provider_delete_impl};
use spanbay_model::model_catalog::{ModelPricing, PricingMode};

fn model_ref(provider_id: &str, model: &str) -> ModelRef {
    ModelRef {
        provider_id: provider_id.to_owned(),
        model: model.to_owned(),
    }
}

fn client_for(fx: &Fx, models: &[&str]) -> String {
    llm_gateway_client_create_impl(
        &fx.state,
        ClientInput {
            name: "Codex".to_owned(),
            allow_all: false,
            allowed_aliases: Vec::new(),
            allowed_models: models.iter().map(|m| (*m).to_owned()).collect(),
        },
    )
    .unwrap()
    .client
    .id
}

#[test]
fn a_provider_recreated_under_a_deleted_name_inherits_no_grant_price_or_rule() {
    let fx = Fx::new();
    let old = fx.provider("Relay", 1, &["m"]);
    let other = fx.provider("Other", 1, &["o"]);
    let client = client_for(&fx, &["relay/m", "other/o"]);
    fx.alias("smart", &[("other", None, "o"), ("relay", None, "m")]);
    with_connection(&fx.state, |c| {
        store::save_pricing(
            c,
            ModelPricing {
                mode: PricingMode::Custom,
                input_per_mtok: Some(1.0),
                output_per_mtok: Some(2.0),
                ..ModelPricing::dynamic(&old.provider_id, "m")
            },
            1,
        )
    })
    .unwrap();

    let impact = llm_provider_impact_impl(&fx.state, &old.provider_id).unwrap();
    assert_eq!(impact.clients.len(), 1);
    assert!(!impact.clients[0].only);
    assert_eq!(impact.gateway_rules, vec![model_ref("other", "o")]);
    assert_eq!(
        (impact.keys, impact.models, impact.unknown.len()),
        (1, 1, 0)
    );

    llm_provider_delete_impl(&fx.state, &old.provider_id).unwrap();
    let again = fx.provider("Relay", 1, &["m"]);
    assert_eq!(again.provider_id, "relay-2");
    with_connection(&fx.state, |c| {
        assert_eq!(
            store::get_client(c, &client)?.allowed_models,
            vec!["other/o"]
        );
        assert!(
            store::list_pricing(c, None)?
                .iter()
                .all(|p| p.provider_id != "relay")
        );
        assert!(!store::list_fallbacks(c)?.contains_key(&model_ref("other", "o")));
        Ok(())
    })
    .unwrap();
    drop(other);
}

#[test]
fn a_rule_whose_primary_was_deleted_is_listed_as_an_orphan_and_can_be_deleted() {
    let fx = Fx::new();
    fx.provider("Relay", 1, &["m", "n"]);
    fx.alias("smart", &[("relay", None, "m"), ("relay", None, "n")]);
    // A model leaving the list without the user (a live refresh) leaves its rule behind.
    with_connection(&fx.state, |c| store::delete_model(c, "relay", "m")).unwrap();
    fx.state.llm.touch();

    let rules = llm_gateway_rules_impl(&fx.state).unwrap();
    assert_eq!(rules.len(), 1);
    assert!(rules[0].orphan);
    assert_eq!(rules[0].fallback_status, vec![RuleStatus::Ok]);

    crate::llm_model_fallbacks_set_impl(&fx.state, "relay", "m", Vec::new()).unwrap();
    assert!(llm_gateway_rules_impl(&fx.state).unwrap().is_empty());
    // A model the user deletes takes its rules along.
    fx.alias("smart", &[("relay", None, "n"), ("relay", None, "x")]);
    llm_model_delete_impl(&fx.state, "relay", "n").unwrap();
    assert!(llm_gateway_rules_impl(&fx.state).unwrap().is_empty());
}

#[test]
fn an_unknown_provider_has_no_impact_to_preview() {
    let fx = Fx::new();
    let error = llm_provider_impact_impl(&fx.state, "nope").unwrap_err();
    assert!(matches!(error, AppError::NotFound { .. }), "{error:?}");
}
