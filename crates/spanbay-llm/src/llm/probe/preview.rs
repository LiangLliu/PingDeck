//! The first HTTP request a check sends, rebuilt without sending it ("Copy as curl", "Open in API
//! client").
//!
//! Single responsibility: call the same request builders the checks call, so a preview can never
//! drift from what a run sends. Probe bodies are not stored (the prompts are fixed and public);
//! the use-case layer adds headers and a key placeholder. Nothing here holds a key.

use serde_json::Value;

use super::checks::{
    cache_request, chat_request, json_schema_request, reasoning_request, tools_request,
    vision_request,
};
use super::free::auth_request;
use super::http::list_protocol;
use super::misc::embeddings_request;
use super::native::{pdf_request, search_request};
use super::stream::stream_request;
use super::{ProbeOptions, ProbeTest};
use crate::llm::client::encode_for;
use crate::llm::hub::HubRequest;
use crate::llm::router::Target;
use crate::llm::wire::{Protocol, models_url};

/// One request of a check, without credentials.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewRequest {
    /// Protocol of the call (the protocol whose headers authenticate it).
    pub protocol: Protocol,
    /// Whether the response is an event stream.
    pub stream: bool,
    /// The request URL. A GET when `body` is `Null`, a POST otherwise.
    pub url: String,
    /// The JSON body; `Null` for the GET checks.
    pub body: Value,
}

/// A chat-family POST: the request encoded for the target's chat protocol.
fn chat_post(
    target: &Target,
    options: &ProbeOptions,
    request: &HubRequest,
    stream: bool,
) -> Option<PreviewRequest> {
    let protocol = target.chat_protocol(options.protocol)?;
    Some(PreviewRequest {
        protocol,
        stream,
        url: target.url(protocol, stream).ok()?,
        body: encode_for(target, protocol, request, stream).ok()?,
    })
}

