//! The gateway's listening socket and the guard on every accepted one.
//!
//! Single responsibility: socket-level hygiene that hyper's HTTP/1 server does not have. The
//! listener enables `SO_KEEPALIVE` (accepted sockets inherit it), so a half-dead peer is noticed
//! by the OS. [`StallGuard`] fails the connection when the peer stops accepting response bytes:
//! hyper stops polling a response body once its write buffer is full, so without it a client that
//! sends a streaming request and never reads would pin its connection permit, its in-flight slot
//! and the upstream stream forever (the body, and with it the upstream stream, is dropped when
//! the connection ends).

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpSocket, TcpStream};
use tokio::time::Sleep;

/// Pending connections the OS queues for the accept loop.
pub(super) const LISTEN_BACKLOG: u32 = 1024;

/// How long a client may leave a response byte unaccepted (a full socket buffer it does not
/// drain) before the connection is dropped. A reader that is merely slow keeps making progress
/// and never trips it.
pub(super) const WRITE_STALL: Duration = Duration::from_secs(60);

/// Binds `address` with `SO_KEEPALIVE` set; [`TcpSocket::listen`] (inside the gateway runtime)
/// makes it a listener.
///
/// # Errors
/// The I/O error of creating, configuring or binding the socket (`AddrInUse` when the port is
/// taken).
pub(super) fn bind(address: SocketAddr) -> io::Result<TcpSocket> {
    let socket = TcpSocket::new_v4()?;
    socket.set_keepalive(true)?;
    // What `std::net::TcpListener::bind` does on Unix; on Windows it means port stealing.
    #[cfg(unix)]
    socket.set_reuseaddr(true)?;
    socket.bind(address)?;
    Ok(socket)
}

/// An accepted socket whose writes fail with [`io::ErrorKind::TimedOut`] once they have made no
/// progress for `limit`.
pub(super) struct StallGuard {
    inner: TcpStream,
    limit: Duration,
    /// Runs while a write is pending; any progress clears it.
    stalled: Option<Pin<Box<Sleep>>>,
}

impl StallGuard {
    pub(super) fn new(inner: TcpStream, limit: Duration) -> Self {
        Self {
            inner,
            limit,
            stalled: None,
        }
    }

    /// Passes a write-side poll result on: progress stops the stall clock, `Pending` runs it, and
    /// an expired clock turns into an error.
    fn watch<T>(
        &mut self,
        cx: &mut Context<'_>,
        polled: Poll<io::Result<T>>,
    ) -> Poll<io::Result<T>> {
        if polled.is_ready() {
            self.stalled = None;
            return polled;
        }
        let limit = self.limit;
        let clock = self
            .stalled
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(limit)));
        if clock.as_mut().poll(cx).is_ready() {
            self.stalled = None;
            return Poll::Ready(Err(io::ErrorKind::TimedOut.into()));
        }
        Poll::Pending
    }
}

impl AsyncRead for StallGuard {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for StallGuard {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let polled = Pin::new(&mut self.inner).poll_write(cx, buf);
        self.watch(cx, polled)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let polled = Pin::new(&mut self.inner).poll_write_vectored(cx, bufs);
        self.watch(cx, polled)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let polled = Pin::new(&mut self.inner).poll_flush(cx);
        self.watch(cx, polled)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_listening_socket_has_keepalive_and_reports_its_port() {
        let socket = bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        assert!(socket.keepalive().unwrap());
        assert_ne!(socket.local_addr().unwrap().port(), 0);
    }
}
