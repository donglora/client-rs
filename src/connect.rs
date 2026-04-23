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

use donglora_protocol::{Info, LoRaConfig, Modulation};
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
                let prepared = prepare_config(&info, m)?;
                let result = session.set_config(prepared, timeout).await?;
                Some(result.current)
            }
            None => None,
        }
    } else {
        None
    };

    Ok(Dongle::new(session, info, kind, applied, opts.keepalive))
}

/// Validate and auto-adjust `modulation` against the device's advertised caps.
///
/// Per-field policy:
///
/// * `tx_power_dbm`: clamped into `[tx_power_min_dbm, tx_power_max_dbm]`.
///   A clamp is logged at INFO — "give me max power" quietly returning
///   less is the universally-expected behaviour and not worth a hard error.
/// * `freq_hz`: rejected with [`ClientError::ConfigNotSupported`] when
///   outside `[freq_min_hz, freq_max_hz]`. Silently shifting a 915 MHz
///   request to 868 MHz (or vice versa) would cross regulatory boundaries.
/// * `sf`, `bw`: rejected with [`ClientError::ConfigNotSupported`] when
///   the corresponding capability bit isn't set. These change airtime and
///   sensitivity dramatically; silent substitution is more confusing
///   than helpful.
///
/// Non-LoRa modulations pass through untouched — the firmware rejects
/// unsupported modulation IDs with `EMODULATION` on its own.
pub(crate) fn prepare_config(info: &Info, modulation: Modulation) -> ClientResult<Modulation> {
    let Modulation::LoRa(cfg) = modulation else {
        return Ok(modulation);
    };
    Ok(Modulation::LoRa(prepare_lora_config(info, cfg)?))
}

