//! The request send loop: protocol dispatch, redirect following, timing and cancellation.

use crate::executor::auth::{
    apply_aws_signature, is_authorization_header, mask_credential_header, retry_with_digest_auth,
};
use crate::executor::body::{
    BodyKind, apply_graphql_body, apply_http_rest_body, payload_sha256, sent_body_preview,
};
use crate::executor::client::{build_client_async, proxy_route};
use crate::executor::cookies::{
    capture_hop_cookies, cookie_header_for_url, cookie_header_value, mask_cookie_header,
};
use crate::executor::oauth_flow::oauth2_token;
use crate::executor::request::{
    RedirectHeaderPolicy, all_unresolved_keys, build_request_builder, redirect_target,
    request_method, request_origin, request_url, resolve_value, variable_scopes,
};
use crate::executor::response::{BodyReadOptions, read_response_body};
use crate::executor::retry::execute_with_retries;
use crate::executor::trace::{auth_trace, collect_variable_traces, transport_trace, version_label};
use crate::executor::types::{
    CookieJarOutcome, CookieReport, ExecutionError, ExecutionHeader, ExecutionResult,
    ExecutorOptions, GuardRefusal, SentRequest,
};
use crate::executor::view::suggest_view;
use pingdeck_base::cancel::until_cancelled;
use pingdeck_base::error::NetworkCause;
use pingdeck_base::secrets::{REDACTED, is_secret_header_name, mask_secrets};
use pingdeck_model::models::{CallChainTrace, PhaseTiming, RedirectHopTrace};
use pingdeck_model::{AuthConfig, Environment, HttpMethod, ProtocolKind, Request, VariableScopes};
use reqwest::Method;
use reqwest::header::{COOKIE, LOCATION};
use std::future::Future;
use std::time::{Duration, Instant};
use url::Url;

/// Awaits `future`, resolving to [`ExecutionError::Cancelled`] if the options' token fires first.
pub(super) async fn await_or_cancel<F: Future>(
    options: &ExecutorOptions,
    future: F,
) -> Result<F::Output, ExecutionError> {
    until_cancelled(options.cancel.as_ref(), future)
        .await
        .ok_or(ExecutionError::cancelled())
}

/// Whether the request to `url` goes through an explicit proxy, so the client dials the proxy and
/// not the origin (the route `build_client` applies).
fn proxied(request: &Request, options: &ExecutorOptions, url: &Url) -> bool {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    matches!(
        proxy_route(request, &options.auxiliary, &host),
        pingdeck_base::proxy_url::ProxyRoute::Proxy(_)
    )
}

/// Whether the request to `url` dials the origin itself: no proxy, not even the system's.
fn dials_origin(request: &Request, options: &ExecutorOptions, url: &Url) -> bool {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    matches!(
        proxy_route(request, &options.auxiliary, &host),
        pingdeck_base::proxy_url::ProxyRoute::Direct
    )
}

/// Executes `request`, resolving variables from `options` or `environment`.
pub async fn execute_request(
    request: &Request,
    environment: Option<&Environment>,
    options: ExecutorOptions,
) -> Result<ExecutionResult, ExecutionError> {
    execute_request_with_scopes(
        request,
        options
            .variable_scopes
            .clone()
            .unwrap_or_else(|| variable_scopes(environment)),
        options,
    )
    .await
}

/// Executes `request` with explicit variable `scopes`; non-HTTP protocols are rejected.
pub async fn execute_request_with_scopes(
    request: &Request,
    scopes: VariableScopes,
    options: ExecutorOptions,
) -> Result<ExecutionResult, ExecutionError> {
    let outcome = match request.protocol {
        ProtocolKind::HttpRest => execute_http_rest_request(request, &scopes, &options).await,
        ProtocolKind::Graphql => execute_graphql_request(request, &scopes, &options).await,
        _ => Err(ExecutionError::UnsupportedProtocol),
    };
    outcome.map_err(|error| match error {
        ExecutionError::UnresolvedVariable { keys } => ExecutionError::UnresolvedVariable {
            keys: all_unresolved_keys(request, &scopes, keys),
        },
        error => error,
    })
}

async fn execute_http_rest_request(
    request: &Request,
    scopes: &VariableScopes,
    options: &ExecutorOptions,
) -> Result<ExecutionResult, ExecutionError> {
    execute_with_retries(request, options, || {
        execute_http_rest_request_once(request, scopes, options)
    })
    .await
}

async fn execute_http_rest_request_once(
    request: &Request,
    scopes: &VariableScopes,
    options: &ExecutorOptions,
) -> Result<ExecutionResult, ExecutionError> {
    let Some(token) = oauth2_token(request, scopes, options).await? else {
        return send_once(request, scopes, options, BodyKind::HttpRest).await;
    };
    let result = send_once(&token.apply(request), scopes, options, BodyKind::HttpRest).await?;
    // A refused token is dropped and the send retried once with a new one.
    if result.status_code == 401
        && let Some(fresh) = token.renewed(options).await?
    {
        return send_once(&fresh.apply(request), scopes, options, BodyKind::HttpRest).await;
    }
    Ok(result)
}

