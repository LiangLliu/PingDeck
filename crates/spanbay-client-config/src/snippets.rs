//! Snippet-only clients: Cursor, Zed, Cherry Studio, a generic environment block and the five
//! code samples. Nothing here touches a file; blocks that need a token carry
//! [`TOKEN_PLACEHOLDER`] unless the caller passes the real token for the one-time reveal.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{check_gateway_url, check_token};
use spanbay_base::error::AppError;

/// What snippets show in place of a token after the one-time reveal.
pub const TOKEN_PLACEHOLDER: &str = "YOUR_SPANBAY_TOKEN";
/// Zed's `max_tokens` when the alias's first target has no known context window.
pub const DEFAULT_MAX_TOKENS: u64 = 128_000;
/// Title of the generic block's informational note; the app layer turns a block with this title
/// into a note instead of a copyable block.
pub const NOTE_TITLE: &str = "Note";
/// Title of Cursor's leading warning; the app layer turns a block with this title into a warning
/// instead of a copyable block.
pub const WARNING_TITLE: &str = "Warning";

/// A client that is configured by hand from copyable blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnippetClient {
    /// Cursor (needs a public HTTPS address).
    Cursor,
    /// Zed (`settings.json` is JSONC, so it is never written).
    Zed,
    /// Cherry Studio.
    CherryStudio,
    /// A plain environment-variable block for any other tool.
    GenericEnv,
}

impl SnippetClient {
    /// Every snippet client, in display order.
    pub const ALL: [Self; 4] = [
        Self::Cursor,
        Self::Zed,
        Self::CherryStudio,
        Self::GenericEnv,
    ];

    /// The macOS app bundle that marks the client as installed.
    pub(super) const fn app_bundle(self) -> Option<&'static str> {
        match self {
            Self::Cursor => Some("Cursor.app"),
            Self::Zed => Some("Zed.app"),
            Self::CherryStudio => Some("Cherry Studio.app"),
            Self::GenericEnv => None,
        }
    }

    /// The config directory that marks the client as installed.
    pub(super) fn config_dir(self, home: &Path) -> Option<PathBuf> {
        match self {
            Self::Zed => Some(home.join(".config").join("zed")),
            Self::Cursor | Self::CherryStudio | Self::GenericEnv => None,
        }
    }
}

/// One copyable block of a snippet client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnippetBlock {
    /// Heading above the block.
    pub title: String,
    /// Syntax name for highlighting (`json`, `bash`, `python`, `javascript`, `text`).
    pub language: String,
    /// The text the copy button puts on the clipboard.
    pub text: String,
}

/// What to put in the blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnippetRequest<'a> {
    /// Gateway root, for example `http://127.0.0.1:4141` or a LAN address such as
    /// `http://192.168.1.20:4141` (file writers only ever use the loopback address).
    pub address: &'a str,
    /// Alias (or `provider/model`) the tool should call.
    pub alias: &'a str,
    /// The real token, passed only for the one-time reveal; `None` writes the placeholder.
    pub token: Option<&'a str>,
    /// Context window of the alias's first target, used by Zed; `None` uses
    /// `DEFAULT_MAX_TOKENS`. The code samples ignore it.
    pub max_tokens: Option<u64>,
}

/// The validated values every block is built from.
struct Values<'a> {
    gateway: &'a str,
    alias: &'a str,
    token: &'a str,
    max_tokens: u64,
}

impl<'a> Values<'a> {
    fn new(request: &SnippetRequest<'a>) -> Result<Self, AppError> {
        // The alias is checked before the gateway so its error wins when both are bad.
        let alias = check_alias(request.alias)?;
        Ok(Self {
            gateway: check_gateway_url(request.address)?,
            alias,
            token: match request.token {
                Some(token) => check_token(token)?,
                None => TOKEN_PLACEHOLDER,
            },
            max_tokens: request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        })
    }
}

/// An alias without surrounding blanks, safe to splice into the blocks.
///
/// # Errors
/// [`AppError::validation`] when it is empty or holds quotes, backslashes or control characters.
fn check_alias(alias: &str) -> Result<&str, AppError> {
    let alias = alias.trim();
    if alias.is_empty()
        || alias
            .chars()
            .any(|c| c.is_control() || matches!(c, '"' | '\'' | '\\'))
    {
        return Err(AppError::validation(
            "alias must be non-empty and contain no quotes, backslashes or control characters",
        ));
    }
    Ok(alias)
}

fn block(title: &str, language: &str, text: impl Into<String>) -> SnippetBlock {
    SnippetBlock {
        title: title.to_owned(),
        language: language.to_owned(),
        text: text.into(),
    }
}

