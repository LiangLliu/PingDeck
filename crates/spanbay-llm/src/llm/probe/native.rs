//! The Full-set checks that need the hub's native request parts: provider web search and a
//! PDF document.
//!
//! Single responsibility: [`test_search`] and [`test_pdf`], with the bundled one-page PDF.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;

use super::{Ctx, Outcome, chat, fail, pass, preview, set_tokens, user_request};
use crate::llm::failure::FailureKind;
use crate::llm::hub::{
    HubDocument, HubDocumentSource, HubMessage, HubNativeTool, HubPart, HubRequest, HubResponse,
    HubRole,
};
use crate::llm::store::ProbeResult;
use crate::llm::upstream::{AttemptError, provider_message};

/// The word written on the bundled PDF page.
pub(super) const PDF_WORD: &str = "PINEAPPLE";

/// Statuses with which a provider says it does not take the tool or the document.
const fn feature_refused(status: u16) -> bool {
    matches!(status, 400 | 415 | 422)
}

/// A one-page PDF with [`PDF_WORD`] set in 36 pt Helvetica. Built in code so the cross-reference
/// offsets are exact; the bytes are the same on every run.
pub(super) fn pdf_bytes() -> Vec<u8> {
    let content = format!("BT /F1 36 Tf 72 700 Td ({PDF_WORD}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

/// How many native search events and citations a response carries.
pub(super) fn search_evidence(response: &HubResponse) -> (usize, usize) {
    let mut events = 0;
    let mut citations = 0;
    for part in &response.content {
        match part {
            HubPart::ServerToolUse { .. } => events += 1,
            HubPart::ServerToolResult {
                citations: found, ..
            } => {
                events += 1;
                citations += found.len();
            }
            HubPart::Text {
                citations: found, ..
            } => citations += found.len(),
            _ => {}
        }
    }
    (events, citations)
}

/// The Search check's request: a question that needs current information and the native tool.
pub(super) fn search_request(model: &str) -> HubRequest {
    let mut request = user_request(
        model,
        "Search the web: what is the latest stable version of the Rust programming language? \
         Cite your source.",
        512,
    );
    request.native_tools = vec![HubNativeTool::WebSearch {
        max_uses: Some(1),
        allowed_domains: Vec::new(),
        blocked_domains: Vec::new(),
    }];
    request
}

pub(super) async fn test_search(ctx: &Ctx<'_>, result: &mut ProbeResult) -> Outcome {
    let protocol = ctx.chat_proto()?;
    result.endpoint = protocol.as_str().to_owned();
    let request = search_request(&ctx.target.model);
    let (response, ms) = match chat(ctx, protocol, &request, ctx.timeouts.chat).await {
        Ok(answer) => answer,
        Err(AttemptError::Status { status, body, .. }) if feature_refused(status) => {
            result.status = Some(status);
            fail(
                result,
                FailureKind::ProtocolMismatch,
                format!(
                    "the provider rejected the search tool: {}",
                    provider_message(&body)
                ),
            );
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    result.latency_ms = Some(ms);
    set_tokens(result, &response.usage);
    let (events, citations) = search_evidence(&response);
    result.detail = json!({
        "search_events": events,
        "citations": citations,
        "answer": preview(&response.text(), 200),
    });
    if events > 0 || citations > 0 {
        pass(
            result,
            format!("searched the web ({events} search events, {citations} citations)"),
        );
    } else {
        fail(
            result,
            FailureKind::Unknown,
            "the answer carries no search event and no citation",
        );
    }
    Ok(())
}

/// The PDF check's request: the bundled one-page PDF and a question about its word.
pub(super) fn pdf_request(model: &str) -> HubRequest {
    let mut request = user_request(model, "", 256);
    request.messages = vec![HubMessage {
        role: HubRole::User,
        content: vec![
            HubPart::Document {
                document: HubDocument {
                    media_type: "application/pdf".to_owned(),
                    name: Some("probe.pdf".to_owned()),
                    source: HubDocumentSource::Base64 {
                        data: STANDARD.encode(pdf_bytes()),
                    },
                },
            },
            HubPart::text(
                "Which single word is written on the page of this PDF? Answer with that word only.",
            ),
        ],
    }];
    request
}

pub(super) async fn test_pdf(ctx: &Ctx<'_>, result: &mut ProbeResult) -> Outcome {
    let protocol = ctx.chat_proto()?;
    result.endpoint = protocol.as_str().to_owned();
    let request = pdf_request(&ctx.target.model);
    let (response, ms) = match chat(ctx, protocol, &request, ctx.timeouts.chat).await {
        Ok(answer) => answer,
        Err(AttemptError::Status { status, body, .. }) if feature_refused(status) => {
            result.status = Some(status);
            fail(
                result,
                FailureKind::ProtocolMismatch,
                format!(
                    "the provider rejected the document: {}",
                    provider_message(&body)
                ),
            );
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    result.latency_ms = Some(ms);
    set_tokens(result, &response.usage);
    let text = response.text();
    result.detail = json!({"answer": preview(&text, 200)});
    if text
        .to_ascii_lowercase()
        .contains(&PDF_WORD.to_ascii_lowercase())
    {
        pass(result, "the model read the document");
    } else {
        fail(
            result,
            FailureKind::Unknown,
            format!(
                "the model did not read the word on the page: {}",
                preview(&text, 100)
            ),
        );
    }
    Ok(())
}
