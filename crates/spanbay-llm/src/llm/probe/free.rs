//! The three free checks: connectivity, authentication and the model list.
//!
//! Single responsibility: HTTP-only probes that spend no model tokens.

use serde_json::json;

use super::http::{list_model_ids, list_protocol, read_capped};
use super::{Ctx, Outcome, chat, fail, pass, rejected, set_tokens, user_request};
use crate::llm::failure::FailureKind;
use crate::llm::hub::HubRequest;
use crate::llm::store::ProbeResult;
use crate::llm::upstream::AttemptError;
use crate::llm::wire::{models_url, request_headers};

/// The one-token chat that tests a key when the provider has no model list endpoint.
pub(super) fn auth_request(model: &str) -> HubRequest {
    user_request(model, "hi", 1)
}

pub(super) async fn test_connectivity(ctx: &Ctx<'_>, result: &mut ProbeResult) -> Outcome {
    let url = ctx.target.base_url.trim();
    let response = ctx
        .get("base", ctx.timeouts.connectivity)
        .raw(url, &[])
        .await?;
    let status = response.status().as_u16();
    result.status = Some(status);
    pass(result, format!("the host answered HTTP {status}"));
    Ok(())
}

pub(super) async fn test_auth(ctx: &Ctx<'_>, result: &mut ProbeResult) -> Outcome {
    if let Some(endpoint) = ctx.models_endpoint() {
        let protocol = list_protocol(ctx.target);
        let headers = request_headers(
            protocol,
            ctx.target.auth_for(protocol),
            &ctx.target.secret,
            &ctx.target.quirks,
        );
        let url = models_url(&ctx.target.base_url, endpoint);
        let response = ctx
            .get("models", ctx.timeouts.auth)
            .raw(&url, &headers)
            .await?;
        let status = response.status().as_u16();
        result.status = Some(status);
        if rejected(status) {
            let retry_after_ms = crate::llm::upstream::retry_after_ms(response.headers());
            let body = read_capped(response, ctx.cancel).await;
            return Err(AttemptError::Status {
                status,
                body,
                retry_after_ms,
            });
        }
        let message = if response.status().is_success() {
            "the key is accepted".to_owned()
        } else {
            format!(
                "HTTP {status}: the endpoint answered the credentialed request, so the key is accepted"
            )
        };
        pass(result, message);
        return Ok(());
    }
    // No list endpoint to hit: the cheapest authenticated call is a one-token chat.
    let protocol = ctx.chat_proto()?;
    result.endpoint = protocol.as_str().to_owned();
    match chat(
        ctx,
        protocol,
        &auth_request(&ctx.target.model),
        ctx.timeouts.auth,
    )
    .await
    {
        Ok((response, _)) => {
            result.status = Some(200);
            set_tokens(result, &response.usage);
            pass(result, "the key is accepted");
            Ok(())
        }
        Err(error) if error.status().is_some_and(|status| !rejected(status)) => {
            result.status = error.status();
            pass(
                result,
                format!(
                    "{}: the key is accepted, the request itself was refused",
                    error.message()
                ),
            );
            Ok(())
        }
        Err(error) => Err(error),
    }
}

pub(super) async fn test_models(ctx: &Ctx<'_>, result: &mut ProbeResult) -> Outcome {
    let Some(endpoint) = ctx.models_endpoint() else {
        fail(
            result,
            FailureKind::Unknown,
            "this provider has no model list endpoint",
        );
        return Ok(());
    };
    let ids = list_model_ids(&ctx.get("models", ctx.timeouts.models), endpoint).await?;
    let wanted = ctx
        .target
        .model
        .strip_prefix("models/")
        .unwrap_or(&ctx.target.model);
    let listed = ids.iter().any(|id| id == wanted);
    result.detail = json!({
        "count": ids.len(),
        "listed": listed,
        "sample": ids.iter().take(20).collect::<Vec<_>>(),
    });
    pass(
        result,
        format!(
            "{} models; {} is {}",
            ids.len(),
            ctx.target.model,
            if listed { "listed" } else { "not listed" }
        ),
    );
    Ok(())
}
