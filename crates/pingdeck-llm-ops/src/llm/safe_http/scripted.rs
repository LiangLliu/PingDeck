//! Scripted [`SafeHttp`] double for tests: canned replies per method and path, a request log.
//!
//! Single responsibility: let sniffer, balance and AI-run tests assert exactly which requests
//! were sent (count, order, headers, bodies) without opening a socket. Unmatched requests answer
//! `404` with an empty body, like a server that has no such route.

use std::{
    sync::{Mutex, PoisonError},
    time::Duration,
};

use futures_util::future::BoxFuture;

use super::{SafeHttp, SafeMethod, SafeRequest, SafeResponse, SafeTransportError};
use pingdeck_base::cancel::{CancelToken, until_cancelled};

/// One canned answer.
#[derive(Debug, Clone)]
pub enum Reply {
    /// A response, optionally delayed (the delay observes the request timeout and the cancel
    /// token like a real server that is slow).
    Response {
        /// Status code.
        status: u16,
        /// Headers (any case).
        headers: Vec<(String, String)>,
        /// Body bytes.
        body: Vec<u8>,
        /// Time the server takes before answering.
        delay: Duration,
    },
    /// The transport fails with this error.
    Error(SafeTransportError),
}

impl Reply {
    /// A JSON response with `status`.
    #[must_use]
    pub fn json(status: u16, body: &serde_json::Value) -> Self {
        Self::Response {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.to_string().into_bytes(),
            delay: Duration::ZERO,
        }
    }

    /// A response with raw text and an explicit content type.
    #[must_use]
    pub fn text(status: u16, content_type: &str, body: &str) -> Self {
        Self::Response {
            status,
            headers: vec![("content-type".into(), content_type.into())],
            body: body.as_bytes().to_vec(),
            delay: Duration::ZERO,
        }
    }

    /// Adds a response header.
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        if let Self::Response { headers, .. } = &mut self {
            headers.push((name.to_owned(), value.to_owned()));
        }
        self
    }

    /// Delays the answer.
    #[must_use]
    pub fn with_delay(mut self, by: Duration) -> Self {
        if let Self::Response { delay, .. } = &mut self {
            *delay = by;
        }
        self
    }
}

/// One scripted route: the requests it matches, its replies and how many it has served.
#[derive(Debug)]
struct Route {
    method: SafeMethod,
    host: Option<String>,
    path: String,
    replies: Vec<Reply>,
    served: usize,
}

/// The scripted transport.
#[derive(Debug, Default)]
pub struct ScriptedHttp {
    routes: Mutex<Vec<Route>>,
    log: Mutex<Vec<SafeRequest>>,
}

/// The lower-case authority and the path (query and fragment dropped, leading `/` kept) of `url`.
fn split_url(url: &str) -> (String, String) {
    let without_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let (authority, rest) = without_scheme
        .split_once('/')
        .map_or((without_scheme, ""), |(host, path)| (host, path));
    let path = rest.split(['?', '#']).next().unwrap_or_default();
    (authority.to_ascii_lowercase(), format!("/{path}"))
}

impl ScriptedHttp {
    /// An empty script: every request answers `404`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Answers `method path` (query ignored, any host) with `replies` in order; the last reply
    /// repeats once the list is used up.
    #[must_use]
    pub fn route(self, method: SafeMethod, path: &str, replies: Vec<Reply>) -> Self {
        self.push_route(method, None, path, replies)
    }

    /// Like [`Self::route`] but only for the authority `host` (`api.example.com:8443`).
    #[must_use]
    pub fn route_on(self, host: &str, method: SafeMethod, path: &str, replies: Vec<Reply>) -> Self {
        self.push_route(method, Some(host.to_ascii_lowercase()), path, replies)
    }

    fn push_route(
        self,
        method: SafeMethod,
        host: Option<String>,
        path: &str,
        replies: Vec<Reply>,
    ) -> Self {
        self.routes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Route {
                method,
                host,
                path: path.to_owned(),
                replies,
                served: 0,
            });
        self
    }

    /// Every request received so far, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<SafeRequest> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Number of requests received so far.
    #[must_use]
    pub fn count(&self) -> usize {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn pick(&self, request: &SafeRequest) -> Reply {
        let (host, path) = split_url(&request.url);
        let mut routes = self.routes.lock().unwrap_or_else(PoisonError::into_inner);
        for route in routes.iter_mut() {
            if route.method == request.method
                && route.path == path
                && route.host.as_ref().is_none_or(|wanted| *wanted == host)
                && !route.replies.is_empty()
            {
                let index = route.served.min(route.replies.len() - 1);
                route.served += 1;
                return route.replies[index].clone();
            }
        }
        Reply::Response {
            status: 404,
            headers: Vec::new(),
            body: Vec::new(),
            delay: Duration::ZERO,
        }
    }
}

impl SafeHttp for ScriptedHttp {
    fn send<'a>(
        &'a self,
        request: SafeRequest,
        cancel: &'a CancelToken,
    ) -> BoxFuture<'a, Result<SafeResponse, SafeTransportError>> {
        Box::pin(async move {
            self.log
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(request.clone());
            let (status, headers, mut body, delay) = match self.pick(&request) {
                Reply::Error(error) => return Err(error),
                Reply::Response {
                    status,
                    headers,
                    body,
                    delay,
                } => (status, headers, body, delay),
            };
            if !delay.is_zero() {
                let wait = delay.min(request.timeout.saturating_add(Duration::from_millis(1)));
                if until_cancelled(Some(cancel), tokio::time::sleep(wait))
                    .await
                    .is_none()
                {
                    return Err(SafeTransportError::Cancelled);
                }
                if delay > request.timeout {
                    return Err(SafeTransportError::Timeout);
                }
            }
            let truncated = body.len() > request.max_body;
            body.truncate(request.max_body);
            Ok(SafeResponse {
                status,
                headers: headers
                    .into_iter()
                    .map(|(name, value)| (name.to_ascii_lowercase(), value))
                    .collect(),
                body,
                truncated,
                elapsed: delay,
            })
        })
    }
}
