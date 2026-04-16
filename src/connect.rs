//! High-level `connect()` entry points.
//!
//! Mirrors the Python client's `connect()` behaviour: explicit port → TCP
//! mux (env var) → Unix mux (socket file) → direct USB serial. The first
//! successful mux connection within a process is "sticky": subsequent
//! calls stay on the mux even if the socket temporarily disappears.
//!
//! Public surface:
//!
//! - [`ConnectOptions`] — builder for `port`, `timeout`, `config`,
//!   `auto_configure`, `keepalive`.
//! - [`connect`] — one-shot convenience: `connect().await?` returns a
//!   [`Dongle`] with defaults.
//! - [`connect_with`] — takes a populated [`ConnectOptions`].
//! - [`try_connect`] — like `connect` but never blocks for a USB device;
//!   returns an error instead of polling forever.
//! - [`connect_mux_auto`] — mux-only (TCP env var, then Unix socket);
//!   never falls back to USB.
//! - [`mux_unix_connect`] / [`mux_tcp_connect`] — explicit single-transport.
//! - [`default_socket_path`] / [`find_mux_socket`] — helpers.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use donglora_protocol::Modulation;
use tracing::{debug, info};

use crate::discovery;
use crate::dongle::{Dongle, TransportKind};
use crate::errors::{ClientError, ClientResult};
use crate::session::Session;
#[cfg(unix)]
use crate::transport::UnixSocketTransport;
use crate::transport::{AnyTransport, SerialTransport, TcpTransport, Transport};

/// Set once this process connects via mux. All future auto-connects stay
/// on mux rather than falling through to direct USB.
static USED_MUX: AtomicBool = AtomicBool::new(false);

/// Default per-command timeout when the caller doesn't override it.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

/// Builder for [`connect_with`].
///
/// ```no_run
/// # async fn demo() -> Result<(), donglora_client::ClientError> {
/// use donglora_client::{ConnectOptions, connect_with};
/// let dongle = connect_with(
///     ConnectOptions::default()
///         .port("/dev/ttyACM0")
///         .keepalive(false),
/// ).await?;
/// # drop(dongle); Ok(()) }
/// ```
#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
    port: Option<String>,
    timeout: Option<Duration>,
    config: Option<Modulation>,
    auto_configure: bool,
    keepalive: bool,
}

impl ConnectOptions {
    /// Explicit serial port path; skips the mux + auto-discovery chain
    /// entirely.
    #[must_use]
    pub fn port(mut self, path: impl Into<String>) -> Self {
        self.port = Some(path.into());
        self
    }

    /// Per-command timeout for the connect-time PING + GET_INFO +
    /// SET_CONFIG (if any). Default is 2 s.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Supply an initial radio config. When paired with `auto_configure`
    /// (enabled by default via [`Self::auto_configure`]), the config is
    /// applied immediately after `GET_INFO`. Callers that want full
    /// manual control over configuration should leave this as `None`.
    #[must_use]
    pub fn config(mut self, modulation: Modulation) -> Self {
        self.config = Some(modulation);
        self.auto_configure = true;
        self
    }

    /// Whether to apply the [`Self::config`] automatically at connect
    /// time. Default: `true` if a config was supplied, `false` otherwise.
    #[must_use]
    pub fn auto_configure(mut self, enabled: bool) -> Self {
        self.auto_configure = enabled;
        self
    }

    /// Enable the background keepalive task. Default: `true`. Disable
    /// only when the caller manages session liveness themselves (e.g.
    /// a tight send/recv loop that hits the device more often than the
    /// 1 s inactivity timer).
    #[must_use]
    pub fn keepalive(mut self, enabled: bool) -> Self {
        self.keepalive = enabled;
        self
    }
}

/// Convenience constructor — equivalent to `ConnectOptions::default()`
/// but with `keepalive = true`, matching the Python client's defaults.
impl ConnectOptions {
    /// Default options with keepalive enabled.
    #[must_use]
    pub fn new() -> Self {
        Self { keepalive: true, ..Self::default() }
    }
}

/// Resolve the mux socket path in priority order:
///
/// 1. `$DONGLORA_MUX`
/// 2. `$XDG_RUNTIME_DIR/donglora/mux.sock`
/// 3. `/tmp/donglora-mux.sock`
#[must_use]
pub fn default_socket_path() -> String {
    if let Ok(env) = std::env::var("DONGLORA_MUX") {
        return env;
    }
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        return format!("{xdg}/donglora/mux.sock");
    }
    "/tmp/donglora-mux.sock".to_string()
}

/// Check for a mux socket in the standard places and return the first
/// one that exists. Returns `None` if no socket is live.
#[must_use]
pub fn find_mux_socket() -> Option<String> {
    if let Ok(env) = std::env::var("DONGLORA_MUX") {
        if Path::new(&env).exists() {
            return Some(env);
        }
        return None;
    }
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        let p = format!("{xdg}/donglora/mux.sock");
        if Path::new(&p).exists() {
            return Some(p);
        }
    }
    let p = "/tmp/donglora-mux.sock";
    if Path::new(p).exists() {
        return Some(p.to_string());
    }
    None
}

