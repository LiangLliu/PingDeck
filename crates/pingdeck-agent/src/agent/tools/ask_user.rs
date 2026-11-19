//! `ask_user`: the model stops at a real fork and asks the user to pick one of a few options
//!
//! Single responsibility: the tool's spec, its arguments, the gate card built from them, the check
//! of an answer against the card and the text the model reads back. Offered only to a chat's own
//! agent; a sub-agent puts its question in its report. The run host opens a `Question` gate with
//! [`question_card`] and waits ([`super::super::gate`]). The answer is information for the model
//! and approves nothing: a later file write still asks for approval.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use pingdeck_base::error::AppError;

use super::super::model::{
    AgentQuestionAnswer, HumanGateBody, HumanGateDto, HumanGateKind, QuestionOption,
};
use super::{
    AgentTool, CallInspection, ToolClass, ToolContext, ToolOutput, ToolRegistry, ToolSource,
    ToolSpec,
};

/// The tool's function name (a persisted identifier: never rename it).
pub const ASK_USER: &str = "ask_user";
/// Longest question, in characters.
const MAX_QUESTION_CHARS: usize = 500;
/// Longest option label, in characters.
const MAX_LABEL_CHARS: usize = 80;
/// Longest option detail, in characters.
const MAX_DETAIL_CHARS: usize = 200;
/// Fewest and most options the model may offer (the UI adds "Other").
const OPTIONS: std::ops::RangeInclusive<usize> = 2..=4;
/// Longest "Other" answer, in characters.
const MAX_OTHER_CHARS: usize = 2_000;

/// First line of every answer the model reads back.
pub const NOT_AN_APPROVAL: &str = "This is the user's answer, not an approval of any action.";
/// The tool result when nobody answered in time.
pub const ASK_TIMED_OUT_RESULT: &str = "The user did not answer.";

#[derive(Debug, Deserialize)]
struct Args {
    question: String,
    options: Vec<QuestionOption>,
    #[serde(default)]
    multi: bool,
}

fn too_long(what: &str, text: &str, max: usize) -> Result<(), AppError> {
    if text.chars().count() > max {
        return Err(AppError::validation(format!(
            "{what} is longer than {max} characters"
        )));
    }
    Ok(())
}

/// The question, its options and whether several may be chosen, checked and trimmed.
///
/// # Errors
/// [`AppError::Validation`] for an empty or too long question or option, or 0–1 or more than 4
/// options.
pub fn parse(arguments: &Value) -> Result<(String, Vec<QuestionOption>, bool), AppError> {
    let args: Args = serde_json::from_value(arguments.clone())
        .map_err(|error| AppError::validation(format!("Invalid arguments: {error}")))?;
    let question = args.question.trim().to_owned();
    if question.is_empty() {
        return Err(AppError::validation("question is empty"));
    }
    too_long("question", &question, MAX_QUESTION_CHARS)?;
    if !OPTIONS.contains(&args.options.len()) {
        return Err(AppError::validation(format!(
            "give {} to {} options",
            OPTIONS.start(),
            OPTIONS.end()
        )));
    }
    let mut options = Vec::with_capacity(args.options.len());
    for option in args.options {
        let label = option.label.trim().to_owned();
        if label.is_empty() {
            return Err(AppError::validation("an option label is empty"));
        }
        too_long("an option label", &label, MAX_LABEL_CHARS)?;
        let detail = option
            .detail
            .map(|detail| detail.trim().to_owned())
            .filter(|detail| !detail.is_empty());
        if let Some(detail) = &detail {
            too_long("an option detail", detail, MAX_DETAIL_CHARS)?;
        }
        options.push(QuestionOption { label, detail });
    }
    Ok((question, options, args.multi))
}

/// The card of the `Question` gate opened by call `call_id` of `run_id`.
#[must_use]
pub fn question_card(
    run_id: &str,
    call_id: &str,
    (question, options, multi): (String, Vec<QuestionOption>, bool),
    now_ms: i64,
) -> HumanGateDto {
    HumanGateDto {
        id: call_id.to_owned(),
        run_id: run_id.to_owned(),
        call_id: call_id.to_owned(),
        kind: HumanGateKind::Question,
        created_at_ms: now_ms,
        body: HumanGateBody::Question {
            question,
            options,
            multi,
        },
    }
}

/// Checks `answer` against the card: options numbered 1..=n, no repeats, one pick unless `multi`
/// (an "Other" text counts as a pick), something chosen unless skipped.
///
/// # Errors
/// [`AppError::Validation`] naming what is wrong; the question keeps waiting.
pub fn check_answer(card: &HumanGateDto, answer: &AgentQuestionAnswer) -> Result<(), AppError> {
    let HumanGateBody::Question { options, multi, .. } = &card.body else {
        return Err(AppError::validation("this gate is not a question"));
    };
    if answer.skipped {
        return Ok(());
    }
    let mut seen = Vec::with_capacity(answer.choices.len());
    for &choice in &answer.choices {
        if choice == 0 || usize::from(choice) > options.len() {
            return Err(AppError::validation(format!(
                "option {choice} does not exist"
            )));
        }
        if seen.contains(&choice) {
            return Err(AppError::validation(format!("option {choice} is repeated")));
        }
        seen.push(choice);
    }
    let other = answer.other.as_deref().map(str::trim).unwrap_or_default();
    too_long("the answer", other, MAX_OTHER_CHARS)?;
    let picks = seen.len() + usize::from(!other.is_empty());
    if picks == 0 {
        return Err(AppError::validation("choose an option or write an answer"));
    }
    if !multi && picks > 1 {
        return Err(AppError::validation(
            "this question takes one option or one answer",
        ));
    }
    Ok(())
}