/// The blocks of one snippet client.
///
/// # Errors
/// [`AppError::validation`] for a bad address, token or alias.
pub fn client_snippets(
    client: SnippetClient,
    request: &SnippetRequest<'_>,
) -> Result<Vec<SnippetBlock>, AppError> {
    let values = Values::new(request)?;
    Ok(match client {
        SnippetClient::GenericEnv => generic_env_blocks(&values),
        SnippetClient::Zed => zed_blocks(&values),
        SnippetClient::CherryStudio => cherry_studio_blocks(&values),
        SnippetClient::Cursor => cursor_blocks(&values),
    })
}

/// The environment block for any tool, the model name and the container note.
fn generic_env_blocks(v: &Values<'_>) -> Vec<SnippetBlock> {
    let Values { gateway, token, .. } = *v;
    vec![
        block(
            "Environment variables",
            "bash",
            format!(
                "OPENAI_BASE_URL={gateway}/v1\nOPENAI_API_KEY={token}\n\
                 ANTHROPIC_BASE_URL={gateway}\nANTHROPIC_AUTH_TOKEN={token}\n\
                 GOOGLE_GEMINI_BASE_URL={gateway}\nGEMINI_API_KEY={token}"
            ),
        ),
        block("Model name", "text", v.alias),
        block(
            NOTE_TITLE,
            "text",
            "From a container or devcontainer use `host.docker.internal` instead of \
             `127.0.0.1` and add it under Extra allowed hosts (Settings > AI > Gateway access).",
        ),
    ]
}

/// Zed's `language_models` provider block and its key variable.
fn zed_blocks(v: &Values<'_>) -> Vec<SnippetBlock> {
    let Values {
        gateway,
        alias,
        token,
        max_tokens,
    } = *v;
    let settings = json!({
        "language_models": { "openai_compatible": { "spanbay": {
            "api_url": format!("{gateway}/v1"),
            "available_models": [{
                "name": alias,
                "display_name": format!("SpanBay {alias}"),
                "max_tokens": max_tokens,
            }],
        } } }
    });
    vec![
        block(
            "settings.json",
            "json",
            // Serialising a `Value` built by `json!` cannot fail.
            serde_json::to_string_pretty(&settings).unwrap_or_default(),
        ),
        block(
            "Environment variable",
            "bash",
            format!("SPANBAY_API_KEY={token}"),
        ),
    ]
}

/// Cherry Studio's provider steps (the root address: it appends `/v1` itself).
fn cherry_studio_blocks(v: &Values<'_>) -> Vec<SnippetBlock> {
    let Values {
        gateway,
        alias,
        token,
        ..
    } = *v;
    vec![block(
        "Steps",
        "text",
        format!(
            "1. Settings > Model Provider > Add, type OpenAI.\n\
             2. API address: {gateway}\n\
             3. API key: {token}\n\
             4. Add model: {alias}"
        ),
    )]
}

/// Cursor's public-address warning and its provider steps.
fn cursor_blocks(v: &Values<'_>) -> Vec<SnippetBlock> {
    let Values {
        gateway,
        alias,
        token,
        ..
    } = *v;
    vec![
        block(
            WARNING_TITLE,
            "text",
            "Cursor sends model requests from its own servers, so 127.0.0.1 and local-network \
             addresses do not work. Cursor needs a public HTTPS address, which SpanBay does \
             not provide. A tunnel plus an allowed host (Settings > AI > Gateway access) can \
             reach the gateway: a token is still required, the gateway is then exposed to the \
             internet, and tunnels are outside SpanBay's support.",
        ),
        block(
            "Steps",
            "text",
            format!(
                "1. Settings > Models > OpenAI API Key: {token}\n\
                 2. Override OpenAI Base URL: {gateway}/v1\n\
                 3. Add model: {alias}"
            ),
        ),
    ]
}

