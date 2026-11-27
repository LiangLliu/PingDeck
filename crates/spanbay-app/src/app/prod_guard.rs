//! The production guard: "Confirm before send" enforced in the core.
//!
//! Single responsibility: decide whether a send, a collection run or a flow run may start against
//! an environment, given the caller's `confirmed` flag. The UI asks the user first and passes
//! `confirmed: true`; every other caller (the agent's `send_request`, a CLI, a script) gets
//! [`AppError::ConfirmationRequired`] instead of a request on the wire, so the guard cannot be
//! bypassed by skipping the dialog, nor by a pre-request script that widens the method after the
//! check ([`guard_method_change`]). A script's `pm.sendRequest` meets the same rule in the script
//! host and its refusal reaches the caller as the same error.

use crate::error::AppError;
use crate::workspace::HttpMethod;
use crate::workspace::models::Environment;

/// Upper-case name of `method`, as the guard message shows it.
fn method_name(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Head => "HEAD",
        HttpMethod::Options => "OPTIONS",
    }
}

/// Lets a single `method` request through unless `environment` guards it and the caller did not
/// confirm. Safe methods (GET, HEAD, OPTIONS) are guarded only when the environment's
/// `confirm_safe_methods` is on.
///
/// # Errors
/// [`AppError::ConfirmationRequired`] naming the environment and the method; nothing was sent.
pub(crate) fn guard_request(
    environment: Option<&Environment>,
    method: HttpMethod,
    confirmed: Option<bool>,
) -> Result<(), AppError> {
    match environment {
        Some(environment)
            if confirmed != Some(true) && environment.requires_confirmation(method) =>
        {
            Err(AppError::confirmation_required(format!(
                "{} is guarded: confirm before sending a {} request",
                environment.name,
                method_name(method)
            )))
        }
        _ => Ok(()),
    }
}

/// Re-checks the guard when a pre-request script changed the method from `from` to `to`.
///
/// The caller's check covered `from`: when the environment guards it, the caller confirmed
/// already (or was refused), so only a method the caller never saw as guarded is checked here.
///
/// # Errors
/// [`AppError::ConfirmationRequired`] when `to` is guarded and the caller did not confirm.
pub(crate) fn guard_method_change(
    environment: Option<&Environment>,
    from: HttpMethod,
    to: HttpMethod,
    confirmed: Option<bool>,
) -> Result<(), AppError> {
    if environment.is_some_and(|environment| environment.requires_confirmation(from)) {
        return Ok(());
    }
    guard_request(environment, to, confirmed)
}
/// Lets a run (a collection run or a flow run) through unless `environment` guards at least one of
/// its `methods` and the caller did not confirm. One confirmation covers the whole run:
/// the shell asks once, before the first request.
///
/// # Errors
/// [`AppError::ConfirmationRequired`] naming the environment and the number of guarded requests;
/// nothing ran.
pub(crate) fn guard_run(
    environment: Option<&Environment>,
    methods: impl IntoIterator<Item = HttpMethod>,
    confirmed: Option<bool>,
) -> Result<(), AppError> {
    let Some(environment) = environment else {
        return Ok(());
    };
    if confirmed == Some(true) {
        return Ok(());
    }
    let (guarded, total) =
        methods
            .into_iter()
            .fold((0_usize, 0_usize), |(guarded, total), method| {
                (
                    guarded + usize::from(environment.requires_confirmation(method)),
                    total + 1,
                )
            });
    if guarded == 0 {
        return Ok(());
    }
    Err(AppError::confirmation_required(format!(
        "{} is guarded: confirm before running {total} request{}",
        environment.name,
        if total == 1 { "" } else { "s" }
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(confirm: bool, safe: bool) -> Environment {
        let mut environment = Environment::new("env_prod", "ws_1", "Production").unwrap();
        environment.confirm_before_send = confirm;
        environment.confirm_safe_methods = safe;
        environment
    }

    fn is_confirmation(result: Result<(), AppError>) -> bool {
        matches!(result, Err(AppError::ConfirmationRequired { .. }))
    }

    #[test]
    fn guard_blocks_unsafe_methods_until_confirmed_and_never_blocks_safe_ones_by_default() {
        let production = environment(true, false);
        assert!(is_confirmation(guard_request(
            Some(&production),
            HttpMethod::Post,
            None
        )));
        assert!(is_confirmation(guard_request(
            Some(&production),
            HttpMethod::Delete,
            Some(false)
        )));
        assert!(guard_request(Some(&production), HttpMethod::Post, Some(true)).is_ok());
        for safe in [HttpMethod::Get, HttpMethod::Head, HttpMethod::Options] {
            assert!(guard_request(Some(&production), safe, None).is_ok());
        }
    }

    #[test]
    fn guard_widens_to_safe_methods_when_asked_and_is_off_by_default() {
        assert!(is_confirmation(guard_request(
            Some(&environment(true, true)),
            HttpMethod::Get,
            None
        )));
        assert!(guard_request(Some(&environment(false, true)), HttpMethod::Post, None).is_ok());
        assert!(guard_request(None, HttpMethod::Post, None).is_ok());
    }

    #[test]
    fn run_guard_asks_once_for_the_whole_run_and_counts_only_when_something_is_guarded() {
        let production = environment(true, false);
        let error = guard_run(
            Some(&production),
            [HttpMethod::Get, HttpMethod::Post, HttpMethod::Put],
            None,
        )
        .unwrap_err();
        assert!(matches!(error, AppError::ConfirmationRequired { .. }));
        assert!(error.message().contains("3 requests"), "{error}");
        assert!(error.message().contains("Production"), "{error}");
        assert!(guard_run(Some(&production), [HttpMethod::Post], Some(true)).is_ok());
        assert!(guard_run(Some(&production), [HttpMethod::Get, HttpMethod::Head], None).is_ok());
        assert!(guard_run(Some(&production), [], None).is_ok());
        assert!(guard_run(None, [HttpMethod::Post], None).is_ok());
    }

    #[test]
    fn a_script_that_widens_the_method_needs_the_confirmation_the_saved_method_did_not() {
        let production = environment(true, false);
        // GET -> DELETE: the caller never saw a guarded method.
        assert!(is_confirmation(guard_method_change(
            Some(&production),
            HttpMethod::Get,
            HttpMethod::Delete,
            None
        )));
        assert!(
            guard_method_change(
                Some(&production),
                HttpMethod::Get,
                HttpMethod::Delete,
                Some(true)
            )
            .is_ok()
        );
        // Already guarded (hence confirmed by the caller) or unchanged: nothing more to ask.
        assert!(
            guard_method_change(
                Some(&production),
                HttpMethod::Post,
                HttpMethod::Delete,
                None
            )
            .is_ok()
        );
        assert!(
            guard_method_change(Some(&production), HttpMethod::Get, HttpMethod::Head, None).is_ok()
        );
        assert!(guard_method_change(None, HttpMethod::Get, HttpMethod::Delete, None).is_ok());
    }
}
