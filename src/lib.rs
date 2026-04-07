//! DongLoRa host library — connect, configure, send/receive LoRa packets.
//!
//! Implements the DongLoRa USB protocol (COBS-framed fixed-size LE).
//! See the [`protocol`] module for wire types and constants.
//!
//! # Quick start
//!
//! ```no_run
//! use donglora_client::*;
//!
//! let mut client = connect_default()?;
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
pub use codec::{FrameReader, decode_frame, encode_frame, read_frame};
pub use connect::{connect, connect_default, connect_mux_auto, default_socket_path, try_connect};
pub use discovery::{USB_PID, USB_VID, find_port, wait_for_device};
pub use protocol::{
    Bandwidth, Command, ErrorCode, MAX_PAYLOAD, PREAMBLE_DEFAULT, RADIO_CONFIG_SIZE, RadioConfig, Response,
    TX_POWER_MAX,
};
pub use transport::{AnyTransport, MuxTransport, SerialTransport, Transport};

#[cfg(unix)]
pub use connect::mux_connect;
pub use connect::mux_tcp_connect;