fn prepare_lora_config(info: &Info, mut cfg: LoRaConfig) -> ClientResult<LoRaConfig> {
    if cfg.freq_hz < info.freq_min_hz || cfg.freq_hz > info.freq_max_hz {
        return Err(ClientError::ConfigNotSupported {
            reason: format!(
                "frequency {} Hz outside device range [{}, {}] Hz",
                cfg.freq_hz, info.freq_min_hz, info.freq_max_hz
            ),
        });
    }

    if info.supported_sf_bitmap & (1u16 << cfg.sf) == 0 {
        let supported: Vec<u8> = (0u8..16).filter(|i| info.supported_sf_bitmap & (1u16 << i) != 0).collect();
        return Err(ClientError::ConfigNotSupported {
            reason: format!("SF{} not supported by this device (supports SF{:?})", cfg.sf, supported),
        });
    }

    let bw_bit = cfg.bw.as_u8();
    if info.supported_bw_bitmap & (1u16 << bw_bit) == 0 {
        return Err(ClientError::ConfigNotSupported {
            reason: format!(
                "bandwidth {:?} (bit {}) not in supported_bw_bitmap 0x{:04X}",
                cfg.bw, bw_bit, info.supported_bw_bitmap
            ),
        });
    }

    if cfg.tx_power_dbm > info.tx_power_max_dbm {
        info!(requested = cfg.tx_power_dbm, device_max = info.tx_power_max_dbm, "clamping tx_power_dbm to device max");
        cfg.tx_power_dbm = info.tx_power_max_dbm;
    } else if cfg.tx_power_dbm < info.tx_power_min_dbm {
        info!(requested = cfg.tx_power_dbm, device_min = info.tx_power_min_dbm, "clamping tx_power_dbm to device min");
        cfg.tx_power_dbm = info.tx_power_min_dbm;
    }

    Ok(cfg)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use donglora_protocol::{
        FskConfig, LoRaBandwidth, LoRaCodingRate, LoRaHeaderMode, MAX_MCU_UID_LEN, MAX_RADIO_UID_LEN, RadioChipId,
    };

    fn info(tx_min: i8, tx_max: i8, freq_min: u32, freq_max: u32, sf_bm: u16, bw_bm: u16) -> Info {
        Info {
            proto_major: 1,
            proto_minor: 0,
            fw_major: 0,
            fw_minor: 0,
            fw_patch: 0,
            radio_chip_id: RadioChipId::Sx1262.as_u16(),
            capability_bitmap: donglora_protocol::cap::LORA,
            supported_sf_bitmap: sf_bm,
            supported_bw_bitmap: bw_bm,
            max_payload_bytes: 255,
            rx_queue_capacity: 32,
            tx_queue_capacity: 1,
            freq_min_hz: freq_min,
            freq_max_hz: freq_max,
            tx_power_min_dbm: tx_min,
            tx_power_max_dbm: tx_max,
            mcu_uid_len: 0,
            mcu_uid: [0u8; MAX_MCU_UID_LEN],
            radio_uid_len: 0,
            radio_uid: [0u8; MAX_RADIO_UID_LEN],
        }
    }

    fn lora(freq_hz: u32, sf: u8, bw: LoRaBandwidth, tx_power_dbm: i8) -> LoRaConfig {
        LoRaConfig {
            freq_hz,
            sf,
            bw,
            cr: LoRaCodingRate::Cr4_5,
            preamble_len: 8,
            sync_word: 0x3444,
            tx_power_dbm,
            header_mode: LoRaHeaderMode::Explicit,
            payload_crc: true,
            iq_invert: false,
        }
    }

    const SUB_GHZ_ALL_SF: u16 = 0x1FE0; // SF5..SF12
    const SX127X_SF: u16 = 0x1FC0; // SF6..SF12
    const SUB_GHZ_BW: u16 = 0x03FF; // BW 0..9

    #[test]
    fn tx_power_above_max_clamps_down() {
        let i = info(-9, 20, 863_000_000, 928_000_000, SUB_GHZ_ALL_SF, SUB_GHZ_BW);
        let cfg = lora(915_000_000, 7, LoRaBandwidth::Khz125, 30);
        let Modulation::LoRa(out) = prepare_config(&i, Modulation::LoRa(cfg)).unwrap() else {
            panic!("expected LoRa");
        };
        assert_eq!(out.tx_power_dbm, 20);
        assert_eq!(out.freq_hz, 915_000_000);
    }

    #[test]
    fn tx_power_below_min_clamps_up() {
        let i = info(2, 20, 863_000_000, 928_000_000, SUB_GHZ_ALL_SF, SUB_GHZ_BW);
        let cfg = lora(915_000_000, 7, LoRaBandwidth::Khz125, -30);
        let Modulation::LoRa(out) = prepare_config(&i, Modulation::LoRa(cfg)).unwrap() else {
            panic!("expected LoRa");
        };
        assert_eq!(out.tx_power_dbm, 2);
    }

    #[test]
    fn tx_power_in_range_unchanged() {
        let i = info(-9, 22, 863_000_000, 928_000_000, SUB_GHZ_ALL_SF, SUB_GHZ_BW);
        let cfg = lora(915_000_000, 7, LoRaBandwidth::Khz125, 17);
        let Modulation::LoRa(out) = prepare_config(&i, Modulation::LoRa(cfg)).unwrap() else {
            panic!("expected LoRa");
        };
        assert_eq!(out.tx_power_dbm, 17);
    }

    #[test]
    fn freq_out_of_range_rejected() {
        let i = info(-9, 22, 863_000_000, 928_000_000, SUB_GHZ_ALL_SF, SUB_GHZ_BW);
        let cfg = lora(300_000_000, 7, LoRaBandwidth::Khz125, 14);
        let err = prepare_config(&i, Modulation::LoRa(cfg)).unwrap_err();
        assert!(matches!(err, ClientError::ConfigNotSupported { ref reason } if reason.contains("frequency")));
    }

    #[test]
    fn sf5_rejected_on_sx127x_bitmap() {
        let i = info(2, 20, 863_000_000, 1_020_000_000, SX127X_SF, SUB_GHZ_BW);
        let cfg = lora(915_000_000, 5, LoRaBandwidth::Khz125, 14);
        let err = prepare_config(&i, Modulation::LoRa(cfg)).unwrap_err();
        assert!(matches!(err, ClientError::ConfigNotSupported { ref reason } if reason.contains("SF5")));
    }

    #[test]
    fn bw_not_in_bitmap_rejected() {
        let i = info(-9, 22, 863_000_000, 928_000_000, SUB_GHZ_ALL_SF, SUB_GHZ_BW);
        // Bw200 is bit 10 — SX128x only. Sub-GHz bitmap rejects it.
        let cfg = lora(915_000_000, 7, LoRaBandwidth::Khz200, 14);
        let err = prepare_config(&i, Modulation::LoRa(cfg)).unwrap_err();
        assert!(matches!(err, ClientError::ConfigNotSupported { ref reason } if reason.contains("bandwidth")));
    }

    #[test]
    fn non_lora_modulation_passes_through() {
        let i = info(-9, 22, 863_000_000, 928_000_000, SUB_GHZ_ALL_SF, SUB_GHZ_BW);
        let fsk = FskConfig {
            freq_hz: 50_000_000, // well outside info range — must not be inspected
            bitrate_bps: 50_000,
            freq_dev_hz: 25_000,
            rx_bw: 0,
            preamble_len: 16,
            sync_word_len: 0,
            sync_word: [0u8; 8],
        };
        let out = prepare_config(&i, Modulation::FskGfsk(fsk)).unwrap();
        assert!(matches!(out, Modulation::FskGfsk(_)));
    }
}