/// How a question ended, for [`result_text`].
#[derive(Debug, Clone, Copy)]
pub enum QuestionEnd<'a> {
    /// The user answered (or skipped).
    Answered(&'a AgentQuestionAnswer),
    /// The user wrote a message instead; it follows in the history.
    Wrote,
    /// Nobody answered in time.
    TimedOut,
}

/// What the model reads back: an answer always starts with [`NOT_AN_APPROVAL`].
#[must_use]
pub fn result_text(options: &[QuestionOption], end: QuestionEnd<'_>) -> String {
    let answer = match end {
        QuestionEnd::TimedOut => return ASK_TIMED_OUT_RESULT.to_owned(),
        QuestionEnd::Wrote => {
            return format!(
                "{NOT_AN_APPROVAL}\nThe user did not pick an option; their next message is the answer."
            );
        }
        QuestionEnd::Answered(answer) => answer,
    };
    if answer.skipped {
        return format!(
            "{NOT_AN_APPROVAL}\nThe user skipped the question. Decide yourself, or say what you need."
        );
    }
    let mut lines = vec![NOT_AN_APPROVAL.to_owned()];
    let chosen: Vec<String> = answer
        .choices
        .iter()
        .filter_map(|&n| {
            let option = options.get(usize::from(n).checked_sub(1)?)?;
            Some(format!("({n}) {}", option.label))
        })
        .collect();
    if !chosen.is_empty() {
        lines.push(format!("User chose: {}", chosen.join("; ")));
    }
    if let Some(other) = answer.other.as_deref().map(str::trim)
        && !other.is_empty()
    {
        lines.push(format!("User answered: \"{other}\""));
    }
    lines.join("\n")
}

/// `ask_user`; the run host carries out the call (it waits on the user).
pub struct AskUserTool {
    spec: ToolSpec,
}

/// Registers `ask_user` (a chat's own agent only).
pub fn register(registry: &mut ToolRegistry) {
    registry.register(Arc::new(AskUserTool {
        spec: ToolSpec {
            name: ASK_USER.to_owned(),
            description: "Ask the user to choose between 2 to 4 options when you cannot go on without their choice and a wrong guess is costly. The user may also write their own answer. The answer is information, not an approval of any action.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "question": {"type": "string", "maxLength": MAX_QUESTION_CHARS, "description": "One short question."},
                    "options": {
                        "type": "array",
                        "minItems": OPTIONS.start(),
                        "maxItems": OPTIONS.end(),
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": {"type": "string", "maxLength": MAX_LABEL_CHARS},
                                "detail": {"type": "string", "maxLength": MAX_DETAIL_CHARS, "description": "One line about the option."}
                            },
                            "required": ["label"]
                        }
                    },
                    "multi": {"type": "boolean", "description": "Several options may be chosen."}
                },
                "required": ["question", "options"]
            }),
            read_only: true,
            destructive: false,
            source: ToolSource::Builtin,
            class: ToolClass::Internal,
            parallel_safe: false,
        },
    }));
}

impl AgentTool for AskUserTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        _ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        parse(arguments)?;
        Ok(CallInspection {
            read_only: true,
            ..CallInspection::default()
        })
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ToolContext<'a>,
        _arguments: Value,
    ) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async { ToolOutput::error("ask_user is only available to the chat's own agent.") })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn card() -> HumanGateDto {
        let parsed = parse(&json!({
            "question": " Which layer? ",
            "options": [{"label": "Router"}, {"label": "Gateway", "detail": "queue there"}, {"label": "Both"}]
        }))
        .unwrap();
        question_card("r1", "c1", parsed, 1)
    }

    #[test]
    fn an_answer_reads_back_as_information_with_the_chosen_option() {
        let card = card();
        let HumanGateBody::Question {
            options,
            multi,
            question,
        } = &card.body
        else {
            panic!("not a question");
        };
        assert_eq!((question.as_str(), *multi), ("Which layer?", false));
        let answer = AgentQuestionAnswer {
            choices: vec![2],
            ..AgentQuestionAnswer::default()
        };
        check_answer(&card, &answer).unwrap();
        assert_eq!(
            result_text(options, QuestionEnd::Answered(&answer)),
            format!("{NOT_AN_APPROVAL}\nUser chose: (2) Gateway")
        );
        let other = AgentQuestionAnswer {
            other: Some("only the docs".into()),
            ..AgentQuestionAnswer::default()
        };
        assert!(
            result_text(options, QuestionEnd::Answered(&other))
                .ends_with("User answered: \"only the docs\"")
        );
        assert!(result_text(options, QuestionEnd::Wrote).starts_with(NOT_AN_APPROVAL));
        assert_eq!(
            result_text(options, QuestionEnd::TimedOut),
            ASK_TIMED_OUT_RESULT
        );
    }

    #[test]
    fn bad_questions_and_answers_are_refused() {
        let one = json!({"question": "q", "options": [{"label": "a"}]});
        assert!(parse(&one).is_err());
        let card = card();
        let bad = |choices: Vec<u8>, other: Option<&str>| AgentQuestionAnswer {
            choices,
            other: other.map(str::to_owned),
            skipped: false,
        };
        for answer in [
            bad(vec![], None),
            bad(vec![4], None),
            bad(vec![0], None),
            bad(vec![1, 2], None),
            bad(vec![1], Some("and this")),
        ] {
            assert!(check_answer(&card, &answer).is_err(), "{answer:?}");
        }
        let skipped = AgentQuestionAnswer {
            skipped: true,
            ..AgentQuestionAnswer::default()
        };
        assert!(check_answer(&card, &skipped).is_ok());
    }
}