/// One send of `request` (its auth already resolved) with redirects and a Digest answer.
async fn send_once(
    request: &Request,
    scopes: &VariableScopes,
    options: &ExecutorOptions,
    body_kind: BodyKind,
) -> Result<ExecutionResult, ExecutionError> {
    let method = match body_kind {
        BodyKind::HttpRest => request_method(request.method),
        BodyKind::Graphql => Method::POST,
    };
    let url = request_url(request, scopes)?;
    let result = send_with_redirects(
        request,
        scopes,
        options,
        method.clone(),
        url.clone(),
        body_kind,
    )
    .await?;
    retry_with_digest_auth(request, scopes, options, method, url, body_kind, result).await
}

async fn execute_graphql_request(
    request: &Request,
    scopes: &VariableScopes,
    options: &ExecutorOptions,
) -> Result<ExecutionResult, ExecutionError> {
    execute_with_retries(request, options, || {
        execute_graphql_request_once(request, scopes, options)
    })
    .await
}

async fn execute_graphql_request_once(
    request: &Request,
    scopes: &VariableScopes,
    options: &ExecutorOptions,
) -> Result<ExecutionResult, ExecutionError> {
    if request.method != HttpMethod::Post {
        return Err(ExecutionError::UnsupportedGraphqlMethod);
    }

    let Some(token) = oauth2_token(request, scopes, options).await? else {
        return send_once(request, scopes, options, BodyKind::Graphql).await;
    };
    let result = send_once(&token.apply(request), scopes, options, BodyKind::Graphql).await?;
    if result.status_code == 401
        && let Some(fresh) = token.renewed(options).await?
    {
        return send_once(&fresh.apply(request), scopes, options, BodyKind::Graphql).await;
    }
    Ok(result)
}

/// Names of the `sent` headers that no enabled editor row set (body Content-Type, auth, jar
/// cookies, SigV4), first occurrence order, for the editor's "auto" row.
fn implicit_header_names(
    request: &Request,
    scopes: &VariableScopes,
    sent: &[ExecutionHeader],
) -> Vec<String> {
    let explicit = request
        .headers
        .iter()
        .filter(|header| header.enabled)
        .map(|header| resolve_value(&header.key, scopes).unwrap_or_else(|_| header.key.clone()))
        .collect::<Vec<_>>();
    let mut names: Vec<String> = Vec::new();
    for header in sent {
        let is_explicit = explicit
            .iter()
            .any(|key| key.trim().eq_ignore_ascii_case(header.key.trim()));
        if !is_explicit
            && !names
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&header.key))
        {
            names.push(header.key.clone());
        }
    }
    names
}

