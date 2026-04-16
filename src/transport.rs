//! Async transport abstractions.
//!
//! The [`Transport`] marker trait unifies tokio-based byte streams so the
//! [`Session`](crate::session::Session) can drive any of them. Three
//! concrete transports are provided:
//!
//! - [`SerialTransport`] — `tokio-serial` USB CDC-ACM.
//! - [`UnixSocketTransport`] — mux daemon over `tokio::net::UnixStream`.
//! - [`TcpTransport`] — mux daemon over `tokio::net::TcpStream`.
//!
//! Most callers use [`AnyTransport`] so the concrete variant is decided
//! at `connect()` time without leaking type parameters into user code.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio_serial::{SerialPortBuilderExt, SerialStream};

use crate::errors::{ClientError, ClientResult};

/// Marker for any async byte-stream we can carry DongLoRa Protocol frames over.
///
/// This is intentionally a blanket trait so the [`Session`](crate::session::Session)
/// can accept `SerialStream`, `UnixStream`, `TcpStream`, or the
/// type-erased [`AnyTransport`] uniformly.
pub trait Transport: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Transport for T {}

// ── Serial transport ───────────────────────────────────────────────

/// Direct USB serial port connection (tokio-serial).
pub struct SerialTransport {
    inner: SerialStream,
}

impl SerialTransport {
    /// Open a serial port. Baud rate is irrelevant for USB CDC-ACM but
    /// `tokio-serial` still demands a value; 115200 is conventional.
    pub fn open(path: &str) -> ClientResult<Self> {
        let inner = tokio_serial::new(path, 115_200)
            .open_native_async()
            .map_err(|e| ClientError::Other(format!("failed to open serial port {path}: {e}")))?;
        Ok(Self { inner })
    }
}

impl AsyncRead for SerialTransport {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for SerialTransport {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

// ── Unix socket transport ──────────────────────────────────────────

/// Mux daemon connection over a Unix domain socket.
#[cfg(unix)]
pub struct UnixSocketTransport {
    inner: UnixStream,
}

#[cfg(unix)]
impl UnixSocketTransport {
    /// Connect to the mux daemon at `path`.
    pub async fn connect(path: &str) -> ClientResult<Self> {
        let inner = UnixStream::connect(path)
            .await
            .map_err(|e| ClientError::Other(format!("failed to connect to mux socket {path}: {e}")))?;
        Ok(Self { inner })
    }
}

#[cfg(unix)]
impl AsyncRead for UnixSocketTransport {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

#[cfg(unix)]
impl AsyncWrite for UnixSocketTransport {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

// ── TCP transport ──────────────────────────────────────────────────

/// Mux daemon connection over TCP. Uses `TCP_NODELAY` because DongLoRa Protocol frames
/// are small and latency-sensitive.
pub struct TcpTransport {
    inner: TcpStream,
}

impl TcpTransport {
    /// Connect to `host:port`. A failed connect attempt returns an error
    /// immediately; no internal retries (callers decide policy).
    pub async fn connect(host: &str, port: u16, timeout: Duration) -> ClientResult<Self> {
        let addr = format!("{host}:{port}");
        let fut = TcpStream::connect(&addr);
        let stream = tokio::time::timeout(timeout, fut)
            .await
            .map_err(|_| ClientError::Timeout { what: "tcp connect" })?
            .map_err(|e| ClientError::Other(format!("failed to connect to {addr}: {e}")))?;
        stream.set_nodelay(true).map_err(|e| ClientError::Other(format!("failed to set TCP_NODELAY: {e}")))?;
        Ok(Self { inner: stream })
    }
}

impl AsyncRead for TcpTransport {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for TcpTransport {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

// ── AnyTransport ───────────────────────────────────────────────────

/// Type-erased transport returned by [`crate::connect`]. One of the three
/// concrete variants depending on which transport the auto-discovery chain
/// succeeded on.
pub enum AnyTransport {
    Serial(SerialTransport),
    #[cfg(unix)]
    Unix(UnixSocketTransport),
    Tcp(TcpTransport),
}

impl AsyncRead for AnyTransport {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Serial(t) => Pin::new(t).poll_read(cx, buf),
            #[cfg(unix)]
            Self::Unix(t) => Pin::new(t).poll_read(cx, buf),
            Self::Tcp(t) => Pin::new(t).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for AnyTransport {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Serial(t) => Pin::new(t).poll_write(cx, buf),
            #[cfg(unix)]
            Self::Unix(t) => Pin::new(t).poll_write(cx, buf),
            Self::Tcp(t) => Pin::new(t).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Serial(t) => Pin::new(t).poll_flush(cx),
            #[cfg(unix)]
            Self::Unix(t) => Pin::new(t).poll_flush(cx),
            Self::Tcp(t) => Pin::new(t).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Serial(t) => Pin::new(t).poll_shutdown(cx),
            #[cfg(unix)]
            Self::Unix(t) => Pin::new(t).poll_shutdown(cx),
            Self::Tcp(t) => Pin::new(t).poll_shutdown(cx),
        }
    }
}
