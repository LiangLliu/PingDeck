//! The copyable blocks of the snippet-only clients and the five code samples.
//!
//! Single responsibility: [`llm_client_snippets_impl`] and [`llm_client_code_samples_impl`], the
//! shell-facing calls over `client_config::snippets`, so the shapes live in one place. The
//! answer holds the token *placeholder* only: the one component that holds a freshly revealed
//! token splices it in. The core's "Note" and "Warning" blocks become notes with a stable code
//! the UI can translate; the Zed `max_tokens` comes from the context window of the alias's first
//! enabled target.

use serde::{Deserialize, Serialize};

use super::with_connection;
use crate::app::AppState;
use crate::client_config::{
    self, NOTE_TITLE, SnippetBlock, SnippetClient, SnippetRequest, TOKEN_PLACEHOLDER, WARNING_TITLE,
};
use crate::error::AppError;
use crate::llm::store;

/// How loudly a note speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnippetTone {
    /// Something to know.
    Info,
    /// Something that may stop it from working.
    Warn,
}

/// A line of advice above the blocks of a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnippetNote {
    /// How loudly it speaks.
    pub tone: SnippetTone,
    /// A stable code (`generic_docker`, `zed_jsonc`, `cursor_public_https`, `token_placeholder`)
    /// the UI maps to its own words.
    pub code: String,
    /// The note in English, for a UI that has no words for `code`.
    pub text: String,
}

/// One copyable block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnippetBlockView {
    /// A stable id derived from the title (`settings-json`, `steps`, `openai-python`).
    pub id: String,
    /// Heading above the block.
    pub title: String,
    /// Syntax name for highlighting (`json`, `bash`, `python`, `javascript`, `text`).
    pub language: String,
    /// The text the copy button puts on the clipboard, with the token placeholder in it.
    pub text: String,
}

/// The notes and blocks of one client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnippetSet {
    /// Advice above the blocks.
    pub notes: Vec<SnippetNote>,
    /// What to copy.
    pub blocks: Vec<SnippetBlockView>,
}

fn id_of(title: &str) -> String {
    let mut id = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            id.push(c.to_ascii_lowercase());
        } else if !id.ends_with('-') && !id.is_empty() {
            id.push('-');
        }
    }
    id.trim_end_matches('-').to_owned()
}

fn view(block: SnippetBlock) -> SnippetBlockView {
    SnippetBlockView {
        id: id_of(&block.title),
        title: block.title,
        language: block.language,
        text: block.text,
    }
}

/// The context window of the model `reference` names (a model reference or a legacy alias
/// name), when its stored row has one.
fn model_context(state: &AppState, reference: &str) -> Result<Option<u64>, AppError> {
    let snapshot = state.llm.snapshot_blocking()?;
    let Some(model) = snapshot.resolve_ref(reference) else {
        return Ok(None);
    };
    with_connection(state, |connection| {
        Ok(
            store::get_model(connection, &model.provider_id, &model.model)?
                .and_then(|model| model.context),
        )
    })
}

/// The notes and blocks of one snippet client for `alias` at `address` (a gateway root such as
/// `http://127.0.0.1:4141`, validated). `token_revealed` says the caller will splice a real token
/// into the placeholder; without it a note explains what the placeholder stands for.
///
/// # Errors
/// [`AppError::Validation`] for a bad address or alias.
pub fn llm_client_snippets_impl(
    state: &AppState,
    client: SnippetClient,
    address: &str,
    alias: &str,
    token_revealed: bool,
) -> Result<SnippetSet, AppError> {
    let request = SnippetRequest {
        address,
        alias,
        token: None,
        max_tokens: model_context(state, alias.trim())?.filter(|context| *context > 0),
    };
    let mut set = SnippetSet {
        notes: Vec::new(),
        blocks: Vec::new(),
    };
    for block in client_config::client_snippets(client, &request)? {
        match (client, block.title.as_str()) {
            (SnippetClient::GenericEnv, NOTE_TITLE) => set.notes.push(SnippetNote {
                tone: SnippetTone::Info,
                code: "generic_docker".to_owned(),
                text: block.text,
            }),
            (SnippetClient::Cursor, WARNING_TITLE) => set.notes.push(SnippetNote {
                tone: SnippetTone::Warn,
                code: "cursor_public_https".to_owned(),
                text: block.text,
            }),
            _ => set.blocks.push(view(block)),
        }
    }
    if client == SnippetClient::Zed {
        set.notes.push(SnippetNote {
            tone: SnippetTone::Info,
            code: "zed_jsonc".to_owned(),
            text: "Zed's settings.json is JSONC, so PingDeck never writes it: merge this block \
                   by hand."
                .to_owned(),
        });
    }
    if !token_revealed {
        set.notes.push(SnippetNote {
            tone: SnippetTone::Info,
            code: "token_placeholder".to_owned(),
            text: format!(
                "{TOKEN_PLACEHOLDER} stands for a client token. Create or rotate a client to \
                 reveal one."
            ),
        });
    }
    Ok(set)
}

