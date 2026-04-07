//! Connection auto-detection and mux client helpers.
//!
//! The [`connect`] function tries mux connections first (TCP via env var, then
//! Unix socket), falling back to direct USB serial. This matches the Python
//! client's `connect()` behavior.

use std::time::Duration;

use tracing::debug;

use crate::client::Client;
use crate::discovery;
use crate::transport::{AnyTransport, MuxTransport, SerialTransport};

/// Default read timeout for connections.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

/// Resolve the mux socket path in priority order.
///
/// 1. `$DONGLORA_MUX` environment variable
/// 2. `$XDG_RUNTIME_DIR/donglora/mux.sock`
/// 3. `/tmp/donglora-mux.sock`
pub fn default_socket_path() -> String {
    if let Ok(env) = std::env::var("DONGLORA_MUX") {
        return env;
    }
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        return format!("{xdg}/donglora/mux.sock");
    }
    "/tmp/donglora-mux.sock".to_string()
}

/// Find an existing mux socket path, or `None` if no socket file exists.
fn find_mux_socket() -> Option<String> {
    if let Ok(env) = std::env::var("DONGLORA_MUX") {
        if std::path::Path::new(&env).exists() {
            return Some(env);
        }
        return None;
    }
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        let p = format!("{xdg}/donglora/mux.sock");
        if std::path::Path::new(&p).exists() {
            return Some(p);
        }
    }
    let p = "/tmp/donglora-mux.sock";
    if std::path::Path::new(p).exists() {
        return Some(p.to_string());
    }
    None
}

/// Connect to the mux daemon via Unix domain socket.
#[cfg(unix)]
pub fn mux_connect(path: Option<&str>, timeout: Duration) -> anyhow::Result<Client<MuxTransport>> {
    let path = match path {
        Some(p) => p.to_string(),
        None => find_mux_socket().ok_or_else(|| anyhow::anyhow!("no mux socket found"))?,
    };
    let transport = MuxTransport::unix(&path, timeout)?;
    let mut client = Client::new(transport);
    client.validate()?;
    Ok(client)
}

/// Connect to the mux daemon via TCP.
pub fn mux_tcp_connect(host: &str, port: u16, timeout: Duration) -> anyhow::Result<Client<MuxTransport>> {
    let transport = MuxTransport::tcp(host, port, timeout)?;
    let mut client = Client::new(transport);
    client.validate()?;
    Ok(client)
}

/// Auto-detect and connect to a DongLoRa device.
///
/// Priority:
/// 1. `DONGLORA_MUX_TCP` env var → TCP mux connection
/// 2. Unix socket mux (if socket file exists)
/// 3. Direct USB serial (auto-detect by VID:PID, blocks until device appears)
///
/// If `port` is `Some`, skips mux detection and connects directly to that serial port.
pub fn connect(port: Option<&str>, timeout: Duration) -> anyhow::Result<Client<AnyTransport>> {
    // If explicit port given, go direct
    if let Some(port) = port {
        debug!("opening serial port {port}");
        let transport = SerialTransport::open(port, timeout)?;
        let mut client = Client::new(AnyTransport::Serial(transport));
        client.validate()?;
        return Ok(client);
    }

    // Try TCP mux via environment variable
    if let Ok(tcp) = std::env::var("DONGLORA_MUX_TCP")
        && let Some(transport) = try_tcp_mux(&tcp, timeout)
    {
        debug!("connected to TCP mux at {tcp}");
        let mut client = Client::new(AnyTransport::Mux(transport));
        client.validate()?;
        return Ok(client);
    }

    // Try Unix socket mux — if a mux socket exists, commit to it. Never fall through
    // to USB serial when a mux is (or was) running. This prevents a client from
    // stealing the serial port while the mux is temporarily reconnecting.
    #[cfg(unix)]
    if let Some(path) = find_mux_socket() {
        debug!("mux socket found at {path} — connecting via mux only");
        let transport = MuxTransport::unix(&path, timeout)?;
        let mut client = Client::new(AnyTransport::Mux(transport));
        client.validate()?;
        return Ok(client);
    }

    // Direct USB serial — only reached when no mux socket exists at all
    let port_path = discovery::find_port().unwrap_or_else(discovery::wait_for_device);
    debug!("opening serial port {port_path}");
    let transport = SerialTransport::open(&port_path, timeout)?;
    let mut client = Client::new(AnyTransport::Serial(transport));
    client.validate()?;
    Ok(client)
}

