//! Embedding and decision-API checks.
//!
//! Single responsibility: the two checks that do not speak a chat protocol.

use serde_json::{Value, json};

use super::{Ctx, Outcome, fail, pass, post_json, preview, set_tokens};
use crate::llm::adapters::{decision, embeddings};
use crate::llm::failure::FailureKind;
use crate::llm::hub::HubUsage;
use crate::llm::router::Target;
use crate::llm::store::ProbeResult;
use crate::llm::upstream::AttemptError;
use crate::llm::wire::{Protocol, gemini_embed_url};
use pingdeck_base::error::AppError;

/// The Embeddings check's request: the protocol, an explicit URL (Gemini only) and the body.
pub(super) struct EmbeddingsRequest {
    pub(super) protocol: Protocol,
    pub(super) url: Option<String>,
    pub(super) body: Value,
}

/// Builds the Embeddings check's request for the first embeddings protocol the provider speaks
/// (OpenAI-style, then Gemini).
///
/// # Errors
/// [`AttemptError::Invalid`] when the provider speaks neither or the Gemini URL cannot be built.
pub(super) fn embeddings_request(target: &Target) -> Result<EmbeddingsRequest, AttemptError> {
    const INPUT: &str = "PingDeck embedding test";
    if target.endpoint(Protocol::Embeddings).is_some() {
        Ok(EmbeddingsRequest {
            protocol: Protocol::Embeddings,
            url: None,
            body: embeddings::openai_probe_body(&target.model, INPUT),
        })
    } else if let Some(endpoint) = target.endpoint(Protocol::Gemini) {
        let url = gemini_embed_url(&target.base_url, &endpoint.path_prefix, &target.model)
            .map_err(AttemptError::Invalid)?;
        Ok(EmbeddingsRequest {
            protocol: Protocol::Gemini,
            url: Some(url),
            body: embeddings::gemini_probe_body(&target.model, INPUT),
        })
    } else {
        Err(AttemptError::Invalid(AppError::unsupported(
            "provider speaks no embeddings protocol",
        )))
    }
}

pub(super) async fn test_embeddings(ctx: &Ctx<'_>, result: &mut ProbeResult) -> Outcome {
    let request = embeddings_request(ctx.target)?;
    result.endpoint = request.protocol.as_str().to_owned();
    let value = post_json(
        ctx,
        request.protocol,
        request.url.as_deref(),
        &request.body,
        ctx.timeouts.embeddings_request,
    )
    .await?;
    let (dimension, usage) = if request.protocol == Protocol::Embeddings {
        (
            embeddings::openai_dimension(&value),
            embeddings::usage(&value),
        )
    } else {
        (embeddings::gemini_dimension(&value), HubUsage::default())
    };
    result.input_tokens = usage.input_tokens;
    match dimension.filter(|dimension| *dimension > 0) {
        Some(dimension) => {
            result.detail = json!({"dimension": dimension});
            pass(result, format!("{dimension}-dimensional embedding"));
        }
        None => fail(
            result,
            FailureKind::ProtocolMismatch,
            format!(
                "the response has no embedding vector: {}",
                preview(&value.to_string(), 200)
            ),
        ),
    }
    Ok(())
}

pub(super) async fn test_decision(ctx: &Ctx<'_>, result: &mut ProbeResult) -> Outcome {
    let protocol = [Protocol::DecisionSystemone, Protocol::DecisionOpenai]
        .into_iter()
        .find(|protocol| ctx.target.endpoint(*protocol).is_some())
        .ok_or_else(|| {
            AttemptError::Invalid(AppError::unsupported(
                "provider speaks no decision protocol",
            ))
        })?;
    result.endpoint = protocol.as_str().to_owned();
    let body = decision::probe_body(protocol, &ctx.target.model);
    let value = post_json(ctx, protocol, None, &body, ctx.timeouts.chat).await?;
    match decision::read_response(&value) {
        Ok(summary) => {
            set_tokens(result, &summary.usage);
            result.detail = decision::summary_detail(&summary);
            pass(result, format!("{} answer(s)", summary.answers));
        }
        Err(error) => fail(result, FailureKind::ProtocolMismatch, error.message()),
    }
    Ok(())
}
