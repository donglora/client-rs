//! DongLoRa host library — connect, configure, send/receive LoRa packets.
//!
//! Implements the DongLoRa USB protocol (COBS-framed fixed-size LE).
//! See `firmware/PROTOCOL.md` for the full specification.
//!
//! # Quick start
//!
//! ```no_run
//! use donglora_client::*;
//!
//! let mut client = connect_default()?;
//! client.ping()?;
//! client.set_config(RadioConfig::default())?;
//! client.start_rx()?;
//!
//! loop {
//!     if let Some(Response::RxPacket { rssi, snr, payload }) = client.recv()? {
//!         println!("RX rssi={rssi} snr={snr} len={}", payload.len());
//!     }
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```

pub mod client;
pub mod codec;
pub mod connect;
pub mod discovery;
pub mod protocol;
pub mod transport;

// Flat re-exports for convenience
pub use client::Client;
pub use codec::{decode_frame, encode_frame, read_frame, FrameReader};
pub use connect::{connect, connect_default, default_socket_path};
pub use discovery::{find_port, wait_for_device, USB_PID, USB_VID};
pub use protocol::{
    Bandwidth, Command, ErrorCode, RadioConfig, Response, CMD_TAG_SET_CONFIG, CMD_TAG_START_RX,
    CMD_TAG_STOP_RX, ERROR_INVALID_CONFIG, MAX_PAYLOAD, PREAMBLE_DEFAULT, RADIO_CONFIG_SIZE,
    RESP_TAG_ERROR, RESP_TAG_OK, RESP_TAG_RX_PACKET, TX_POWER_MAX,
};
pub use transport::{AnyTransport, MuxTransport, SerialTransport, Transport};

#[cfg(unix)]
pub use connect::mux_connect;
pub use connect::mux_tcp_connect;
