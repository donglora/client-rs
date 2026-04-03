//! High-level DongLoRa client.
//!
//! The [`Client`] wraps a transport and provides ergonomic send/recv methods
//! that handle the command/response discipline (buffering unsolicited RxPackets
//! while waiting for solicited responses).

use std::collections::VecDeque;

use crate::codec::{encode_frame, read_frame};
use crate::protocol::{Command, ErrorCode, RadioConfig, Response};
use crate::transport::Transport;

/// Maximum number of buffered RxPackets before oldest are dropped.
const RX_BUFFER_CAP: usize = 256;

/// Maximum frames to read while waiting for a solicited response before giving up.
const MAX_UNSOLICITED_BEFORE_TIMEOUT: usize = 50;

/// High-level DongLoRa client, generic over transport.
///
/// Works with any [`Transport`]: direct USB serial, Unix socket mux, or TCP mux.
pub struct Client<T: Transport> {
    transport: T,
    rx_buffer: VecDeque<Response>,
}

impl<T: Transport> Client<T> {
    /// Create a new client wrapping the given transport.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            rx_buffer: VecDeque::with_capacity(64),
        }
    }

    /// Send a command and wait for the solicited response.
    ///
    /// Any unsolicited `RxPacket` frames encountered while waiting are buffered
    /// and retrievable via [`recv`](Self::recv) or [`drain_rx`](Self::drain_rx).
    pub fn send(&mut self, cmd: Command) -> anyhow::Result<Response> {
        let frame = encode_frame(&cmd.to_bytes());
        std::io::Write::write_all(&mut self.transport, &frame)?;
        std::io::Write::flush(&mut self.transport)?;

        for _ in 0..MAX_UNSOLICITED_BEFORE_TIMEOUT {
            let data = read_frame(&mut self.transport)?
                .ok_or_else(|| anyhow::anyhow!("timeout waiting for response"))?;
            let resp = Response::from_bytes(&data)
                .ok_or_else(|| anyhow::anyhow!("malformed response ({} bytes)", data.len()))?;
            if resp.is_rx_packet() {
                self.buffer_rx(resp);
                continue;
            }
            return Ok(resp);
        }
        anyhow::bail!("no solicited response after {MAX_UNSOLICITED_BEFORE_TIMEOUT} frames")
    }

    /// Return the next RxPacket from the buffer or the wire.
    ///
    /// Returns `Ok(None)` on timeout (no packet available).
    pub fn recv(&mut self) -> anyhow::Result<Option<Response>> {
        if let Some(pkt) = self.rx_buffer.pop_front() {
            return Ok(Some(pkt));
        }
        let Some(data) = read_frame(&mut self.transport)? else {
            return Ok(None);
        };
        let resp = Response::from_bytes(&data)
            .ok_or_else(|| anyhow::anyhow!("malformed response ({} bytes)", data.len()))?;
        if resp.is_rx_packet() {
            Ok(Some(resp))
        } else {
            // Non-RxPacket unsolicited response — shouldn't happen, discard
            Ok(None)
        }
    }

    /// Drain all buffered and pending RxPacket frames.
    ///
    /// Temporarily reduces the read timeout to quickly drain any frames still
    /// in flight, then restores the original timeout.
    pub fn drain_rx(&mut self) -> anyhow::Result<Vec<Response>> {
        let mut packets: Vec<Response> = self.rx_buffer.drain(..).collect();

        let old_timeout = self.transport.timeout();
        self.transport
            .set_timeout(std::time::Duration::from_millis(10))?;

        loop {
            let Some(data) = read_frame(&mut self.transport)? else {
                break;
            };
            if let Some(resp) = Response::from_bytes(&data)
                && resp.is_rx_packet()
            {
                packets.push(resp);
            }
        }

        self.transport.set_timeout(old_timeout)?;
        Ok(packets)
    }

    /// Send a Ping and verify the Pong response.
    pub fn ping(&mut self) -> anyhow::Result<()> {
        match self.send(Command::Ping)? {
            Response::Pong => Ok(()),
            other => anyhow::bail!("unexpected response to Ping: {other:?}"),
        }
    }

    /// Set the radio configuration.
    pub fn set_config(&mut self, config: RadioConfig) -> anyhow::Result<()> {
        match self.send(Command::SetConfig(config))? {
            Response::Ok => Ok(()),
            Response::Error(code) => anyhow::bail!("SetConfig failed: {code}"),
            other => anyhow::bail!("unexpected response to SetConfig: {other:?}"),
        }
    }

    /// Start receiving LoRa packets.
    pub fn start_rx(&mut self) -> anyhow::Result<()> {
        match self.send(Command::StartRx)? {
            Response::Ok => Ok(()),
            Response::Error(code) => anyhow::bail!("StartRx failed: {code}"),
            other => anyhow::bail!("unexpected response to StartRx: {other:?}"),
        }
    }

    /// Stop receiving LoRa packets.
    pub fn stop_rx(&mut self) -> anyhow::Result<()> {
        match self.send(Command::StopRx)? {
            Response::Ok => Ok(()),
            Response::Error(code) => anyhow::bail!("StopRx failed: {code}"),
            other => anyhow::bail!("unexpected response to StopRx: {other:?}"),
        }
    }

    /// Transmit a LoRa packet.
    pub fn transmit(
        &mut self,
        payload: &[u8],
        config: Option<RadioConfig>,
    ) -> anyhow::Result<()> {
        let cmd = Command::Transmit {
            config,
            payload: payload.to_vec(),
        };
        match self.send(cmd)? {
            Response::TxDone => Ok(()),
            Response::Error(ErrorCode::TxTimeout) => anyhow::bail!("transmit timed out"),
            Response::Error(code) => anyhow::bail!("Transmit failed: {code}"),
            other => anyhow::bail!("unexpected response to Transmit: {other:?}"),
        }
    }

    /// Get the board's MAC address.
    pub fn get_mac(&mut self) -> anyhow::Result<[u8; 6]> {
        match self.send(Command::GetMac)? {
            Response::MacAddress(mac) => Ok(mac),
            Response::Error(code) => anyhow::bail!("GetMac failed: {code}"),
            other => anyhow::bail!("unexpected response to GetMac: {other:?}"),
        }
    }

    /// Get the current radio configuration from the device.
    pub fn get_config(&mut self) -> anyhow::Result<RadioConfig> {
        match self.send(Command::GetConfig)? {
            Response::Config(cfg) => Ok(cfg),
            Response::Error(code) => anyhow::bail!("GetConfig failed: {code}"),
            other => anyhow::bail!("unexpected response to GetConfig: {other:?}"),
        }
    }

    /// Turn on the display (if present).
    pub fn display_on(&mut self) -> anyhow::Result<()> {
        match self.send(Command::DisplayOn)? {
            Response::Ok => Ok(()),
            Response::Error(code) => anyhow::bail!("DisplayOn failed: {code}"),
            other => anyhow::bail!("unexpected response to DisplayOn: {other:?}"),
        }
    }

    /// Turn off the display (if present).
    pub fn display_off(&mut self) -> anyhow::Result<()> {
        match self.send(Command::DisplayOff)? {
            Response::Ok => Ok(()),
            Response::Error(code) => anyhow::bail!("DisplayOff failed: {code}"),
            other => anyhow::bail!("unexpected response to DisplayOff: {other:?}"),
        }
    }

    /// Consume the client and return the inner transport.
    pub fn into_inner(self) -> T {
        self.transport
    }

    /// Get a reference to the inner transport.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Get a mutable reference to the inner transport.
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    fn buffer_rx(&mut self, resp: Response) {
        if self.rx_buffer.len() >= RX_BUFFER_CAP {
            self.rx_buffer.pop_front(); // drop oldest
        }
        self.rx_buffer.push_back(resp);
    }
}