/// The five code samples: curl, OpenAI Python, OpenAI Node, Anthropic Python, Gemini curl.
///
/// # Errors
/// [`AppError::validation`] for a bad address, token or alias.
pub fn code_samples(request: &SnippetRequest<'_>) -> Result<Vec<SnippetBlock>, AppError> {
    let Values {
        gateway,
        alias,
        token,
        ..
    } = Values::new(request)?;
    Ok(vec![
        block(
            "curl",
            "bash",
            format!(
                "curl {gateway}/v1/chat/completions \\\n  -H \"Authorization: Bearer {token}\" \\\n  \
                 -H \"Content-Type: application/json\" \\\n  \
                 -d '{{\"model\": \"{alias}\", \"messages\": [{{\"role\": \"user\", \"content\": \"Hello\"}}]}}'"
            ),
        ),
        block(
            "OpenAI Python",
            "python",
            format!(
                "from openai import OpenAI\n\n\
                 client = OpenAI(base_url=\"{gateway}/v1\", api_key=\"{token}\")\n\
                 reply = client.chat.completions.create(\n    model=\"{alias}\",\n    \
                 messages=[{{\"role\": \"user\", \"content\": \"Hello\"}}],\n)\n\
                 print(reply.choices[0].message.content)"
            ),
        ),
        block(
            "OpenAI Node",
            "javascript",
            format!(
                "import OpenAI from \"openai\";\n\n\
                 const client = new OpenAI({{ baseURL: \"{gateway}/v1\", apiKey: \"{token}\" }});\n\
                 const reply = await client.chat.completions.create({{\n  model: \"{alias}\",\n  \
                 messages: [{{ role: \"user\", content: \"Hello\" }}],\n}});\n\
                 console.log(reply.choices[0].message.content);"
            ),
        ),
        block(
            "Anthropic Python",
            "python",
            format!(
                "from anthropic import Anthropic\n\n\
                 client = Anthropic(base_url=\"{gateway}\", auth_token=\"{token}\")\n\
                 message = client.messages.create(\n    model=\"{alias}\",\n    max_tokens=1024,\n    \
                 messages=[{{\"role\": \"user\", \"content\": \"Hello\"}}],\n)\n\
                 print(message.content[0].text)"
            ),
        ),
        block(
            "Gemini curl",
            "bash",
            format!(
                "curl \"{gateway}/v1beta/models/{alias}:generateContent\" \\\n  \
                 -H \"x-goog-api-key: {token}\" \\\n  -H \"Content-Type: application/json\" \\\n  \
                 -d '{{\"contents\": [{{\"role\": \"user\", \"parts\": [{{\"text\": \"Hello\"}}]}}]}}'"
            ),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = "pd_zd_a1b2c3d4e5f60718293a4b5c6d7e9d2f";

    fn req(token: Option<&'static str>) -> SnippetRequest<'static> {
        SnippetRequest {
            address: "http://127.0.0.1:4141/",
            alias: "fast",
            token,
            max_tokens: None,
        }
    }

    fn all_text(blocks: &[SnippetBlock]) -> String {
        blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn every_snippet_uses_the_placeholder_and_never_a_token_shape() {
        for client in SnippetClient::ALL {
            let text = all_text(&client_snippets(client, &req(None)).unwrap());
            assert!(!text.contains("pd_") && !text.contains("sk-"), "{client:?}");
            assert!(
                text.contains("fast") && text.contains("http://127.0.0.1:4141"),
                "{client:?}"
            );
            assert!(text.contains(TOKEN_PLACEHOLDER), "{client:?}");
        }
        let samples = code_samples(&req(None)).unwrap();
        assert_eq!(samples.len(), 5);
        let text = all_text(&samples);
        assert!(!text.contains("pd_") && !text.contains("sk-"));
        assert_eq!(text.matches(TOKEN_PLACEHOLDER).count(), 5);
    }

    #[test]
    fn the_real_token_appears_only_when_passed() {
        for client in SnippetClient::ALL {
            let text = all_text(&client_snippets(client, &req(Some(REAL))).unwrap());
            assert!(
                text.contains(REAL) && !text.contains(TOKEN_PLACEHOLDER),
                "{client:?}"
            );
        }
        let text = all_text(&code_samples(&req(Some(REAL))).unwrap());
        assert_eq!(text.matches(REAL).count(), 5);
    }

    #[test]
    fn zed_block_has_the_provider_shape_and_the_env_variable() {
        let blocks = client_snippets(SnippetClient::Zed, &req(None)).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            (blocks[0].title.as_str(), blocks[0].language.as_str()),
            ("settings.json", "json")
        );
        let json: serde_json::Value = serde_json::from_str(&blocks[0].text).unwrap();
        let provider = &json["language_models"]["openai_compatible"]["spanbay"];
        assert_eq!(provider["api_url"], "http://127.0.0.1:4141/v1");
        let model = &provider["available_models"][0];
        assert_eq!(model["name"], "fast");
        assert_eq!(model["display_name"], "SpanBay fast");
        assert_eq!(model["max_tokens"], 128_000);
        assert_eq!(blocks[1].text, "SPANBAY_API_KEY=YOUR_SPANBAY_TOKEN");
    }

    #[test]
    fn zed_max_tokens_follows_the_given_context() {
        let mut request = req(None);
        request.max_tokens = Some(200_000);
        let blocks = client_snippets(SnippetClient::Zed, &request).unwrap();
        let json: serde_json::Value = serde_json::from_str(&blocks[0].text).unwrap();
        assert_eq!(
            json["language_models"]["openai_compatible"]["spanbay"]["available_models"][0]["max_tokens"],
            200_000
        );
    }

    #[test]
    fn cursor_leads_with_the_public_https_warning() {
        let blocks = client_snippets(SnippetClient::Cursor, &req(None)).unwrap();
        assert_eq!(blocks[0].title, "Warning");
        let warning = &blocks[0].text;
        assert!(
            warning
                .contains("Cursor needs a public HTTPS address, which SpanBay does not provide.")
        );
        assert!(warning.contains("A tunnel plus an allowed host (Settings > AI > Gateway access)"));
        assert!(warning.contains("tunnels are outside SpanBay's support"));
        assert!(
            blocks[1]
                .text
                .contains("Override OpenAI Base URL: http://127.0.0.1:4141/v1")
        );
        assert!(blocks[1].text.contains("Add model: fast"));
    }

    #[test]
    fn cherry_studio_steps_use_the_root_address() {
        let blocks = client_snippets(SnippetClient::CherryStudio, &req(None)).unwrap();
        assert_eq!(blocks.len(), 1);
        let text = &blocks[0].text;
        assert!(text.contains("API address: http://127.0.0.1:4141\n"));
        assert!(text.contains("API key: YOUR_SPANBAY_TOKEN"));
        assert!(text.contains("Add model: fast"));
    }

    #[test]
    fn generic_env_lists_the_three_protocol_pairs() {
        let blocks = client_snippets(SnippetClient::GenericEnv, &req(None)).unwrap();
        assert_eq!(
            blocks[0].text,
            "OPENAI_BASE_URL=http://127.0.0.1:4141/v1\nOPENAI_API_KEY=YOUR_SPANBAY_TOKEN\n\
             ANTHROPIC_BASE_URL=http://127.0.0.1:4141\nANTHROPIC_AUTH_TOKEN=YOUR_SPANBAY_TOKEN\n\
             GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:4141\nGEMINI_API_KEY=YOUR_SPANBAY_TOKEN"
        );
        assert_eq!(blocks[1].text, "fast");
        assert_eq!(blocks[2].title, "Note");
        assert!(blocks[2].text.contains("`host.docker.internal`"));
        assert!(blocks[2].text.contains("Extra allowed hosts"));
    }

    #[test]
    fn code_samples_keep_the_known_shapes() {
        let samples = code_samples(&req(None)).unwrap();
        let titles: Vec<_> = samples.iter().map(|b| b.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "curl",
                "OpenAI Python",
                "OpenAI Node",
                "Anthropic Python",
                "Gemini curl"
            ]
        );
        assert!(
            samples[0]
                .text
                .starts_with("curl http://127.0.0.1:4141/v1/chat/completions \\\n")
        );
        assert!(samples[0].text.contains("\"model\": \"fast\""));
        assert!(
            samples[1]
                .text
                .contains("OpenAI(base_url=\"http://127.0.0.1:4141/v1\"")
        );
        assert!(
            samples[2]
                .text
                .contains("baseURL: \"http://127.0.0.1:4141/v1\"")
        );
        assert!(
            samples[3]
                .text
                .contains("Anthropic(base_url=\"http://127.0.0.1:4141\"")
        );
        assert!(
            samples[4]
                .text
                .contains("/v1beta/models/fast:generateContent")
        );
    }

    #[test]
    fn bad_input_is_rejected() {
        let mut request = req(None);
        request.address = "not a url";
        assert!(code_samples(&request).is_err());
        let mut request = req(Some("a b"));
        assert!(client_snippets(SnippetClient::Zed, &request).is_err());
        request.token = None;
        request.alias = "a\"b";
        assert!(client_snippets(SnippetClient::Zed, &request).is_err());
        request.alias = " ";
        assert!(client_snippets(SnippetClient::Zed, &request).is_err());
    }

    #[test]
    fn a_bad_alias_wins_over_a_bad_address() {
        let mut request = req(None);
        request.address = "not a url";
        request.alias = " ";
        let alias_error =
            "alias must be non-empty and contain no quotes, backslashes or control characters";
        assert_eq!(code_samples(&request).unwrap_err().message(), alias_error);
        assert_eq!(
            client_snippets(SnippetClient::Zed, &request)
                .unwrap_err()
                .message(),
            alias_error
        );
    }
}
