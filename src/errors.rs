//! Error taxonomy for `donglora-client`.
//!
//! Mirrors the Python client's `DongloraError` hierarchy: one base error
//! with specific variants for each spec-defined failure mode plus a
//! handful of transport-level conditions. Callers who just want to
//! surface failures match on the top-level [`ClientError`]; code that
//! needs to retry on a specific kind (e.g. `ChannelBusy`) pattern-matches
//! that variant.

use donglora_protocol::ErrorCode;
use thiserror::Error;

/// Every fallible operation in `donglora-client` returns this error
/// type. Variants mirror `PROTOCOL.md` §7 plus Rust-side conditions
/// (timeouts, closed transports, bad frames).
#[derive(Debug, Error)]
pub enum ClientError {
    /// Firmware returned `ERR(ENOTCONFIGURED)`. Callers usually want to
    /// re-apply their config and retry; [`crate::Dongle`] does this
    /// automatically via `_with_recovery`.
    #[error("device is not configured (ENOTCONFIGURED)")]
    NotConfigured,

    /// Firmware returned `ERR(EBUSY)` — TX queue full. Host should back
    /// off briefly and retry with a new tag.
    #[error("firmware TX queue is full (EBUSY)")]
    Busy,

    /// Firmware returned `ERR(EPARAM)` — a parameter value is out of
    /// range or invalid.
    #[error("parameter out of range (EPARAM)")]
    Param,

    /// Firmware returned `ERR(ELENGTH)` — payload length is wrong for
    /// the command or modulation.
    #[error("payload length wrong (ELENGTH)")]
    Length,

    /// Firmware returned `ERR(EMODULATION)` — requested modulation not
    /// supported on this chip.
    #[error("modulation not supported (EMODULATION)")]
    Modulation,

    /// Firmware returned `ERR(EUNKNOWN_CMD)` — unknown command type
    /// byte.
    #[error("unknown command (EUNKNOWN_CMD)")]
    UnknownCmd,

    /// Firmware returned `ERR(ERADIO)` — SPI error or unexpected radio
    /// hardware state.
    #[error("radio hardware error (ERADIO)")]
    Radio,

    /// Firmware reported a framing error (`ERR(EFRAME)`), usually
    /// because of CRC/COBS corruption on the H→D path. Rare on USB.
    #[error("framing error (EFRAME)")]
    Frame,

    /// Firmware returned `ERR(EINTERNAL)` — firmware bug / invariant
    /// violation.
    #[error("firmware internal error (EINTERNAL)")]
    Internal,

    /// Firmware returned an error code this client doesn't recognise.
    /// Preserves the raw u16 so forward-compat with minor-version
    /// extensions doesn't lose information.
    #[error("unknown error code 0x{0:04X}")]
    UnknownCode(u16),

    /// CAD detected activity and the TX was aborted before airtime.
    /// Per spec §6.10, host should randomized-backoff and retry with a
    /// **new tag**. Reported as a distinct variant from `Busy` because
    /// the retry policy differs.
    #[error("channel busy — CAD detected activity")]
    ChannelBusy,

    /// A queued TX was cancelled by a reconfigure or disconnect before
    /// it reached the air. Don't retry — the cancellation is terminal.
    #[error("TX cancelled before airtime")]
    Cancelled,

    /// Command did not complete before its deadline.
    #[error("timed out waiting for {what}")]
    Timeout { what: &'static str },

    /// The underlying transport (USB, socket) closed or errored.
    #[error("transport closed: {0}")]
    TransportClosed(String),

    /// Session reader thread died while waiting for a response.
    #[error("session reader exited")]
    ReaderExited,

    /// An inbound frame failed CRC or COBS decoding and was dropped.
    /// The session reader logs these as async events; callers can poll
    /// for them via [`crate::Dongle::drain_async_errors`].
    #[error("inbound frame corrupted: {0}")]
    BadFrame(String),

    /// Encoding an outbound frame failed (should not happen in normal
    /// use — the protocol crate's limits are enforced at the type level).
    #[error("frame encode failed: {0}")]
    EncodeFailed(String),

    /// Underlying I/O error from `tokio::io` or `tokio-serial`.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Catch-all for transport initialisation issues that don't have a
    /// dedicated variant.
    #[error("{0}")]
    Other(String),
}

impl ClientError {
    /// True if retrying the operation with a fresh tag is spec-sanctioned.
    /// Used by [`crate::RetryPolicy`] to decide whether to loop.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::ChannelBusy | Self::Busy)
    }

    /// Map an DongLoRa Protocol `ErrorCode` from the wire to the matching variant.
    #[must_use]
    pub fn from_wire(code: ErrorCode) -> Self {
        match code {
            ErrorCode::ENotConfigured => Self::NotConfigured,
            ErrorCode::EBusy => Self::Busy,
            ErrorCode::EParam => Self::Param,
            ErrorCode::ELength => Self::Length,
            ErrorCode::EModulation => Self::Modulation,
            ErrorCode::EUnknownCmd => Self::UnknownCmd,
            ErrorCode::ERadio => Self::Radio,
            ErrorCode::EFrame => Self::Frame,
            ErrorCode::EInternal => Self::Internal,
            ErrorCode::Unknown(raw) => Self::UnknownCode(raw),
        }
    }
}

/// Short alias for the crate's `Result` type.
pub type ClientResult<T> = Result<T, ClientError>;