// ── Connect entry points ──────────────────────────────────────────

/// One-shot convenience: run the full auto-discovery chain with default
/// options and return a ready [`Dongle`].
///
/// Blocks (asynchronously) waiting for a USB device if no mux is
/// available and no device is currently present. Use [`try_connect`] if
/// you want a single non-blocking scan instead.
pub async fn connect() -> ClientResult<Dongle> {
    connect_with(ConnectOptions::new()).await
}

/// Connect with the given [`ConnectOptions`].
pub async fn connect_with(opts: ConnectOptions) -> ClientResult<Dongle> {
    let timeout = opts.timeout.unwrap_or(DEFAULT_TIMEOUT);

    // Explicit port always bypasses mux.
    if let Some(port) = opts.port.as_deref() {
        debug!("opening serial port {port}");
        let transport = SerialTransport::open(port)?;
        return finalize(AnyTransport::Serial(transport), TransportKind::Serial(port.to_string()), &opts, timeout)
            .await;
    }

    // Sticky: once we've used a mux, stay on mux (wait if socket
    // disappeared — a mux restart).
    if USED_MUX.load(Ordering::Relaxed) {
        return connect_mux_sticky(&opts, timeout).await;
    }

    // Try TCP mux via env var.
    if let Some((transport, endpoint)) = try_tcp_env(timeout).await {
        USED_MUX.store(true, Ordering::Relaxed);
        return finalize(AnyTransport::Tcp(transport), TransportKind::MuxTcp(endpoint), &opts, timeout).await;
    }

    // Try existing Unix mux socket.
    #[cfg(unix)]
    if let Some(path) = find_mux_socket() {
        debug!("connecting to Unix mux at {path}");
        let transport = UnixSocketTransport::connect(&path).await?;
        USED_MUX.store(true, Ordering::Relaxed);
        return finalize(AnyTransport::Unix(transport), TransportKind::MuxUnix(path), &opts, timeout).await;
    }

    // Direct USB: wait indefinitely for a device.
    let port = match discovery::find_port() {
        Some(p) => p,
        None => discovery::wait_for_device().await,
    };
    debug!("opening serial port {port}");
    let transport = SerialTransport::open(&port)?;
    finalize(AnyTransport::Serial(transport), TransportKind::Serial(port), &opts, timeout).await
}

/// Like [`connect`] but returns an error rather than blocking if no USB
/// device is present. Mux-sticky behaviour still applies.
pub async fn try_connect() -> ClientResult<Dongle> {
    try_connect_with(ConnectOptions::new()).await
}

/// Non-blocking variant of [`connect_with`].
pub async fn try_connect_with(opts: ConnectOptions) -> ClientResult<Dongle> {
    let timeout = opts.timeout.unwrap_or(DEFAULT_TIMEOUT);

    if let Some(port) = opts.port.as_deref() {
        let transport = SerialTransport::open(port)?;
        return finalize(AnyTransport::Serial(transport), TransportKind::Serial(port.to_string()), &opts, timeout)
            .await;
    }

    if USED_MUX.load(Ordering::Relaxed) {
        let path =
            find_mux_socket().ok_or_else(|| ClientError::Other("mux not available (waiting for restart)".into()))?;
        #[cfg(unix)]
        {
            let transport = UnixSocketTransport::connect(&path).await?;
            return finalize(AnyTransport::Unix(transport), TransportKind::MuxUnix(path), &opts, timeout).await;
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            return Err(ClientError::Other("Unix mux requires a unix target".into()));
        }
    }

    if let Some((transport, endpoint)) = try_tcp_env(timeout).await {
        USED_MUX.store(true, Ordering::Relaxed);
        return finalize(AnyTransport::Tcp(transport), TransportKind::MuxTcp(endpoint), &opts, timeout).await;
    }

    #[cfg(unix)]
    if let Some(path) = find_mux_socket() {
        let transport = UnixSocketTransport::connect(&path).await?;
        USED_MUX.store(true, Ordering::Relaxed);
        return finalize(AnyTransport::Unix(transport), TransportKind::MuxUnix(path), &opts, timeout).await;
    }

    let port = discovery::find_port()
        .ok_or_else(|| ClientError::Other("no DongLoRa device found (no mux, no USB device)".into()))?;
    let transport = SerialTransport::open(&port)?;
    finalize(AnyTransport::Serial(transport), TransportKind::Serial(port), &opts, timeout).await
}

/// Mux-only connect. Never falls back to direct USB; returns an error if
/// no mux (TCP via env var or Unix socket) is reachable.
pub async fn connect_mux_auto() -> ClientResult<Dongle> {
    connect_mux_auto_with(ConnectOptions::new()).await
}

