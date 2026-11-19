//! Thin Tokio ↔ GPUI executor bridge and frame-coalesced stream subscriptions.
//!
//! All core work executes on [`pingdeck_desktop::runtime::DesktopRuntime`]'s multi-threaded Tokio
//! runtime (blocking SQLite/filesystem ops on Tokio's blocking pool), and results return cleanly
//! to GPUI entities on the main UI thread via `cx.spawn` + `WeakEntity::update`.

use std::future::Future;
use std::sync::Arc;

use gpui_kit::{App, Context, Global, Task, WeakEntity};
use pingdeck_desktop::core::app::AppState;
use pingdeck_desktop::runtime::DesktopRuntime;
use pingdeck_desktop::streams::{AbortOnDrop, StreamReceiver};

/// GPUI global holding the shared [`DesktopRuntime`].
pub struct CoreBridge {
    runtime: Arc<DesktopRuntime>,
}

impl Global for CoreBridge {}

impl CoreBridge {
    #[must_use]
    pub fn new(runtime: Arc<DesktopRuntime>) -> Self {
        Self { runtime }
    }

    #[must_use]
    pub fn runtime(&self) -> &Arc<DesktopRuntime> {
        &self.runtime
    }
}

/// Accesses the shared [`DesktopRuntime`] from any GPUI `App` context.
#[must_use]
pub fn runtime(cx: &App) -> Arc<DesktopRuntime> {
    Arc::clone(cx.global::<CoreBridge>().runtime())
}

/// Runs a synchronous core closure (`AppState -> T`) on Tokio's blocking pool and applies `on_done`
/// on the GPUI foreground thread once complete.
pub fn spawn_blocking_into<V, T, Work, Done>(
    cx: &mut Context<V>,
    work: Work,
    on_done: Done,
) -> Task<()>
where
    V: 'static,
    T: Send + 'static,
    Work: FnOnce(&AppState) -> T + Send + 'static,
    Done: FnOnce(&mut V, T, &mut Context<V>) + 'static,
{
    let rt = runtime(cx);
    let join = rt.run_state(work);
    cx.spawn(async move |this: WeakEntity<V>, cx| {
        if let Ok(value) = join.await {
            let _ = this.update(cx, |view, cx| {
                on_done(view, value, cx);
            });
        }
    })
}

/// Runs an async core future (`AppState -> Future<Output = T>`) on Tokio's multi-threaded runtime
/// and delivers `T` onto `Entity<V>` on the GPUI foreground thread.
pub fn spawn_async_into<V, F, T, Build, Done>(
    cx: &mut Context<V>,
    build: Build,
    on_done: Done,
) -> Task<()>
where
    V: 'static,
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
    Build: FnOnce(AppState) -> F,
    Done: FnOnce(&mut V, T, &mut Context<V>) + 'static,
{
    let rt = runtime(cx);
    let state = rt.state_cloned();
    let join = rt.spawn_async(build(state));
    cx.spawn(async move |this: WeakEntity<V>, cx| {
        if let Ok(value) = join.await {
            let _ = this.update(cx, |view, cx| {
                on_done(view, value, cx);
            });
        }
    })
}

/// Subscribes an entity to a typed [`StreamReceiver<E>`], greedily batching ready events so bursts
/// coalesce into a single GPUI update / `cx.notify()` pass. Dropping the returned [`Task`] aborts
/// the inner Tokio recv task and drops the receiver, immediately closing the core [`EventSink`].
pub fn subscribe_stream_into<V, E, OnBatch>(
    cx: &mut Context<V>,
    mut rx: StreamReceiver<E>,
    max_batch: usize,
    mut on_batch: OnBatch,
) -> Task<()>
where
    V: 'static,
    E: Send + 'static,
    OnBatch: FnMut(&mut V, Vec<E>, &mut Context<V>) + 'static,
{
    let handle = runtime(cx).tokio_handle();
    cx.spawn(async move |this: WeakEntity<V>, cx| {
        loop {
            let guard = AbortOnDrop::new(handle.spawn(async move {
                let batch = rx.recv_batch(max_batch).await;
                (batch, rx)
            }));
            let Some((Some(batch), next_rx)) = guard.wait().await else {
                break;
            };
            rx = next_rx;
            let alive = this
                .update(cx, |view, cx| {
                    on_batch(view, batch, cx);
                })
                .is_ok();
            if !alive {
                break;
            }
        }
    })
}
