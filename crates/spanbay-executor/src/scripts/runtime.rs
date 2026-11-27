use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU8, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use boa_engine::{
    Context, JsNativeError, JsResult, JsString, JsValue, NativeFunction, Source, error::PanicError,
    job::SimpleJobExecutor, js_string,
};
use serde_json::Value;

use super::bridge::handle_operation;
use super::{PRELUDE, ScriptInfo, ScriptInput, ScriptLimits, ScriptMutations, ScriptOutput};
use crate::executor::ExecutionResult;
use spanbay_base::cancel::{CANCELLED_MESSAGE, CancelToken};
use spanbay_model::script_result::{ConsoleEntry, ScriptError, ScriptTestResult};
use spanbay_model::{
    CookieJarEntry, CookieJarEntryId,
    ai_request::AiScriptResult,
    models::{Environment, EnvironmentVariable, Request},
};

#[derive(Clone)]
pub(super) struct HostState {
    pub(super) request: Request,
    pub(super) response: Option<ExecutionResult>,
    pub(super) ai: Option<AiScriptResult>,
    pub(super) environment: Option<Environment>,
    pub(super) cancel: Option<CancelToken>,
    pub(super) collection_variables: Vec<EnvironmentVariable>,
    pub(super) globals: Vec<EnvironmentVariable>,
    pub(super) mutations: ScriptMutations,
    pub(super) iteration_data: BTreeMap<String, String>,
    pub(super) local: BTreeMap<String, String>,
    pub(super) tests: Vec<ScriptTestResult>,
    pub(super) console: Vec<ConsoleEntry>,
    pub(super) next_request: Option<Option<String>>,
    pub(super) info: ScriptInfo,
    pub(super) current_source: String,
    /// `pm.sendRequest` calls made by the script that is running (reset per script).
    pub(super) send_requests: u32,
    pub(super) limits: ScriptLimits,
    /// When the phase runs out of wall-clock time; the next bridge call then stops the script.
    pub(super) deadline: Instant,
    /// The `pm.sendRequest` clients, built on first use so their connections are reused per run;
    /// one per client certificate presented (`""` for none).
    pub(super) http_clients: super::bridge::ScriptClients,
    pub(super) confirmed: bool,
    pub(super) confirmation_required: Option<String>,
    pub(super) cookie_jar: Vec<CookieJarEntry>,
    pub(super) cookie_writes: Vec<CookieJarEntry>,
    pub(super) cookie_deletes: Vec<CookieJarEntryId>,
    /// The request URL `pm.cookies` reads for and whose host bounds `pm.cookies.jar()`, fixed
    /// before any script of the phase runs so a script cannot move it by rewriting variables.
    pub(super) cookie_url: Option<url::Url>,
}

type SharedHost = Rc<RefCell<HostState>>;

thread_local! {
    static HOST: RefCell<Option<SharedHost>> = const { RefCell::new(None) };
}

/// Abandoned (timed-out) script threads that still run before new script phases are refused.
const MAX_STUCK_WORKERS: usize = 4;

/// Script threads abandoned after a timeout or cancel that have not finished yet.
static STUCK_WORKERS: AtomicUsize = AtomicUsize::new(0);

/// State of a script worker thread: still running, finished normally, or abandoned by the waiter.
const WORKER_RUNNING: u8 = 0;
const WORKER_DONE: u8 = 1;
const WORKER_ABANDONED: u8 = 2;

/// Message of a script phase that ran out of wall-clock time.
fn timeout_message(limit: Duration) -> String {
    format!("{TIMEOUT_PREFIX} {} s time limit", limit.as_secs_f64())
}

/// Start of [`timeout_message`]; the bridge uses it to tell a timeout from an ordinary error.
pub(super) const TIMEOUT_PREFIX: &str = "Script exceeded the";

