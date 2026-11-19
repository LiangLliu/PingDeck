//! The one place a balance request leaves the process. [`Session`] sends the `GET`s of one check
//! through [`SafeHttp`] only (no redirects, 256 KiB body cap, 10 s timeout, the adapter's own
//! headers and `Accept`, no cookies), counts them against the request budget, honours the cancel
//! token and turns transport and status failures into [`BalanceError`]s. A URL is always built by
//! the adapter from the target's origin or a fixed path, never from a response body.

use std::time::Duration;

use serde_json::Value;

use crate::llm::safe_http::{SafeHttp, SafeMethod, SafeRequest, SafeResponse};
use pingdeck_base::cancel::CancelToken;
use pingdeck_base::secrets::redact_secrets;
pub(super) use pingdeck_llm_domain::llm::balance_state::BalanceAuth;

use super::error::{BalanceError, BalanceErrorKind};
use super::target::normalise_origin;
use super::{BalanceTarget, MAX_BODY_BYTES};

/// Total time limit of one balance request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Connection setup limit of one balance request.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest vendor message copied into an error.
const MAX_MESSAGE_CHARS: usize = 200;

/// `Authorization: Bearer <token>`.
pub(super) fn bearer(token: &str) -> (String, String) {
    ("Authorization".to_owned(), format!("Bearer {token}"))
}

/// A successful, parsed answer. No `Debug`: the body may echo the key or the console token.
pub(super) struct Fetched {
    /// The body as text (for JSONPath).
    pub text: String,
    /// The body as JSON.
    pub json: Value,
}

/// The requests of one balance check.
pub(super) struct Session<'a> {
    http: &'a dyn SafeHttp,
    cancel: &'a CancelToken,
    origin: String,
    allow_private: bool,
    budget: u32,
    used: u32,
    secrets: Vec<String>,
    allowed_origins: Vec<String>,
}

impl<'a> Session<'a> {
    /// A session against `origin` (`scheme://host[:port]`, normalised by the caller).
    pub(super) fn new(
        http: &'a dyn SafeHttp,
        cancel: &'a CancelToken,
        origin: String,
        target: &BalanceTarget<'_>,
    ) -> Self {
        let secrets = [Some(target.api_key), target.management_token]
            .into_iter()
            .flatten()
            .filter(|secret| !secret.is_empty())
            .map(str::to_owned)
            .collect();
        Self {
            http,
            cancel,
            origin,
            allow_private: target.allow_private,
            budget: target.request_budget,
            used: 0,
            secrets,
            allowed_origins: target.allowed_origins(),
        }
    }

    /// The target's origin.
    pub(super) fn origin(&self) -> &str {
        &self.origin
    }

