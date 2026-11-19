//! The model-list requests of a scan: the planned attempts in order until one lists models,
//! then the continuation pages of that list (at most [`MAX_PAGES`]).

use serde_json::Value;

use super::{
    auth::AuthFamily,
    exec::{Call, MODELS_BODY_CAP, Purpose, Sent, Spend, TIER_MODELS},
    models::{ListCursor, MAX_PAGES, ParsedPage, parse_page, parse_truncated},
    plan::{ModelAttempt, ModelSource},
    scan::{FirstModelsRequest, Scan},
    types::SniffWarning,
};
use crate::llm::{
    safe_http::{SafeMethod, SafeResponse},
    wire::{AuthStyle, GatewayFamily, Protocol},
};

/// Characters of an error answer kept for the key verdict's quota check.
const ERROR_TEXT_CHARS: usize = 512;

/// The list body of a 2xx answer: the parsed JSON, else what can be salvaged from a body cut
/// at the response cap.
fn list_value(response: &SafeResponse) -> Option<Value> {
    response.json().or_else(|| {
        response
            .truncated
            .then(|| parse_truncated(&response.body))
            .flatten()
    })
}

/// The sentence of a model-list answer: how many entries it listed.
fn describe_models(response: &SafeResponse) -> Option<String> {
    if !response.is_success() {
        return None;
    }
    let count = parse_page(&list_value(response)?).models.len();
    Some(format!("{count} models"))
}

/// The page request path: `path`, the first-page query and the continuation.
fn page_path(attempt: &ModelAttempt, next: Option<&ListCursor>) -> String {
    let mut query: Vec<String> = Vec::new();
    if let Some(typed) = &attempt.query {
        query.push(typed.clone());
    }
    let encode =
        |text: &str| -> String { url::form_urlencoded::byte_serialize(text.as_bytes()).collect() };
    match next {
        Some(ListCursor::AfterId(id)) => query.push(format!("after_id={}", encode(id))),
        Some(ListCursor::Token(token)) => query.push(format!("pageToken={}", encode(token))),
        None => {}
    }
    if query.is_empty() {
        attempt.path.clone()
    } else {
        format!("{}?{}", attempt.path, query.join("&"))
    }
}

/// What one list request found.
enum Page {
    /// A page with models.
    Found(ParsedPage),
    /// Nothing here: the next attempt may list.
    FallThrough,
    /// The list phase is over.
    End,
}

impl Scan<'_> {
    /// The model-list requests in order: Ollama's `/api/tags` first on a detected Ollama host
    /// that is not a preset, then the planned attempts.
    pub(super) fn model_attempts(&self) -> Vec<ModelAttempt> {
        let mut attempts = self.plan.model_attempts.clone();
        let ollama = self
            .state()
            .local
            .classify(self.url.port)
            .map(|hit| hit.family)
            == Some(GatewayFamily::Ollama);
        if ollama && self.plan.known.is_none() {
            attempts.insert(
                0,
                ModelAttempt {
                    source: ModelSource::OllamaTags,
                    path: "/api/tags".to_owned(),
                    mount: String::new(),
                    query: None,
                },
            );
        }
        attempts
    }

    /// The model list: `attempts` in order until one lists models. `first_row` is the planned
    /// row and `hold` the reserved slot of the first attempt.
    pub(super) async fn models(
        &self,
        attempts: &[ModelAttempt],
        first_row: Option<u32>,
        hold: Option<u32>,
    ) {
        let mut row = first_row;
        let mut hold = hold;
        for attempt in attempts {
            match self
                .models_page(attempt, None, row.take(), hold.take())
                .await
            {
                Page::Found(first) => {
                    self.collect_pages(attempt, first).await;
                    return;
                }
                Page::FallThrough => {}
                Page::End => return,
            }
        }
    }

    async fn collect_pages(&self, attempt: &ModelAttempt, first: ParsedPage) {
        let mut next = first.next.clone();
        let anonymous_pricing = self.key.is_none()
            && first.models.iter().any(|raw| {
                raw.model.input_cost_per_mtok.is_some() && raw.model.context_window.is_some()
            });
        {
            let mut state = self.state();
            state.facts.openrouter_anonymous_models |= anonymous_pricing;
            state.models.raw.extend(first.models);
            state
                .models
                .answered
                .push((attempt.source, attempt.mount.clone()));
            state.models.winner = Some(attempt.clone());
        }
        let mut pages = 1;
        while let Some(token) = next.take() {
            if pages >= MAX_PAGES {
                self.ex.warn(SniffWarning::ListTruncated);
                return;
            }
            pages += 1;
            match self.models_page(attempt, Some(&token), None, None).await {
                Page::Found(page) => {
                    next = page.next.clone();
                    self.state().models.raw.extend(page.models);
                }
                Page::FallThrough | Page::End => return,
            }
        }
    }

    async fn models_page(
        &self,
        attempt: &ModelAttempt,
        next: Option<&ListCursor>,
        row: Option<u32>,
        hold: Option<u32>,
    ) -> Page {
        let path = page_path(attempt, next);
        let protocol = attempt.source.protocol();
        let call = Call {
            max_body: MODELS_BODY_CAP,
            row,
            describe: describe_models,
            ..Call::new(
                Purpose::Models,
                "Models",
                SafeMethod::Get,
                path.as_str(),
                self.ex.limits.models,
                Spend::held(TIER_MODELS, hold),
            )
        };
        let (sent, style) = self.authed(AuthFamily::of(protocol), call).await;
        let Sent::Done { result } = sent else {
            return Page::End;
        };
        self.record_first_request(&path, style, protocol);
        let Ok(response) = result else {
            return Page::End;
        };
        self.observe(&response);
        self.record_list_status(attempt, &response);
        if response.is_success() {
            let page = list_value(&response)
                .map(|value| parse_page(&value))
                .unwrap_or_default();
            return if page.models.is_empty() {
                Page::FallThrough
            } else {
                Page::Found(page)
            };
        }
        if matches!(response.status, 404 | 405) {
            Page::FallThrough
        } else {
            Page::End
        }
    }

    /// Keeps the first list request that left, for "Copy as curl".
    fn record_first_request(&self, path: &str, style: AuthStyle, protocol: Protocol) {
        self.state()
            .models
            .first_request
            .get_or_insert_with(|| FirstModelsRequest {
                url: format!("{}{path}", self.ex.origin),
                style,
                protocol,
            });
    }

    /// Keeps the status of a list answer, its error text, and whether its mount is alive.
    fn record_list_status(&self, attempt: &ModelAttempt, response: &SafeResponse) {
        let mut state = self.state();
        state.models.status = Some(response.status);
        state.models.error_text = if response.status >= 400 {
            String::from_utf8_lossy(&response.body)
                .chars()
                .take(ERROR_TEXT_CHARS)
                .collect()
        } else {
            String::new()
        };
        if !matches!(response.status, 404 | 405) {
            state
                .models
                .first_alive
                .push((attempt.source, attempt.mount.clone()));
        }
    }
}