/// Executes all scripts for one phase in order. Scripts are expected to be
/// already ordered collection → folder chain → request.
///
/// The phase runs on its own thread so a script stuck inside one native builtin (a catastrophic
/// regex, a huge string) cannot hold the caller: the wait ends when the phase finishes, is
/// cancelled or exceeds [`ScriptLimits::wall_clock_limit`].
// shortcut: Boa 0.22 has no interrupt hook and a thread cannot be killed, so an abandoned script
// keeps its core busy until it finishes or reaches a bridge call; after `MAX_STUCK_WORKERS` such
// threads new phases are refused. Upgrade to a killable subprocess if that ever happens in practice.
#[must_use]
pub fn run_scripts(mut input: ScriptInput) -> ScriptOutput {
    if input
        .scripts
        .iter()
        .all(|script| script.source.trim().is_empty())
    {
        return run_scripts_inline(input, &Mutex::new(String::new()));
    }
    let scripts = std::mem::take(&mut input.scripts);
    let first_label = scripts
        .first()
        .map(|script| script.label.clone())
        .unwrap_or_default();
    let fallback = input.clone();
    input.scripts = scripts;
    let failed = move |source: String, message: String| {
        let mut output = into_output_of(fallback);
        output.error = Some(ScriptError { source, message });
        output
    };
    if STUCK_WORKERS.load(Ordering::Relaxed) >= MAX_STUCK_WORKERS {
        return failed(
            first_label,
            "Earlier scripts are still running after a timeout; restart SpanBay to stop them"
                .to_owned(),
        );
    }

    let limit = input.limits.wall_clock_limit;
    let cancel = input.cancel.clone();
    let deadline = Instant::now() + limit;
    let current = Arc::new(Mutex::new(first_label.clone()));
    let state = Arc::new(AtomicU8::new(WORKER_RUNNING));
    let (sender, receiver) = mpsc::channel();
    let spawned = {
        let (current, state) = (Arc::clone(&current), Arc::clone(&state));
        std::thread::Builder::new()
            .name("spanbay-script".to_owned())
            .spawn(move || {
                let _ = sender.send(run_scripts_inline(input, &current));
                if state
                    .compare_exchange(
                        WORKER_RUNNING,
                        WORKER_DONE,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    STUCK_WORKERS.fetch_sub(1, Ordering::AcqRel);
                }
            })
    };
    let source = || {
        current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    };
    if let Err(error) = spawned {
        return failed(
            source(),
            format!("could not start the script thread: {error}"),
        );
    }
    loop {
        let message = match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(output) => return output,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return failed(
                    source(),
                    "The script engine stopped unexpectedly".to_owned(),
                );
            }
            Err(mpsc::RecvTimeoutError::Timeout)
                if cancel.as_ref().is_some_and(CancelToken::is_cancelled) =>
            {
                CANCELLED_MESSAGE.to_owned()
            }
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                timeout_message(limit)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
        };
        // Count before the state flips so the worker's own decrement can never run first.
        STUCK_WORKERS.fetch_add(1, Ordering::AcqRel);
        if state
            .compare_exchange(
                WORKER_RUNNING,
                WORKER_ABANDONED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            STUCK_WORKERS.fetch_sub(1, Ordering::AcqRel);
            if let Ok(output) = receiver.try_recv() {
                return output;
            }
        }
        return failed(source(), message);
    }
}

/// The unmodified request, variables and response of `input` as a script output (no script ran).
fn into_output_of(input: ScriptInput) -> ScriptOutput {
    ScriptOutput {
        request: input.request,
        response: input.response,
        environment: input.environment,
        collection_variables: input.collection_variables,
        globals: input.globals,
        mutations: ScriptMutations::default(),
        local: input.local,
        tests: Vec::new(),
        console: Vec::new(),
        error: None,
        next_request: None,
        confirmation_required: None,
        cookie_jar: input.cookie_jar,
        cookie_writes: Vec::new(),
        cookie_deletes: Vec::new(),
    }
}

/// Runs the scripts of one phase on the current thread; `current` follows the label of the script
/// that is running so an abandoned phase can name it.
fn run_scripts_inline(input: ScriptInput, current: &Mutex<String>) -> ScriptOutput {
    let deadline = Instant::now() + input.limits.wall_clock_limit;
    let mut state = HostState {
        request: input.request,
        response: input.response,
        ai: input.ai,
        environment: input.environment,
        cancel: input.cancel,
        collection_variables: input.collection_variables,
        globals: input.globals,
        mutations: ScriptMutations::default(),
        iteration_data: input.iteration_data,
        local: input.local,
        tests: Vec::new(),
        console: Vec::new(),
        next_request: None,
        info: input.info,
        current_source: String::new(),
        send_requests: 0,
        limits: input.limits,
        deadline,
        http_clients: Vec::new(),
        confirmed: input.confirmed,
        confirmation_required: None,
        cookie_jar: input.cookie_jar,
        cookie_writes: Vec::new(),
        cookie_deletes: Vec::new(),
        cookie_url: None,
    };
    state.cookie_url = super::cookies::frozen_cookie_url(&state);
    let host = Rc::new(RefCell::new(state));
    let mut error = None;

    for script in input.scripts {
        if script.source.trim().is_empty() {
            continue;
        }
        *current.lock().unwrap_or_else(PoisonError::into_inner) = script.label.clone();
        {
            let mut state = host.borrow_mut();
            state.current_source.clone_from(&script.label);
            state.send_requests = 0;
        }
        HOST.with(|slot| *slot.borrow_mut() = Some(Rc::clone(&host)));
        let limits = host.borrow().limits.clone();
        let outcome = evaluate_script(&script.source, &limits);
        HOST.with(|slot| slot.borrow_mut().take());

        if let Err(mut message) = outcome {
            // A cancel or timeout aborts the script with an engine panic; report it plainly.
            if host
                .borrow()
                .cancel
                .as_ref()
                .is_some_and(CancelToken::is_cancelled)
            {
                message = CANCELLED_MESSAGE.to_owned();
            } else if Instant::now() >= deadline {
                message = timeout_message(limits.wall_clock_limit);
            }
            error = Some(ScriptError {
                source: script.label,
                message,
            });
            break;
        }
    }

    into_output(host, error)
}

