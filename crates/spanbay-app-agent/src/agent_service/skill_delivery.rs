//! Skill texts in the model's history: a user message with attached skills reaches the
//! model with their `<skill>` blocks in front, and a compaction summary carries the skills its
//! range used, so they are attached again after it.

use serde_json::Value;

use crate::agent::model::{AgentMessage, AgentRole};
use crate::agent::skills::{SkillInfo, render_skill_blocks, skill_bodies};
use crate::llm::hub::HubPart;

/// The skills one history message used: attached ones and those `read_skill` loaded.
pub(super) type UsedSkills = (String, Vec<String>);

fn read_skill_names(parts: &[HubPart]) -> impl Iterator<Item = String> + '_ {
    parts.iter().filter_map(|part| match part {
        HubPart::ToolCall {
            name, arguments, ..
        } if name == "read_skill" => serde_json::from_str::<Value>(arguments)
            .ok()?
            .get("name")?
            .as_str()
            .map(str::to_owned),
        _ => None,
    })
}

/// Adds the skill blocks to the user messages and summaries of `messages` that name skills, and
/// returns the skills each message used, in order. Blocking (reads `SKILL.md` files).
pub(super) fn attach_skills(
    messages: &mut [AgentMessage],
    available: &[SkillInfo],
) -> Vec<UsedSkills> {
    messages
        .iter_mut()
        .map(|message| {
            let attached = message.meta.skills.clone();
            if !attached.is_empty() && matches!(message.role, AgentRole::User | AgentRole::Summary)
            {
                let (blocks, _) = render_skill_blocks(&skill_bodies(available, &attached));
                let part = HubPart::text(blocks);
                if message.role == AgentRole::User {
                    message.parts.insert(0, part);
                } else {
                    message.parts.push(part);
                }
            }
            let mut used = attached;
            for name in read_skill_names(&message.parts) {
                if !used.contains(&name) {
                    used.push(name);
                }
            }
            (message.id.clone(), used)
        })
        .collect()
}

/// The skills used by the messages up to and including `through`, each once: what a summary
/// replacing them attaches again.
pub(super) fn used_through(used: &[UsedSkills], through: &str) -> Vec<String> {
    let end = used
        .iter()
        .position(|(id, _)| id == through)
        .map_or(0, |index| index + 1);
    let mut names = Vec::new();
    for name in used[..end].iter().flat_map(|(_, names)| names) {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
}
