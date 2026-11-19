//! Compacting a chat's history into a summary, proactively, reactively and on demand.
//!
//! Single responsibility: ask the run's own model for a summary of the older history
//! (`context::compaction_cut` picks the cut so a tool call and its results stay together and the
//! last `keep_recent_turns` turns stay verbatim, or, when one turn holds everything, its last tool
//! steps), store it as the chat's compaction summary through
//! the host and publish `ContextCompacted`. When to compact is the loop's decision (the
//! threshold, or a context-length error from the provider).

use pingdeck_base::cancel::CancelToken;
use pingdeck_base::error::AppError;
use pingdeck_llm::llm::hub::{HubMessage, text_of};

use super::context::{
    CompactionCut, compact_history_at, compaction_cut, compaction_request, estimate_context_tokens,
};
use super::model::AgentEvent;
use super::runner::{ChatBackend, HistoryEntry, RunHost, RunInput};

/// What a compaction did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compacted {
    /// Estimated prompt tokens before.
    pub before_tokens: u64,
    /// Estimated prompt tokens after.
    pub after_tokens: u64,
    /// The stored summary message.
    pub summary_message_id: String,
}

/// Where a compaction of `entries` keeping `keep_turns` turns would cut
/// ([`compaction_cut`]); `None` when it would find nothing new to summarise.
#[must_use]
pub fn cut_of(entries: &[HistoryEntry], keep_turns: usize) -> Option<CompactionCut> {
    let roles: Vec<_> = entries.iter().map(|entry| entry.message.role).collect();
    compaction_cut(&roles, |index| entries[index].summary, keep_turns)
}

/// Summarises the history `entries` except its last `keep_turns` turns (or, when a single turn
/// holds the whole history, its last tool steps), stores the summary and emits
/// `ContextCompacted`. `None` when there is nothing older to summarise (only an earlier summary
/// is: no model call is made) or the summary would not be shorter than what it replaces.
///
/// # Errors
/// The summarising call's error (including `Cancelled`) or the store's.
pub async fn compact<B: ChatBackend, H: RunHost>(
    backend: &B,
    host: &H,
    input: &RunInput,
    entries: &[HistoryEntry],
    cancel: &CancelToken,
) -> Result<Option<Compacted>, AppError> {
    let Some(cut) = cut_of(entries, input.compaction.keep_turns) else {
        return Ok(None);
    };
    let messages: Vec<HubMessage> = entries.iter().map(|entry| entry.message.clone()).collect();
    let before = estimate_context_tokens(input.system.as_deref(), &messages, &input.tools);
    let label = input.model_label.clone();
    let metered = std::sync::Mutex::new(None);
    let meta = super::runner::call_meta(host, None, true);
    let (label_ref, metered_ref, meta_ref) = (&label, &metered, &meta);
    let compaction = compact_history_at(&messages, cut, |transcript| async move {
        backend
            .complete(
                meta_ref,
                &input.target,
                compaction_request(label_ref, &transcript),
                Some(cancel),
            )
            .await
            .map(|(response, served)| {
                *metered_ref
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some((response.usage.clone(), served));
                response.text()
            })
    })
    .await;
    // The summarising call costs like any other: it counts in the run, the chat and the log
    // also when its answer is then discarded.
    let usage = metered
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let after = match &compaction {
        Ok(compaction) => {
            let mut after_messages = vec![compaction.summary.clone()];
            after_messages.extend(compaction.kept.iter().cloned());
            estimate_context_tokens(input.system.as_deref(), &after_messages, &input.tools)
        }
        _ => before,
    };
    // The context reading carries on: the prompt as it stands after this call (AI-chatux-10).
    let context_now = after.min(before);
    if let Some((usage, served)) = usage {
        let fallback = super::runner::fallback_info(host, input, served.as_ref()).await;
        let priced = fallback.as_ref().unwrap_or(&input.model);
        let cost = priced.call_cost(&usage);
        let totals = host
            .record_usage(
                &usage,
                &cost,
                context_now,
                pingdeck_llm::llm::client::CallPurpose::Compaction,
            )
            .await?;
        host.emit(AgentEvent::Usage {
            iteration_usage: usage,
            run_usage: totals.usage,
            cost_micro_usd: totals.cost_micro,
            context_used_tokens: context_now,
            context_limit_tokens: input.model.context_limit,
            currency: cost.currency.clone(),
            ttft_ms: None,
            stream_ms: None,
        });
    }
    let compaction = compaction?;
    if after >= before {
        return Ok(None);
    }
    // The summary covers the stored messages up to the last replaced one that is not itself a
    // summary; a replaced range that holds only the old summary leaves nothing new to cover.
    let Some(through) = entries[..compaction.replaced]
        .iter()
        .rev()
        .find(|entry| !entry.summary)
        .map(|entry| entry.id.clone())
    else {
        return Ok(None);
    };
    let text = text_of(&compaction.summary.content);
    let saved = host.save_summary(text, &through).await?;
    host.emit(AgentEvent::ContextCompacted {
        before_tokens: before,
        after_tokens: after,
        summary_message_id: saved.id.clone(),
    });
    Ok(Some(Compacted {
        before_tokens: before,
        after_tokens: after,
        summary_message_id: saved.id,
    }))
}