    /// `path` (starting with `/`) on the target's origin.
    pub(super) fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }

    /// Whether `origin` (normalised) is one of the origins a request may go to.
    fn is_allowed(&self, origin: &str) -> bool {
        self.allowed_origins.iter().any(|allowed| allowed == origin)
    }

    /// Adds the origin of a custom query the user confirmed as another host.
    pub(super) fn allow_origin(&mut self, origin: String) {
        if !self.is_allowed(&origin) {
            self.allowed_origins.push(origin);
        }
    }

    /// Requests issued so far.
    pub(super) const fn used(&self) -> u32 {
        self.used
    }

    /// Whether another request fits the budget.
    pub(super) const fn has_budget(&self) -> bool {
        self.used < self.budget
    }

    /// Every credential this check handles, for redaction.
    pub(super) fn secrets(&self) -> &[String] {
        &self.secrets
    }

    /// Registers more secrets (resolved custom header values) for redaction.
    pub(super) fn add_secrets<'s>(&mut self, values: impl IntoIterator<Item = &'s str>) {
        self.secrets.extend(
            values
                .into_iter()
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        );
    }

    /// Vendor text safe to show: secrets removed, whitespace runs collapsed to one space, at most
    /// [`MAX_MESSAGE_CHARS`] characters (the cut may fall inside a word or after a space).
    pub(super) fn safe_text(&self, text: &str) -> String {
        let redacted = redact_secrets(text, &self.secrets);
        let mut line = String::new();
        let mut room = MAX_MESSAGE_CHARS;
        for (index, word) in redacted.split_whitespace().enumerate() {
            let separator = (index > 0).then_some(' ');
            for ch in separator.into_iter().chain(word.chars()) {
                if room == 0 {
                    return line;
                }
                line.push(ch);
                room -= 1;
            }
        }
        line
    }

    /// The endpoint label of `url`: the path for the target's own origin, `host/path` otherwise.
    pub(super) fn endpoint_label(&self, url: &str) -> String {
        match url.strip_prefix(self.origin.as_str()) {
            Some(path) if path.starts_with('/') => path.to_owned(),
            _ => url
                .split_once("://")
                .map_or(url, |(_, rest)| rest)
                .to_owned(),
        }
    }

    /// `GET`s `url` and returns the response whatever its status.
    ///
    /// # Errors
    /// [`BalanceErrorKind::Cancelled`] before sending when the token fired,
    /// [`BalanceErrorKind::Blocked`] when the budget is spent, and the transport classes when no
    /// response came back.
    pub(super) async fn send(
        &mut self,
        url: &str,
        headers: Vec<(String, String)>,
    ) -> Result<SafeResponse, BalanceError> {
        let endpoint = self.endpoint_label(url);
        if self.cancel.is_cancelled() {
            return Err(BalanceError::new(
                BalanceErrorKind::Cancelled,
                "The balance request was cancelled",
            )
            .at(endpoint));
        }
        if !normalise_origin(url).is_ok_and(|origin| self.is_allowed(&origin)) {
            return Err(BalanceError::new(
                BalanceErrorKind::Blocked,
                "The address is not one of the provider's endpoint origins",
            )
            .at(endpoint));
        }
        if !self.has_budget() {
            return Err(BalanceError::new(
                BalanceErrorKind::Blocked,
                "The request budget does not allow another balance request",
            )
            .at(endpoint));
        }
        self.used += 1;
        let mut all_headers = Vec::with_capacity(headers.len() + 1);
        all_headers.push(("Accept".to_owned(), "application/json".to_owned()));
        all_headers.extend(headers);
        let request = SafeRequest {
            method: SafeMethod::Get,
            url: url.to_owned(),
            headers: all_headers,
            body: None,
            timeout: REQUEST_TIMEOUT,
            connect_timeout: CONNECT_TIMEOUT,
            max_body: MAX_BODY_BYTES,
            allow_private: self.allow_private,
        };
        self.http
            .send(request, self.cancel)
            .await
            .map_err(|error| BalanceError::from_transport(&error, &endpoint))
    }

    /// `GET`s `url` and returns the JSON body of a 2xx answer.
    ///
    /// # Errors
    /// As [`Self::send`], plus the status classes for a non-2xx answer and
    /// [`BalanceErrorKind::SchemaMismatch`] for a truncated or non-JSON body.
    pub(super) async fn get(
        &mut self,
        url: &str,
        headers: Vec<(String, String)>,
    ) -> Result<Fetched, BalanceError> {
        let endpoint = self.endpoint_label(url);
        let response = self.send(url, headers).await?;
        if !response.is_success() {
            return Err(BalanceError::from_response(&response, &endpoint));
        }
        if response.truncated {
            return Err(BalanceError::schema(
                "The answer is larger than 256 KiB",
                &endpoint,
            ));
        }
        let json = serde_json::from_slice(&response.body)
            .map_err(|_| BalanceError::schema("The answer is not JSON", &endpoint))?;
        // The body as text, as `SafeResponse::text` gives it, without a copy when it is UTF-8.
        let text = String::from_utf8(response.body)
            .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned());
        Ok(Fetched { text, json })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::balance::tests::{KEY, target};
    use crate::llm::safe_http::scripted::ScriptedHttp;
    use crate::llm::wire::GatewayFamily;

    fn with_session(check: impl FnOnce(&Session<'_>)) {
        let http = ScriptedHttp::new();
        let cancel = CancelToken::new();
        let target = target(GatewayFamily::NewApi);
        let session = Session::new(&http, &cancel, "https://relay.acme.dev".to_owned(), &target);
        check(&session);
    }

    #[test]
    fn safe_text_is_one_redacted_line_of_at_most_200_characters() {
        with_session(|session| {
            assert_eq!(session.safe_text("  a \n\t b  "), "a b");
            assert_eq!(session.safe_text(""), "");
            assert_eq!(session.safe_text(&"x".repeat(250)), "x".repeat(200));
            assert_eq!(session.safe_text(&"é".repeat(250)), "é".repeat(200));
            assert_eq!(session.safe_text(&"é ".repeat(150)), "é ".repeat(100));
            assert_eq!(
                session.safe_text(&format!("a{}", " é".repeat(150))),
                format!("a{}", " é".repeat(99)) + " "
            );
            // The cut may land just after a separating space.
            assert_eq!(
                session.safe_text(&format!("{} b", "a".repeat(199))),
                format!("{} ", "a".repeat(199))
            );
            let leaked = session.safe_text(&format!("bad key {KEY}"));
            assert!(
                !leaked.contains(KEY) && leaked.starts_with("bad key "),
                "{leaked}"
            );
        });
    }

    #[test]
    fn endpoint_labels_name_the_host_unless_it_is_the_targets_own() {
        with_session(|session| {
            assert_eq!(
                session.endpoint_label("https://relay.acme.dev/api/x"),
                "/api/x"
            );
            assert_eq!(
                session.endpoint_label("https://relay.acme.dev"),
                "relay.acme.dev"
            );
            assert_eq!(
                session.endpoint_label("https://billing.example.net/api/me"),
                "billing.example.net/api/me"
            );
            assert_eq!(
                session.endpoint_label("https://relay.acme.dev.evil/x"),
                "relay.acme.dev.evil/x"
            );
            assert_eq!(
                session.endpoint_label("https://relay.acme.dev:8443/x"),
                "relay.acme.dev:8443/x"
            );
        });
    }
}