/// The five code samples (curl, OpenAI Python, OpenAI Node, Anthropic Python, Gemini curl) for
/// `alias` at `address`, with the token placeholder.
///
/// # Errors
/// [`AppError::Validation`] for a bad address or alias.
pub fn llm_client_code_samples_impl(
    address: &str,
    alias: &str,
) -> Result<Vec<SnippetBlockView>, AppError> {
    let request = SnippetRequest {
        address,
        alias,
        token: None,
        // The code samples take no context window.
        max_tokens: None,
    };
    Ok(client_config::code_samples(&request)?
        .into_iter()
        .map(view)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uc_support::Fx;

    #[test]
    fn ids_are_the_lowercase_title_with_dashes() {
        assert_eq!(id_of("settings.json"), "settings-json");
        assert_eq!(id_of("OpenAI Python"), "openai-python");
        assert_eq!(id_of("Gemini curl"), "gemini-curl");
        assert_eq!(id_of("Steps"), "steps");
    }

    #[test]
    fn notes_split_off_the_core_note_and_warning_blocks() {
        let fx = Fx::new();
        let address = "http://127.0.0.1:4141";
        let cursor =
            llm_client_snippets_impl(&fx.state, SnippetClient::Cursor, address, "fast", true)
                .unwrap();
        assert_eq!(cursor.notes.len(), 1, "revealed: no placeholder note");
        assert_eq!(
            (cursor.notes[0].tone, cursor.notes[0].code.as_str()),
            (SnippetTone::Warn, "cursor_public_https")
        );
        assert_eq!(cursor.blocks.len(), 1);
        assert_eq!(cursor.blocks[0].id, "steps");

        let generic =
            llm_client_snippets_impl(&fx.state, SnippetClient::GenericEnv, address, "fast", false)
                .unwrap();
        let codes: Vec<&str> = generic.notes.iter().map(|n| n.code.as_str()).collect();
        assert_eq!(codes, ["generic_docker", "token_placeholder"]);
        assert_eq!(generic.blocks.len(), 2);
        assert!(generic.blocks[0].text.contains(TOKEN_PLACEHOLDER));

        let zed =
            llm_client_snippets_impl(&fx.state, SnippetClient::Zed, address, "fast", true).unwrap();
        assert_eq!(zed.notes[0].code, "zed_jsonc");
        assert_eq!(zed.blocks[0].id, "settings-json");
    }

    #[test]
    fn zed_max_tokens_is_the_context_of_the_aliases_first_enabled_target() {
        let fx = Fx::new();
        let wired = fx.wire_without_upstream();
        let mut model = crate::llm_model_list_impl(&fx.state, Some(&wired.provider_id))
            .unwrap()
            .into_iter()
            .find(|model| model.model_id == "m")
            .unwrap();
        model.context = Some(200_000);
        crate::llm_model_save_impl(&fx.state, model).unwrap();
        let text = |alias: &str| {
            llm_client_snippets_impl(
                &fx.state,
                SnippetClient::Zed,
                "http://127.0.0.1:4141",
                alias,
                true,
            )
            .unwrap()
            .blocks[0]
                .text
                .clone()
        };
        assert!(text("smart").contains("\"max_tokens\": 200000"));
        assert!(text("other").contains("\"max_tokens\": 128000"));
    }

    #[test]
    fn code_samples_delegate_to_the_config_generator() {
        let samples = llm_client_code_samples_impl("http://127.0.0.1:4141", "fast").unwrap();
        assert_eq!(samples[0].id, "curl");
        assert!(samples[0].text.contains(TOKEN_PLACEHOLDER));
        assert!(llm_client_code_samples_impl("nope", "fast").is_err());
    }
}
