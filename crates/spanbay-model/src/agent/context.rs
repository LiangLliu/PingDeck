//! Repairing a message list whose tool calls and results do not line up.

use std::collections::HashSet;

use crate::hub::{HubMessage, HubPart, HubRole};

/// Marker a repaired tool result carries: the call never finished.
const INTERRUPTED_RESULT: &str = "[interrupted before tool finished]";

/// Makes `messages` valid for every provider after a crash or cancelled run.
///
/// - Every assistant `ToolCall` gets a `ToolResult` in the Tool message(s) that immediately follow
///   it; a missing one is a synthetic error result placed in the first of them (a new Tool message
///   is inserted when the next message is not a Tool message). Results in that first message are
///   put in call order.
/// - `ToolResult` parts that answer no call of the preceding assistant message (and duplicates)
///   are removed, and Tool messages left empty are dropped.
/// - Assistant messages with no content at all are dropped.
///
/// Idempotent.
pub fn repair_orphan_tool_calls(messages: &mut Vec<HubMessage>) {
    messages.retain(|m| m.role != HubRole::Assistant || !m.content.is_empty());

    // Drop results that do not answer a call of the assistant message they follow.
    let mut open: HashSet<String> = HashSet::new();
    for message in messages.iter_mut() {
        match message.role {
            HubRole::Tool => message.content.retain(|part| match part {
                HubPart::ToolResult { call_id, .. } => open.remove(call_id),
                _ => true,
            }),
            HubRole::Assistant => {
                open = message
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        HubPart::ToolCall { id, .. } => Some(id.clone()),
                        _ => None,
                    })
                    .collect();
            }
            HubRole::User => open.clear(),
        }
    }
    messages.retain(|m| m.role != HubRole::Tool || !m.content.is_empty());

    // Answer every call that is still open.
    let mut index = 0;
    while index < messages.len() {
        let calls = calls_of(&messages[index]);
        index += 1;
        if calls.is_empty() {
            continue;
        }
        let mut end = index;
        while messages.get(end).is_some_and(|m| m.role == HubRole::Tool) {
            end += 1;
        }
        let mut answered: HashSet<&str> = messages[index..end]
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|part| match part {
                HubPart::ToolResult { call_id, .. } => Some(call_id.as_str()),
                _ => None,
            })
            .collect();
        let synthetic: Vec<HubPart> = calls
            .iter()
            .filter(|(id, _)| answered.insert(id))
            .map(|(id, name)| interrupted_result(id, name))
            .collect();
        if !synthetic.is_empty() {
            if end > index {
                messages[index].content.extend(synthetic);
            } else {
                messages.insert(
                    index,
                    HubMessage {
                        role: HubRole::Tool,
                        content: synthetic,
                    },
                );
            }
        }
        if let Some(next) = messages.get_mut(index).filter(|m| m.role == HubRole::Tool) {
            sort_results_by_call_order(&mut next.content, &calls);
        }
    }
}

/// The `(id, name)` of every tool call in `message`, in order (assistant messages only).
fn calls_of(message: &HubMessage) -> Vec<(String, String)> {
    if message.role != HubRole::Assistant {
        return Vec::new();
    }
    message
        .content
        .iter()
        .filter_map(|part| match part {
            HubPart::ToolCall { id, name, .. } => Some((id.clone(), name.clone())),
            _ => None,
        })
        .collect()
}

fn interrupted_result(call_id: &str, name: &str) -> HubPart {
    HubPart::ToolResult {
        call_id: call_id.to_owned(),
        name: Some(name.to_owned()),
        content: vec![HubPart::text(INTERRUPTED_RESULT)],
        is_error: true,
    }
}

/// Reorders the `ToolResult` parts of `content` to follow `calls`, leaving other parts in place.
fn sort_results_by_call_order(content: &mut [HubPart], calls: &[(String, String)]) {
    let slots: Vec<usize> = content
        .iter()
        .enumerate()
        .filter(|(_, part)| matches!(part, HubPart::ToolResult { .. }))
        .map(|(slot, _)| slot)
        .collect();
    let mut results: Vec<HubPart> = slots
        .iter()
        .map(|&slot| std::mem::replace(&mut content[slot], HubPart::text("")))
        .collect();
    results.sort_by_key(|part| match part {
        HubPart::ToolResult { call_id, .. } => calls
            .iter()
            .position(|(id, _)| id == call_id)
            .unwrap_or(usize::MAX),
        _ => usize::MAX,
    });
    for (slot, part) in slots.into_iter().zip(results) {
        content[slot] = part;
    }
}

// ---------------------------------------------------------------------------------------------
// Pruning
// ---------------------------------------------------------------------------------------------