/// Mux-only connect, with caller-supplied options.
pub async fn connect_mux_auto_with(opts: ConnectOptions) -> ClientResult<Dongle> {
    let timeout = opts.timeout.unwrap_or(DEFAULT_TIMEOUT);
    if let Some((transport, endpoint)) = try_tcp_env(timeout).await {
        USED_MUX.store(true, Ordering::Relaxed);
        return finalize(AnyTransport::Tcp(transport), TransportKind::MuxTcp(endpoint), &opts, timeout).await;
    }
    #[cfg(unix)]
    {
        let path = find_mux_socket().ok_or_else(|| ClientError::Other("no mux socket found".into()))?;
        let transport = UnixSocketTransport::connect(&path).await?;
        USED_MUX.store(true, Ordering::Relaxed);
        finalize(AnyTransport::Unix(transport), TransportKind::MuxUnix(path), &opts, timeout).await
    }
    #[cfg(not(unix))]
    Err(ClientError::Other("mux-only mode requires Unix socket support or DONGLORA_MUX_TCP".into()))
}

/// Connect to an explicit Unix mux socket. Useful for tests and CLIs
/// that want to bypass the auto-discovery chain.
#[cfg(unix)]
pub async fn mux_unix_connect(path: &str) -> ClientResult<Dongle> {
    let transport = UnixSocketTransport::connect(path).await?;
    USED_MUX.store(true, Ordering::Relaxed);
    finalize(
        AnyTransport::Unix(transport),
        TransportKind::MuxUnix(path.to_string()),
        &ConnectOptions::new(),
        DEFAULT_TIMEOUT,
    )
    .await
}

/// Connect to an explicit TCP mux endpoint.
pub async fn mux_tcp_connect(host: &str, port: u16) -> ClientResult<Dongle> {
    let transport = TcpTransport::connect(host, port, DEFAULT_TIMEOUT).await?;
    USED_MUX.store(true, Ordering::Relaxed);
    finalize(
        AnyTransport::Tcp(transport),
        TransportKind::MuxTcp(format!("{host}:{port}")),
        &ConnectOptions::new(),
        DEFAULT_TIMEOUT,
    )
    .await
}

// ── internals ─────────────────────────────────────────────────────

async fn connect_mux_sticky(opts: &ConnectOptions, timeout: Duration) -> ClientResult<Dongle> {
    if let Some((transport, endpoint)) = try_tcp_env(timeout).await {
        return finalize(AnyTransport::Tcp(transport), TransportKind::MuxTcp(endpoint), opts, timeout).await;
    }
    #[cfg(unix)]
    {
        let path = default_socket_path();
        let mut warned = false;
        loop {
            if Path::new(&path).exists() {
                let transport = UnixSocketTransport::connect(&path).await?;
                return finalize(AnyTransport::Unix(transport), TransportKind::MuxUnix(path), opts, timeout).await;
            }
            if !warned {
                info!("waiting for mux at {path} ...");
                warned = true;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    #[cfg(not(unix))]
    Err(ClientError::Other("no mux endpoint available".into()))
}

async fn try_tcp_env(timeout: Duration) -> Option<(TcpTransport, String)> {
    let tcp = std::env::var("DONGLORA_MUX_TCP").ok()?;
    let (host, port) = parse_tcp_endpoint(&tcp)?;
    match TcpTransport::connect(&host, port, timeout).await {
        Ok(t) => {
            debug!("connected to TCP mux at {host}:{port}");
            Some((t, format!("{host}:{port}")))
        }
        Err(e) => {
            debug!("DONGLORA_MUX_TCP connect failed: {e}");
            None
        }
    }
}

fn parse_tcp_endpoint(addr: &str) -> Option<(String, u16)> {
    if let Some((h, p)) = addr.rsplit_once(':') {
        let host = if h.is_empty() { "localhost".to_string() } else { h.to_string() };
        let port: u16 = p.parse().ok()?;
        Some((host, port))
    } else {
        let port: u16 = addr.parse().ok()?;
        Some(("localhost".to_string(), port))
    }
}

async fn finalize<T: Transport>(
    transport: T,
    kind: TransportKind,
    opts: &ConnectOptions,
    timeout: Duration,
) -> ClientResult<Dongle> {
    let session = Session::spawn(transport);
    // Probe: PING validates the connection, GET_INFO caches device info.
    session.ping(timeout).await?;
    let info = session.get_info(timeout).await?;

    let applied = if opts.auto_configure {
        match opts.config {
            Some(m) => {
                let result = session.set_config(m, timeout).await?;
                Some(result.current)
            }
            None => None,
        }
    } else {
        None
    };

    Ok(Dongle::new(session, info, kind, applied, opts.keepalive))
}