/// Convenience: connect with default timeout.
pub fn connect_default() -> anyhow::Result<Client<AnyTransport>> {
    connect(None, DEFAULT_TIMEOUT)
}

/// Connect to a mux daemon only (TCP via env var, then Unix socket).
///
/// Unlike [`connect`], this **never** falls back to direct USB serial.
/// Returns an error if no mux is reachable — the caller can retry with backoff.
///
/// This is the Rust equivalent of the Python client's "sticky mux" behavior:
/// once you decide to use the mux, you stay on the mux.
pub fn connect_mux_auto(timeout: Duration) -> anyhow::Result<Client<AnyTransport>> {
    // Try TCP mux via environment variable.
    if let Ok(tcp) = std::env::var("DONGLORA_MUX_TCP")
        && let Some(transport) = try_tcp_mux(&tcp, timeout)
    {
        debug!("connected to TCP mux at {tcp}");
        let mut client = Client::new(AnyTransport::Mux(transport));
        client.validate()?;
        return Ok(client);
    }

    // Try Unix socket mux.
    #[cfg(unix)]
    {
        let path = find_mux_socket().ok_or_else(|| anyhow::anyhow!("no mux socket found"))?;
        let transport = MuxTransport::unix(&path, timeout)?;
        debug!("connected to mux socket at {path}");
        let mut client = Client::new(AnyTransport::Mux(transport));
        client.validate()?;
        Ok(client)
    }

    #[cfg(not(unix))]
    anyhow::bail!("mux-only mode requires Unix socket support or DONGLORA_MUX_TCP")
}

/// Like [`connect`] but returns an error instead of blocking when no USB
/// device is present. Suitable for callers with their own retry logic.
///
/// Runs the same fallback chain as `connect(None, timeout)` — TCP mux, Unix
/// socket mux, then direct USB serial — but uses a single non-blocking scan
/// instead of polling indefinitely for a USB device.
pub fn try_connect(timeout: Duration) -> anyhow::Result<Client<AnyTransport>> {
    // Try TCP mux via environment variable
    if let Ok(tcp) = std::env::var("DONGLORA_MUX_TCP")
        && let Some(transport) = try_tcp_mux(&tcp, timeout)
    {
        debug!("connected to TCP mux at {tcp}");
        let mut client = Client::new(AnyTransport::Mux(transport));
        client.validate()?;
        return Ok(client);
    }

    // Try Unix socket mux — commit to it if socket exists (same rationale as connect())
    #[cfg(unix)]
    if let Some(path) = find_mux_socket() {
        debug!("mux socket found at {path} — connecting via mux only");
        let transport = MuxTransport::unix(&path, timeout)?;
        let mut client = Client::new(AnyTransport::Mux(transport));
        client.validate()?;
        return Ok(client);
    }

    // Direct USB serial — single non-blocking scan (only when no mux socket exists)
    let port_path =
        discovery::find_port().ok_or_else(|| anyhow::anyhow!("no DongLoRa device found (no mux, no USB device)"))?;
    debug!("opening serial port {port_path}");
    let transport = SerialTransport::open(&port_path, timeout)?;
    let mut client = Client::new(AnyTransport::Serial(transport));
    client.validate()?;
    Ok(client)
}

fn try_tcp_mux(addr: &str, timeout: Duration) -> Option<MuxTransport> {
    let (host, port) = if let Some((h, p)) = addr.rsplit_once(':') {
        let host = if h.is_empty() { "localhost" } else { h };
        let port: u16 = p.parse().ok()?;
        (host.to_string(), port)
    } else {
        let port: u16 = addr.parse().ok()?;
        ("localhost".to_string(), port)
    };
    MuxTransport::tcp(&host, port, timeout).ok()
}
