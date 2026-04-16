//! DongLoRa host library — connect, configure, send/receive LoRa packets.
//!
//! Implements the DongLoRa Protocol v2 on top of the
//! [`donglora-protocol`](donglora_protocol) wire crate. All I/O is
//! async via tokio; the public surface mirrors the Python client in
//! `client-py/`.
//!
//! # Quick start
//!
//! ```no_run
//! # async fn demo() -> Result<(), donglora_client::ClientError> {
//! use donglora_client::{
//!     connect, LoRaBandwidth, LoRaCodingRate, LoRaConfig, LoRaHeaderMode, Modulation,
//! };
//!
//! let dongle = connect().await?;
//! dongle.set_config(Modulation::LoRa(LoRaConfig {
//!     freq_hz: 910_525_000,
//!     sf: 7,
//!     bw: LoRaBandwidth::Khz62,
//!     cr: LoRaCodingRate::Cr4_5,
//!     preamble_len: 16,
//!     sync_word: 0x1424,
//!     tx_power_dbm: 20,
//!     header_mode: LoRaHeaderMode::Explicit,
//!     payload_crc: true,
//!     iq_invert: false,
//! })).await?;
//! dongle.tx(b"hello world").await?;
//! # Ok(()) }
//! ```
//!
//! # Module layout
//!
//! - [`connect`] — auto-discovery + [`ConnectOptions`] builder.
//! - [`dongle`] — public [`Dongle`] radio session type.
//! - [`session`] — internal async plumbing (not public).
//! - [`transport`] — tokio-based byte-stream transports.
//! - [`errors`] — [`ClientError`] taxonomy.
//! - [`retry`] — [`RetryPolicy`] + [`TxOutcome`] for `tx_with_retry`.
//! - [`discovery`] — USB VID/PID scan + async `wait_for_device`.

#![forbid(unsafe_code)]

pub mod connect;
pub mod discovery;
pub mod dongle;
pub mod errors;
pub mod retry;
pub mod session;
pub mod transport;

// ── Flat re-exports for convenience ─────────────────────────────────

#[cfg(unix)]
pub use connect::mux_unix_connect;
pub use connect::{
    ConnectOptions, connect, connect_mux_auto, connect_mux_auto_with, connect_with, default_socket_path,
    find_mux_socket, mux_tcp_connect, try_connect, try_connect_with,
};
pub use discovery::{USB_PID, USB_VID, find_port, wait_for_device};
pub use dongle::{Dongle, KEEPALIVE_INTERVAL, TransportKind};
pub use errors::{ClientError, ClientResult};
pub use retry::{RetryPolicy, TxAttempt, TxOutcome};
#[cfg(unix)]
pub use transport::UnixSocketTransport;
pub use transport::{AnyTransport, SerialTransport, TcpTransport, Transport};

// Re-export the protocol crate's user-facing types so callers don't need
// to add `donglora-protocol` as a direct dep.
pub use donglora_protocol::{
    Command, DeviceMessage, ErrorCode, FlrcBitrate, FlrcBt, FlrcCodingRate, FlrcConfig, FlrcPreambleLen, FskConfig,
    Info, LoRaBandwidth, LoRaCodingRate, LoRaConfig, LoRaHeaderMode, LrFhssBandwidth, LrFhssCodingRate, LrFhssConfig,
    LrFhssGrid, Modulation, ModulationId, OkPayload, Owner, RadioChipId, RxOrigin, RxPayload, SetConfigResult,
    SetConfigResultCode, TxDonePayload, TxFlags, TxResult, cap,
};