/// Sends the request and follows redirects, capturing timing, trace, cookies and the final response.
pub(super) async fn send_with_redirects(
    request: &Request,
    scopes: &VariableScopes,
    options: &ExecutorOptions,
    initial_method: Method,
    initial_url: Url,
    body_kind: BodyKind,
) -> Result<ExecutionResult, ExecutionError> {
    let started_at = Instant::now();
    // The total limit spans every hop and the body (docs/architecture.md §6); the client's own
    // timeouts are idle ones.
    let total_limit = request
        .network_options
        .total_timeout_ms
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis);
    // A limit too large to add to the clock sets no deadline.
    let deadline = total_limit.and_then(|limit| tokio::time::Instant::now().checked_add(limit));
    let mut client = build_client_async(
        request,
        &options.certificates,
        &initial_url,
        options.resolve_guard.as_ref(),
        &options.auxiliary,
    )
    .await?;
    let max_hops = request.network_options.max_redirect_hops();
    let mut session_cookies = options.cookie_jar.clone();
    let mut captured_cookies = Vec::new();
    let mut rejected_cookies = Vec::new();
    let mut method = initial_method;
    let mut url = initial_url;
    let mut include_body = true;
    let mut policy = RedirectHeaderPolicy::default();
    let mut hop_count = 0;
    let mut hop_started = Instant::now();
    let mut redirect_hops = Vec::new();
    let secret_values = scopes.secret_values();

    loop {
        options.check_cancelled()?;
        let (mut builder, content_type_header, has_explicit_cookie_header, mut sent_headers) =
            build_request_builder(&client, request, scopes, &url, method.clone(), policy)?;

        let use_jar_cookies = request.network_options.send_cookies
            && (!has_explicit_cookie_header || policy.strip_cookies);
        if use_jar_cookies
            && let Some(cookie_header) =
                cookie_header_for_url(&url, &session_cookies, dials_origin(request, options, &url))
        {
            builder = builder.header(COOKIE, cookie_header_value(&cookie_header)?);
            sent_headers.push(ExecutionHeader {
                key: "Cookie".to_owned(),
                value: cookie_header,
            });
        }

        let (builder, sent_body) = if include_body {
            match body_kind {
                BodyKind::HttpRest => {
                    apply_http_rest_body(
                        builder,
                        request,
                        scopes,
                        content_type_header.as_deref(),
                        &mut sent_headers,
                    )
                    .await?
                }
                BodyKind::Graphql => apply_graphql_body(
                    builder,
                    request,
                    scopes,
                    content_type_header.as_deref(),
                    &mut sent_headers,
                )?,
            }
        } else {
            (builder, None)
        };

        // Signing runs after the body is known so the payload hash covers the real bytes.
        // A cross-origin redirect without `follow_auth_header` must not sign at all,
        // otherwise AWS credentials would leak to the redirect target.
        let (builder, sent_headers) = if !policy.strip_authorization
            && matches!(request.auth, AuthConfig::AwsSignatureV4 { .. })
        {
            let payload_hash = payload_sha256(sent_body.as_ref()).await?;
            apply_aws_signature(
                builder,
                request,
                scopes,
                &url,
                &method,
                &payload_hash,
                sent_headers,
            )?
        } else {
            (builder, sent_headers)
        };

        let implicit_headers = implicit_header_names(request, scopes, &sent_headers);
        let (sent_body_text, sent_body_size_bytes, sent_body_truncated) =
            sent_body_preview(sent_body, &secret_values);
        let sent_request = SentRequest {
            method: method.as_str().to_owned(),
            url: mask_secrets(url.as_str(), &secret_values),
            headers: sent_headers
                .into_iter()
                .map(|header| {
                    // The one credential predicate; only Authorization keeps its scheme.
                    let value = if is_authorization_header(&header.key) {
                        mask_credential_header(&header.value)
                    } else if header.key.eq_ignore_ascii_case("cookie") {
                        mask_cookie_header(&header.value)
                    } else if is_secret_header_name(&header.key) {
                        REDACTED.to_owned()
                    } else {
                        mask_secrets(&header.value, &secret_values)
                    };
                    ExecutionHeader {
                        key: header.key,
                        value,
                    }
                })
                .collect(),
            body_text: sent_body_text,
            body_size_bytes: sent_body_size_bytes,
            body_truncated: sent_body_truncated,
            implicit_headers,
        };

        let attempt = client.probe.begin();
        let sent = match deadline {
            Some(deadline) => {
                tokio::time::timeout_at(deadline, await_or_cancel(options, builder.send())).await
            }
            None => Ok(await_or_cancel(options, builder.send()).await),
        };
        let Ok(sent) = sent else {
            return Err(total_timeout_error(total_limit));
        };
        let response = sent?.map_err(|error| {
            if let Some(refusal) = GuardRefusal::find(&error) {
                return ExecutionError::GuardRefused {
                    host: refusal.host.clone(),
                };
            }
            let connect_phase = error.is_connect();
            let cause = network_cause(&error, proxied(request, options, &url));
            ExecutionError::Network {
                message: network_message(error, options.resolve_guard.is_some()),
                attempt_count: 1,
                connect_phase,
                cause,
            }
        })?;
        let connection = attempt.connection();
        drop(attempt);
        let status = response.status();
        let response_url = response.url().clone();
        let response_headers = response.headers().clone();

        capture_hop_cookies(
            &response_headers,
            &response_url,
            &mut captured_cookies,
            &mut session_cookies,
            &mut rejected_cookies,
        );

        let location = response_headers
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);

        if hop_count < max_hops
            && let Some(next_url) =
                redirect_target(&response_headers, &response_url, status.as_u16())
        {
            if let Some(allowed) = &options.redirect_hosts {
                let host = next_url.host_str().unwrap_or_default().to_ascii_lowercase();
                let with_port = next_url
                    .port_or_known_default()
                    .map(|port| format!("{host}:{port}"));
                if !allowed
                    .iter()
                    .any(|entry| *entry == host || Some(entry) == with_port.as_ref())
                {
                    return Err(ExecutionError::RedirectHostNotDeclared {
                        host: with_port.unwrap_or(host),
                    });
                }
            }
            redirect_hops.push(RedirectHopTrace {
                hop: usize::try_from(hop_count + 1).unwrap_or(usize::MAX),
                method: method.as_str().to_owned(),
                url: mask_secrets(response_url.as_str(), &secret_values),
                status_code: status.as_u16(),
                location: location.clone(),
                duration_ms: hop_started.elapsed().as_millis(),
            });
            if request_origin(&response_url) != request_origin(&next_url) {
                policy.strip_authorization = !request.network_options.follow_auth_header;
                policy.strip_cookies = true;
            }
            if request.network_options.remove_referer_on_redirect {
                policy.strip_referer = true;
            }
            if matches!(status.as_u16(), 301..=303)
                && method != Method::HEAD
                && !request.network_options.follow_original_method
            {
                method = Method::GET;
                include_body = false;
            }
            url = next_url;
            hop_count += 1;
            hop_started = Instant::now();
            // A redirect may change host/port, so re-select the client (and its
            // client certificate) for the new origin before the next hop.
            client = build_client_async(
                request,
                &options.certificates,
                &url,
                options.resolve_guard.as_ref(),
                &options.auxiliary,
            )
            .await?;
            continue;
        }

        let headers: Vec<ExecutionHeader> = response_headers
            .iter()
            .map(|(key, value)| ExecutionHeader {
                key: key.as_str().to_owned(),
                value: value.to_str().unwrap_or("<non-utf8>").to_owned(),
            })
            .collect();
        let request_ms = hop_started.elapsed().as_millis();
        let protocol = version_label(response.version());
        let remote_addr = response.remote_addr().map(|address| address.to_string());
        let read_started = Instant::now();
        let body = read_response_body(
            response,
            &response_headers,
            BodyReadOptions {
                limits: &options.limits,
                deadline,
                cancel: options.cancel.as_ref(),
                keep_partial_on_cancel: options.keep_partial_on_cancel,
                sink: options.save_body_to.as_deref(),
            },
        )
        .await?;
        let response_ms = read_started.elapsed().as_millis();
        let timing = connection.apply(
            url.scheme() == "https",
            proxied(request, options, &url),
            PhaseTiming {
                request_ms,
                response_ms,
                total_ms: started_at.elapsed().as_millis(),
                ..PhaseTiming::default()
            },
        );
        let call_chain = CallChainTrace {
            variables: collect_variable_traces(request, scopes),
            auth: auth_trace(&request.auth),
            transport: transport_trace(
                request,
                options,
                &url,
                protocol,
                remote_addr,
                redirect_hops,
            ),
        };

        let suggested_view = suggest_view(&headers, &body.text);
        return Ok(ExecutionResult {
            status_code: status.as_u16(),
            status_text: status.canonical_reason().unwrap_or("").to_owned(),
            url: mask_secrets(response_url.as_str(), &secret_values),
            elapsed_ms: started_at.elapsed().as_millis(),
            attempt_count: 1,
            headers,
            cookies: captured_cookies,
            body_text: body.text,
            body_size_bytes: body.size_bytes,
            raw_body: Some(body.raw),
            body_handle: None,
            suggested_view,
            body_truncated: body.truncated,
            body_total_bytes: body.total_bytes,
            body_incomplete: body.incomplete,
            saved_body_path: options
                .save_body_to
                .as_deref()
                .map(|path| path.display().to_string()),
            body_base64: body.base64,
            raw_body_available: true,
            sent_request: Some(sent_request),
            assertion_results: Vec::new(),
            script_tests: Vec::new(),
            console: Vec::new(),
            script_error: None,
            next_request: None,
            stop_run: false,
            timing,
            call_chain,
            history_id: None,
            history_saved: false,
            ai: None,
            cookie_report: CookieReport {
                jar: CookieJarOutcome::NotStored,
                rejected: rejected_cookies,
            },
        });
    }
}