/// Turns the shared host into the final output. The thread-local reference is always released
/// before this runs, so the unique-owner path is the normal one; a surviving reference is
/// reported as a script error instead of panicking.
fn into_output(host: SharedHost, error: Option<ScriptError>) -> ScriptOutput {
    let (host, error) = match Rc::try_unwrap(host) {
        Ok(cell) => (cell.into_inner(), error),
        Err(shared) => {
            let host = shared.borrow().clone();
            let error = error.or_else(|| {
                Some(ScriptError {
                    source: host.current_source.clone(),
                    message: "script host state was lost".to_owned(),
                })
            });
            (host, error)
        }
    };

    ScriptOutput {
        request: host.request,
        response: host.response,
        environment: host.environment,
        collection_variables: host.collection_variables,
        globals: host.globals,
        mutations: host.mutations,
        local: host.local,
        tests: host.tests,
        console: host.console,
        error,
        next_request: host.next_request,
        confirmation_required: host.confirmation_required,
        cookie_jar: host.cookie_jar,
        cookie_writes: host.cookie_writes,
        cookie_deletes: host.cookie_deletes,
    }
}

/// Marks text cut by [`cap_text`].
pub(super) const TRUNCATION_MARKER: &str = "… [truncated]";

/// Cuts `text` to `max_bytes` on a character boundary and marks the cut.
pub(super) fn cap_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATION_MARKER}", &text[..end])
}

/// Builds a brand-new Boa context with limits, the `__pd` bridge and the evaluated prelude.
///
/// Isolation: every script gets its own context (about 2.5 ms in release, almost all of it parsing
/// the prelude), so a script that overwrites built-ins (`JSON.parse`, `Array.prototype.map`,
/// `Object.prototype`, ...), defines a non-configurable global or leaves state behind cannot affect
/// the next one, whatever thread it runs on. Freezing the intrinsics instead would break ordinary
/// scripts (a frozen `Object.prototype` makes `obj.toString = fn` fail) and still need a reset.
fn new_context(limits: &ScriptLimits) -> Result<Context, String> {
    let mut context = Context::builder()
        .job_executor(Rc::new(SimpleJobExecutor::new()))
        .build()
        .map_err(|error| error.to_string())?;
    context
        .runtime_limits_mut()
        .set_loop_iteration_limit(limits.loop_iteration_limit);
    context
        .runtime_limits_mut()
        .set_recursion_limit(limits.recursion_limit);
    context
        .register_global_builtin_callable(
            js_string!("__pd"),
            2,
            NativeFunction::from_fn_ptr(pd_bridge),
        )
        .map_err(|error| error.to_string())?;
    context
        .eval(Source::from_bytes(PRELUDE.as_bytes()))
        .map_err(|error| error.to_string())?;
    Ok(context)
}

/// Evaluates `source` in a fresh context (see [`new_context`]) inside its own function scope.
fn evaluate_script(source: &str, limits: &ScriptLimits) -> Result<(), String> {
    let mut context = new_context(limits)?;
    let program = format!("(function () {{\n{source}\n}}).call(globalThis);");
    context
        .eval(Source::from_bytes(program.as_bytes()))
        .map_err(|error| error.to_string())?;
    // Promise jobs (async `pm.test` callbacks) run to completion; there are no timers, and the
    // phase's wall clock still bounds a job chain that never ends.
    context.run_jobs().map_err(|error| error.to_string())?;
    context
        .eval(Source::from_bytes(b"__pdSettle();"))
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn pd_bridge(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let operation = js_arg_string(args, 0, context)?;
    let payload = js_arg_string(args, 1, context)?;
    let payload = serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null);

    let result = HOST.with(|slot| {
        let Some(host) = slot.borrow().clone() else {
            return Err("script host is not active".to_owned());
        };
        let mut host = host
            .try_borrow_mut()
            .map_err(|_| "script host is busy".to_owned())?;
        if host.cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
            return Err(CANCELLED_MESSAGE.to_owned());
        }
        if Instant::now() >= host.deadline {
            return Err(timeout_message(host.limits.wall_clock_limit));
        }
        handle_operation(&mut host, &operation, &payload)
    });

    match result {
        Ok(value) => Ok(JsString::from(value).into()),
        // An engine error cannot be caught from JavaScript, so a cancel stops the whole script
        // instead of reaching the `pm.sendRequest` callback as an ordinary error.
        Err(message) if message == CANCELLED_MESSAGE || message.starts_with(TIMEOUT_PREFIX) => {
            Err(PanicError::new(message).into())
        }
        Err(message) => Err(JsNativeError::typ().with_message(message).into()),
    }
}

fn js_arg_string(args: &[JsValue], index: usize, context: &mut Context) -> JsResult<String> {
    let value = args.get(index).cloned().unwrap_or_else(JsValue::undefined);
    Ok(value.to_string(context)?.to_std_string_escaped())
}