/// The first HTTP request `test` sends to `target`; `None` when the check sends nothing
/// (Context and Decision are not previewed), the provider cannot take it (no chat protocol, no
/// embeddings protocol, no model list endpoint for Models) or the request cannot be built.
///
/// Connectivity, Auth and Models are GETs (`body` is `Null`); an Auth check on a provider
/// without a model list is its one-token chat.
#[must_use]
pub fn request_preview(
    target: &Target,
    test: ProbeTest,
    options: &ProbeOptions,
) -> Option<PreviewRequest> {
    let model = target.model.as_str();
    let models_url = || {
        let endpoint = options.models_endpoint.as_deref()?.trim();
        (!endpoint.is_empty()).then(|| models_url(&target.base_url, endpoint))
    };
    let get = |url: String| PreviewRequest {
        protocol: list_protocol(target),
        stream: false,
        url,
        body: Value::Null,
    };
    match test {
        ProbeTest::Connectivity => Some(get(target.base_url.trim().to_owned())),
        ProbeTest::Models => models_url().map(get),
        ProbeTest::Auth => match models_url() {
            Some(url) => Some(get(url)),
            None => chat_post(target, options, &auth_request(model), false),
        },
        ProbeTest::Chat => chat_post(target, options, &chat_request(model, options), false),
        ProbeTest::Stream => chat_post(target, options, &stream_request(model), true),
        ProbeTest::Tools => chat_post(target, options, &tools_request(model), false),
        ProbeTest::JsonSchema => chat_post(target, options, &json_schema_request(model), false),
        ProbeTest::Vision => chat_post(target, options, &vision_request(model), false),
        ProbeTest::Reasoning => chat_post(target, options, &reasoning_request(model), false),
        ProbeTest::Cache => chat_post(target, options, &cache_request(model), false),
        ProbeTest::Search => chat_post(target, options, &search_request(model), false),
        ProbeTest::Pdf => chat_post(target, options, &pdf_request(model), false),
        ProbeTest::Embeddings => {
            let request = embeddings_request(target).ok()?;
            let url = match request.url {
                Some(url) => url,
                None => target.url(request.protocol, false).ok()?,
            };
            Some(PreviewRequest {
                protocol: request.protocol,
                stream: false,
                url,
                body: request.body,
            })
        }
        ProbeTest::Decision | ProbeTest::Context => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::probe::test_support::{OPENAI, openai_fake, opts, spawn_fake, target};

    const ANTHROPIC: &[Protocol] = &[Protocol::AnthropicMessages];

    #[tokio::test]
    async fn every_previewed_request_is_exactly_what_the_check_sends() {
        let fake = spawn_fake(openai_fake).await;
        let target = target(fake.addr, OPENAI);
        let options = opts();
        let runtime = crate::llm::probe::test_support::runtime();
        for test in [
            ProbeTest::Chat,
            ProbeTest::Stream,
            ProbeTest::Tools,
            ProbeTest::JsonSchema,
            ProbeTest::Vision,
            ProbeTest::Reasoning,
            ProbeTest::Cache,
            ProbeTest::Connectivity,
            ProbeTest::Models,
            ProbeTest::Auth,
        ] {
            let before = fake.requests().len();
            let preview = request_preview(&target, test, &options).unwrap();
            crate::llm::probe::run_test(&runtime, &target, test, &options, None).await;
            let sent = &fake.requests()[before];
            // The base URL has no path: the request line says "/".
            let path = preview.url.strip_prefix(&target.base_url).unwrap();
            assert_eq!(
                if path.is_empty() { "/" } else { path },
                sent.path,
                "{test:?}"
            );
            if preview.body == Value::Null {
                assert_eq!(sent.method, "GET", "{test:?}");
            } else {
                let body: Value = serde_json::from_str(&sent.body).unwrap();
                assert_eq!(preview.body, body, "{test:?}");
            }
        }
    }

    #[test]
    fn stream_and_chat_differ_in_cap_and_flag() {
        let target = target("127.0.0.1:1".parse().unwrap(), OPENAI);
        let chat = request_preview(&target, ProbeTest::Chat, &opts()).unwrap();
        let stream = request_preview(&target, ProbeTest::Stream, &opts()).unwrap();
        assert!(!chat.stream && stream.stream);
        assert_eq!(chat.protocol, Protocol::OpenaiChat);
        let cap = |body: &Value| {
            body["max_completion_tokens"]
                .as_u64()
                .or_else(|| body["max_tokens"].as_u64())
        };
        assert_eq!(cap(&chat.body), Some(256));
        assert_eq!(cap(&stream.body), Some(512));
        assert!(
            chat.body
                .to_string()
                .contains("Reply with the single word: pong")
        );
    }

    #[test]
    fn free_checks_are_gets_and_auth_without_a_list_is_a_chat() {
        let target = target("127.0.0.1:1".parse().unwrap(), OPENAI);
        let base = request_preview(&target, ProbeTest::Connectivity, &opts()).unwrap();
        assert_eq!(
            (base.url.as_str(), &base.body),
            ("http://127.0.0.1:1", &Value::Null)
        );
        let models = request_preview(&target, ProbeTest::Models, &opts()).unwrap();
        assert_eq!(models.url, "http://127.0.0.1:1/models");
        let mut without = opts();
        without.models_endpoint = None;
        assert_eq!(request_preview(&target, ProbeTest::Models, &without), None);
        let auth = request_preview(&target, ProbeTest::Auth, &without).unwrap();
        assert!(auth.url.ends_with("/chat/completions") && auth.body != Value::Null);
        assert_eq!(auth.body["messages"][0]["content"], "hi");
    }

    #[test]
    fn checks_that_send_nothing_or_cannot_be_taken_have_no_preview() {
        let chat_only = target("127.0.0.1:1".parse().unwrap(), OPENAI);
        for test in [
            ProbeTest::Context,
            ProbeTest::Decision,
            ProbeTest::Embeddings,
        ] {
            assert_eq!(request_preview(&chat_only, test, &opts()), None, "{test:?}");
        }
        let embeddings_only = target("127.0.0.1:1".parse().unwrap(), &[Protocol::Embeddings]);
        assert_eq!(
            request_preview(&embeddings_only, ProbeTest::Chat, &opts()),
            None
        );
        let embed = request_preview(&embeddings_only, ProbeTest::Embeddings, &opts()).unwrap();
        assert_eq!(embed.protocol, Protocol::Embeddings);
        assert!(embed.url.ends_with("/v1/embeddings"), "{}", embed.url);
        assert_eq!(embed.body["input"], "SpanBay embedding test");
    }

    #[test]
    fn gemini_embeddings_use_the_embed_content_url() {
        let target = target("127.0.0.1:1".parse().unwrap(), &[Protocol::Gemini]);
        let embed = request_preview(&target, ProbeTest::Embeddings, &opts()).unwrap();
        assert_eq!(embed.protocol, Protocol::Gemini);
        assert!(embed.url.ends_with(":embedContent"), "{}", embed.url);
    }

    #[test]
    fn search_and_pdf_carry_their_native_parts_on_messages_providers() {
        let target = target("127.0.0.1:1".parse().unwrap(), ANTHROPIC);
        let search = request_preview(&target, ProbeTest::Search, &opts()).unwrap();
        assert!(search.body.to_string().contains("web_search"));
        let pdf = request_preview(&target, ProbeTest::Pdf, &opts()).unwrap();
        assert_eq!(pdf.body["messages"][0]["content"][0]["type"], "document");
        assert_eq!(pdf.protocol, Protocol::AnthropicMessages);
    }

    #[test]
    fn no_secret_is_in_any_preview() {
        let target = target("127.0.0.1:1".parse().unwrap(), ANTHROPIC);
        for test in ProbeTest::ALL {
            if let Some(preview) = request_preview(&target, test, &opts()) {
                let text = format!("{preview:?}");
                assert!(!text.contains(&target.secret), "{test:?}");
            }
        }
    }
}