/// The failure of an exchange whose total limit ran out before the final response's headers
/// arrived; a timeout like the client's own, so retries and the failure card read it the same way.
fn total_timeout_error(limit: Option<Duration>) -> ExecutionError {
    ExecutionError::Network {
        message: format!(
            "the request exceeded its total time limit of {} ms",
            limit.map_or(0, |limit| limit.as_millis())
        ),
        attempt_count: 1,
        connect_phase: false,
        cause: Some(NetworkCause::Timeout),
    }
}

/// The text of a failed send. A guarded send (the agent's) appends the error's causes, so the
/// guard's own refusal ("refusing to connect to ...") reaches the user instead of the generic
/// "error sending request".
fn network_message(error: reqwest::Error, with_causes: bool) -> String {
    let error = error.without_url();
    let mut message = error.to_string();
    if with_causes {
        let mut source = std::error::Error::source(&error);
        while let Some(cause) = source {
            message.push_str(": ");
            message.push_str(&cause.to_string());
            source = cause.source();
        }
    }
    message
}

/// Why a send's transport failed: the client's timeout flag, else the shared reading of
/// the source chain ([`pingdeck_base::error::transport_cause`]).
fn network_cause(error: &reqwest::Error, proxied: bool) -> Option<NetworkCause> {
    if error.is_timeout() {
        return Some(NetworkCause::Timeout);
    }
    pingdeck_base::error::transport_cause(error, proxied)
}
