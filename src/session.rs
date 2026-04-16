//! Internal session plumbing: tag counter, outstanding-tag correlation,
//! background reader task, async event queue.
//!
//! Direct async port of `client-py/donglora/session.py`. Not part of the
//! public API — [`crate::Dongle`] wraps a [`Session`] and exposes the
//! user-facing surface on top of it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use donglora_protocol::{
    Command, FrameDecoder, FrameResult, Info, MAX_PAYLOAD_FIELD, MAX_WIRE_FRAME, Modulation, OkPayload, RxPayload,
    SetConfigResult, TxDonePayload, TxFlags, TxResult, commands, encode_frame, events,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::errors::{ClientError, ClientResult};
use crate::transport::Transport;

/// Result of a single tag-correlated command.
#[derive(Debug)]
pub(crate) enum SessionResponse {
    /// `OK` with no payload (PING / TX-enqueue-ack / RX_START / RX_STOP).
    Empty,
    /// `OK(Info)` from GET_INFO.
    Info(Info),
    /// `OK(SetConfigResult)` from SET_CONFIG.
    SetConfig(SetConfigResult),
    /// `TX_DONE` — the async TX completion (TRANSMITTED / CHANNEL_BUSY / CANCELLED).
    TxDone(TxDonePayload),
    /// `ERR(code)` — command failed.
    Err(ClientError),
}

/// State tracked for each outstanding command.
struct Pending {
    cmd_type: u8,
    /// Completion channel. For TX, we skip the intermediate `OK` and
    /// fire this only on `TX_DONE` (or early `ERR`). For every other
    /// command, fires on the first `OK`/`ERR`.
    waker: oneshot::Sender<SessionResponse>,
}

type PendingMap = Arc<Mutex<HashMap<u16, Pending>>>;

/// Session state.
pub(crate) struct Session {
    /// Monotonic tag counter; skips 0.
    next_tag: Arc<std::sync::Mutex<u16>>,
    pending: PendingMap,
    rx_rx: Arc<Mutex<mpsc::UnboundedReceiver<RxPayload>>>,
    async_err_rx: Arc<Mutex<mpsc::UnboundedReceiver<ClientError>>>,
    /// Half-duplex writer side. Owned exclusively by write calls via
    /// the mutex.
    writer: Arc<Mutex<Box<dyn AsyncWriteOnly>>>,
    reader_handle: Mutex<Option<JoinHandle<()>>>,
    closed: Arc<std::sync::atomic::AtomicBool>,
}

/// Object-safe helper — `dyn Transport` isn't object-safe because
/// `Transport` is a blanket impl, so we wrap the writer side in this
/// narrower trait.
pub(crate) trait AsyncWriteOnly: tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncWrite + Unpin + Send> AsyncWriteOnly for T {}

impl Session {
    /// Split *transport* into read/write halves and spawn the reader
    /// task. The reader dispatches inbound frames into either the
    /// per-tag `pending` map (command responses) or the `rx`/`async_err`
    /// queues (async events).
    pub(crate) fn spawn<T: Transport>(transport: T) -> Self {
        let (reader, writer) = tokio::io::split(transport);
        let (rx_tx, rx_rx) = mpsc::unbounded_channel();
        let (async_err_tx, async_err_rx) = mpsc::unbounded_channel();
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let pending_reader = pending.clone();
        let closed_reader = closed.clone();
        let handle = tokio::spawn(async move {
            reader_loop(reader, pending_reader, rx_tx, async_err_tx, closed_reader).await;
        });

        Self {
            next_tag: Arc::new(std::sync::Mutex::new(1)),
            pending,
            rx_rx: Arc::new(Mutex::new(rx_rx)),
            async_err_rx: Arc::new(Mutex::new(async_err_rx)),
            writer: Arc::new(Mutex::new(Box::new(writer))),
            reader_handle: Mutex::new(Some(handle)),
            closed,
        }
    }

    fn alloc_tag(&self) -> u16 {
        // `std::sync::Mutex` here: contended for microseconds; not worth
        // the await overhead. Tokio's mutex would demand async context.
        #[allow(clippy::unwrap_used)] // Mutex poisoning is not recoverable here
        let mut t = self.next_tag.lock().unwrap();
        let tag = *t;
        *t = t.wrapping_add(1);
        if *t == 0 {
            *t = 1;
        }
        tag
    }

    /// Send a command and await the (possibly-deferred) response.
    ///
    /// For most commands the waker fires on the first `OK`/`ERR`. For
    /// `TX`, the waker fires on `TX_DONE` (intermediate `OK` is absorbed).
    pub(crate) async fn send_command(
        &self,
        type_id: u8,
        payload: &[u8],
        timeout: Duration,
    ) -> ClientResult<SessionResponse> {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(ClientError::TransportClosed("session closed".into()));
        }
        let tag = self.alloc_tag();
        let (tx, rx) = oneshot::channel();
        {
            let mut map = self.pending.lock().await;
            map.insert(tag, Pending { cmd_type: type_id, waker: tx });
        }

        // Encode + write. If write fails, remove the pending slot so the
        // caller sees the transport error rather than a timeout.
        if let Err(e) = self.write_frame(type_id, tag, payload).await {
            self.pending.lock().await.remove(&tag);
            return Err(e);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(_)) => {
                self.pending.lock().await.remove(&tag);
                Err(ClientError::ReaderExited)
            }
            Err(_) => {
                self.pending.lock().await.remove(&tag);
                Err(ClientError::Timeout { what: describe_cmd(type_id) })
            }
        }
    }

    async fn write_frame(&self, type_id: u8, tag: u16, payload: &[u8]) -> ClientResult<()> {
        let mut wire = [0u8; MAX_WIRE_FRAME];
        let n =
            encode_frame(type_id, tag, payload, &mut wire).map_err(|e| ClientError::EncodeFailed(format!("{e:?}")))?;
        let mut writer = self.writer.lock().await;
        writer.write_all(&wire[..n]).await.map_err(ClientError::Io)?;
        writer.flush().await.map_err(ClientError::Io)?;
        Ok(())
    }

    pub(crate) async fn recv_rx(&self, timeout: Duration) -> Option<RxPayload> {
        let mut rx = self.rx_rx.lock().await;
        tokio::time::timeout(timeout, rx.recv()).await.ok().flatten()
    }

    /// Block forever for the next RX event (for long-running listeners).
    pub(crate) async fn next_rx(&self) -> Option<RxPayload> {
        let mut rx = self.rx_rx.lock().await;
        rx.recv().await
    }

    pub(crate) async fn drain_async_errors(&self) -> Vec<ClientError> {
        let mut out = Vec::new();
        let mut rx = self.async_err_rx.lock().await;
        while let Ok(e) = rx.try_recv() {
            out.push(e);
        }
        out
    }

    /// Close the session. Wakes any pending commands with
    /// `TransportClosed` and aborts the reader task.
    pub(crate) async fn close(&self) {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        // Wake every pending command so no caller hangs forever.
        let pending = {
            let mut map = self.pending.lock().await;
            std::mem::take(&mut *map)
        };
        for (_tag, p) in pending {
            let _ = p.waker.send(SessionResponse::Err(ClientError::TransportClosed("session closed".into())));
        }
        // Stop the reader. Abort is a no-op if it already exited.
        if let Some(h) = self.reader_handle.lock().await.take() {
            h.abort();
        }
    }

    // ── Typed command helpers ─────────────────────────────────────

    pub(crate) async fn ping(&self, timeout: Duration) -> ClientResult<()> {
        match self.send_command(commands::TYPE_PING, &[], timeout).await? {
            SessionResponse::Empty => Ok(()),
            SessionResponse::Err(e) => Err(e),
            other => Err(ClientError::Other(format!("unexpected PING response: {other:?}"))),
        }
    }

    pub(crate) async fn get_info(&self, timeout: Duration) -> ClientResult<Info> {
        match self.send_command(commands::TYPE_GET_INFO, &[], timeout).await? {
            SessionResponse::Info(i) => Ok(i),
            SessionResponse::Err(e) => Err(e),
            other => Err(ClientError::Other(format!("unexpected GET_INFO response: {other:?}"))),
        }
    }

    pub(crate) async fn set_config(&self, modulation: Modulation, timeout: Duration) -> ClientResult<SetConfigResult> {
        let cmd = Command::SetConfig(modulation);
        let mut payload = [0u8; MAX_PAYLOAD_FIELD];
        let n = cmd.encode_payload(&mut payload).map_err(|e| ClientError::EncodeFailed(format!("{e:?}")))?;
        match self.send_command(commands::TYPE_SET_CONFIG, &payload[..n], timeout).await? {
            SessionResponse::SetConfig(r) => Ok(r),
            SessionResponse::Err(e) => Err(e),
            other => Err(ClientError::Other(format!("unexpected SET_CONFIG response: {other:?}"))),
        }
    }

    pub(crate) async fn rx_start(&self, timeout: Duration) -> ClientResult<()> {
        match self.send_command(commands::TYPE_RX_START, &[], timeout).await? {
            SessionResponse::Empty => Ok(()),
            SessionResponse::Err(e) => Err(e),
            other => Err(ClientError::Other(format!("unexpected RX_START response: {other:?}"))),
        }
    }

    pub(crate) async fn rx_stop(&self, timeout: Duration) -> ClientResult<()> {
        match self.send_command(commands::TYPE_RX_STOP, &[], timeout).await? {
            SessionResponse::Empty => Ok(()),
            SessionResponse::Err(e) => Err(e),
            other => Err(ClientError::Other(format!("unexpected RX_STOP response: {other:?}"))),
        }
    }

    pub(crate) async fn transmit(&self, data: &[u8], skip_cad: bool, timeout: Duration) -> ClientResult<TxDonePayload> {
        if data.is_empty() {
            return Err(ClientError::Length);
        }
        let mut payload = [0u8; MAX_PAYLOAD_FIELD];
        payload[0] = TxFlags { skip_cad }.as_byte();
        payload[1..1 + data.len()].copy_from_slice(data);

        match self.send_command(commands::TYPE_TX, &payload[..1 + data.len()], timeout).await? {
            SessionResponse::TxDone(td) => match td.result {
                TxResult::Transmitted => Ok(td),
                TxResult::ChannelBusy => Err(ClientError::ChannelBusy),
                TxResult::Cancelled => Err(ClientError::Cancelled),
            },
            SessionResponse::Err(e) => Err(e),
            other => Err(ClientError::Other(format!("unexpected TX response: {other:?}"))),
        }
    }
}

fn describe_cmd(type_id: u8) -> &'static str {
    match type_id {
        commands::TYPE_PING => "PING",
        commands::TYPE_GET_INFO => "GET_INFO",
        commands::TYPE_SET_CONFIG => "SET_CONFIG",
        commands::TYPE_TX => "TX",
        commands::TYPE_RX_START => "RX_START",
        commands::TYPE_RX_STOP => "RX_STOP",
        _ => "command",
    }
}

async fn reader_loop<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    pending: PendingMap,
    rx_tx: mpsc::UnboundedSender<RxPayload>,
    async_err_tx: mpsc::UnboundedSender<ClientError>,
    closed: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];
    loop {
        if closed.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        let n = match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        // Collect frames into owned buffers (payload lifetime is tied to
        // the decoder's internal buffer, which gets reused on the next
        // feed call).
        let mut works: Vec<FrameWork> = Vec::new();
        decoder.feed(&buf[..n], |res| match res {
            FrameResult::Ok { type_id, tag, payload } => {
                works.push(FrameWork::Ok { type_id, tag, payload: payload.to_vec() });
            }
            FrameResult::Err(_) => works.push(FrameWork::Frame),
        });
        for w in works {
            match w {
                FrameWork::Ok { type_id, tag, payload } => {
                    dispatch(&pending, &rx_tx, &async_err_tx, type_id, tag, &payload).await;
                }
                FrameWork::Frame => {
                    let _ = async_err_tx.send(ClientError::BadFrame("inbound frame failed CRC or COBS".into()));
                }
            }
        }
    }
    // Reader is leaving — wake any pending commands so they fail fast.
    let pending = {
        let mut map = pending.lock().await;
        std::mem::take(&mut *map)
    };
    for (_tag, p) in pending {
        let _ = p.waker.send(SessionResponse::Err(ClientError::TransportClosed("reader exited".into())));
    }
}

enum FrameWork {
    Ok { type_id: u8, tag: u16, payload: Vec<u8> },
    Frame,
}

async fn dispatch(
    pending: &PendingMap,
    rx_tx: &mpsc::UnboundedSender<RxPayload>,
    async_err_tx: &mpsc::UnboundedSender<ClientError>,
    type_id: u8,
    tag: u16,
    payload: &[u8],
) {
    // Async events (tag == 0): RX and async ERR.
    if tag == 0 {
        match type_id {
            events::TYPE_RX => match RxPayload::decode(payload) {
                Ok(rx) => {
                    let _ = rx_tx.send(rx);
                }
                Err(_) => {
                    let _ = async_err_tx.send(ClientError::BadFrame("bad RX payload".into()));
                }
            },
            events::TYPE_ERR => match events::decode_err_payload(payload) {
                Ok(code) => {
                    let _ = async_err_tx.send(ClientError::from_wire(code));
                }
                Err(_) => {
                    let _ = async_err_tx.send(ClientError::BadFrame("bad async ERR payload".into()));
                }
            },
            _ => {
                // Unknown async frame type — log and drop.
                tracing::debug!(type_id, "unknown async frame type");
            }
        }
        return;
    }

    // Tag-correlated: OK / ERR / TX_DONE.
    let Some(Pending { cmd_type, waker }) = pending.lock().await.remove(&tag) else {
        tracing::debug!(tag, type_id, "no pending command for tag");
        return;
    };

    // TX has a two-phase completion (OK → TX_DONE). We want the waker
    // to fire on TX_DONE (or early ERR), so if we see the intermediate
    // OK for a TX, re-insert the pending slot instead of resolving it.
    if cmd_type == commands::TYPE_TX && type_id == events::TYPE_OK {
        pending.lock().await.insert(tag, Pending { cmd_type, waker });
        return;
    }

    match type_id {
        events::TYPE_OK => {
            let resp = match OkPayload::parse_for(cmd_type, payload) {
                Ok(OkPayload::Empty) => SessionResponse::Empty,
                Ok(OkPayload::Info(i)) => SessionResponse::Info(i),
                Ok(OkPayload::SetConfig(r)) => SessionResponse::SetConfig(r),
                Err(_) => {
                    SessionResponse::Err(ClientError::BadFrame("OK payload did not parse for command context".into()))
                }
            };
            let _ = waker.send(resp);
        }
        events::TYPE_ERR => {
            let resp = match events::decode_err_payload(payload) {
                Ok(code) => SessionResponse::Err(ClientError::from_wire(code)),
                Err(_) => SessionResponse::Err(ClientError::BadFrame("bad ERR payload".into())),
            };
            let _ = waker.send(resp);
        }
        events::TYPE_TX_DONE => {
            let resp = match TxDonePayload::decode(payload) {
                Ok(td) => SessionResponse::TxDone(td),
                Err(_) => SessionResponse::Err(ClientError::BadFrame("bad TX_DONE payload".into())),
            };
            let _ = waker.send(resp);
        }
        _ => {
            // Unexpected tagged type — surface as an error to the waiter.
            let _ = waker.send(SessionResponse::Err(ClientError::BadFrame(format!(
                "unexpected tagged frame type 0x{type_id:02X}"
            ))));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use donglora_protocol::{
        FrameDecoder, FrameResult, LoRaBandwidth, LoRaCodingRate, LoRaConfig, LoRaHeaderMode, Modulation, Owner,
        RxOrigin, SetConfigResultCode, TxResult,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn lora_cfg() -> Modulation {
        Modulation::LoRa(LoRaConfig {
            freq_hz: 910_525_000,
            sf: 7,
            bw: LoRaBandwidth::Khz62,
            cr: LoRaCodingRate::Cr4_5,
            preamble_len: 16,
            sync_word: 0x1424,
            tx_power_dbm: 20,
            header_mode: LoRaHeaderMode::Explicit,
            payload_crc: true,
            iq_invert: false,
        })
    }

    /// Read inbound bytes from `device` into a [`FrameDecoder`] until a
    /// single frame with `expected_type` (or any `Err` frame) arrives,
    /// returning `(tag, payload bytes)`.
    async fn read_one_frame(device: &mut tokio::io::DuplexStream) -> (u8, u16, Vec<u8>) {
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 256];
        loop {
            let n = device.read(&mut buf).await.unwrap();
            assert!(n > 0, "device stream closed");
            let mut out: Option<(u8, u16, Vec<u8>)> = None;
            decoder.feed(&buf[..n], |res| match res {
                FrameResult::Ok { type_id, tag, payload } => {
                    out = Some((type_id, tag, payload.to_vec()));
                }
                FrameResult::Err(_) => panic!("inbound frame decode failed"),
            });
            if let Some(tup) = out {
                return tup;
            }
        }
    }

    /// Encode a device-side frame and write it back to the session.
    async fn write_frame(device: &mut tokio::io::DuplexStream, type_id: u8, tag: u16, payload: &[u8]) {
        let mut wire = [0u8; donglora_protocol::MAX_WIRE_FRAME];
        let n = donglora_protocol::encode_frame(type_id, tag, payload, &mut wire).unwrap();
        device.write_all(&wire[..n]).await.unwrap();
        device.flush().await.unwrap();
    }

    #[tokio::test]
    async fn ping_round_trip() {
        let (session_side, mut device) = tokio::io::duplex(512);
        let session = Session::spawn(session_side);

        let ping_task = tokio::spawn(async move {
            session.ping(Duration::from_secs(1)).await.unwrap();
            session.close().await;
        });

        let (type_id, tag, _payload) = read_one_frame(&mut device).await;
        assert_eq!(type_id, commands::TYPE_PING);
        // PING has no payload, so just ack.
        write_frame(&mut device, events::TYPE_OK, tag, &[]).await;

        ping_task.await.unwrap();
    }

    #[tokio::test]
    async fn err_maps_to_client_error() {
        let (session_side, mut device) = tokio::io::duplex(512);
        let session = Session::spawn(session_side);

        let task = tokio::spawn(async move {
            let res = session.ping(Duration::from_secs(1)).await;
            session.close().await;
            res
        });

        let (_type_id, tag, _) = read_one_frame(&mut device).await;
        let mut payload = [0u8; 2];
        donglora_protocol::events::encode_err_payload(donglora_protocol::ErrorCode::ENotConfigured, &mut payload)
            .unwrap();
        write_frame(&mut device, events::TYPE_ERR, tag, &payload).await;

        let result = task.await.unwrap();
        assert!(matches!(result, Err(ClientError::NotConfigured)));
    }

    #[tokio::test]
    async fn tx_two_phase_completion() {
        let (session_side, mut device) = tokio::io::duplex(512);
        let session = Session::spawn(session_side);

        let task = tokio::spawn(async move {
            let res = session.transmit(b"hello", false, Duration::from_secs(1)).await;
            session.close().await;
            res
        });

        let (type_id, tag, payload) = read_one_frame(&mut device).await;
        assert_eq!(type_id, commands::TYPE_TX);
        // TX flags byte first, then data.
        assert_eq!(payload[0] & 1, 0, "skip_cad should be 0");
        assert_eq!(&payload[1..], b"hello");

        // Intermediate OK → host must NOT resolve yet.
        write_frame(&mut device, events::TYPE_OK, tag, &[]).await;
        // Give the session a tick to re-insert the pending slot.
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Now TX_DONE.
        let td = donglora_protocol::TxDonePayload { result: TxResult::Transmitted, airtime_us: 12_345 };
        let mut td_buf = [0u8; TxDonePayload::WIRE_SIZE];
        td.encode(&mut td_buf).unwrap();
        write_frame(&mut device, events::TYPE_TX_DONE, tag, &td_buf).await;

        let result = task.await.unwrap().unwrap();
        assert_eq!(result.airtime_us, 12_345);
        assert_eq!(result.result, TxResult::Transmitted);
    }

    #[tokio::test]
    async fn tx_channel_busy_maps_to_error() {
        let (session_side, mut device) = tokio::io::duplex(512);
        let session = Session::spawn(session_side);

        let task = tokio::spawn(async move {
            let res = session.transmit(b"x", false, Duration::from_secs(1)).await;
            session.close().await;
            res
        });

        let (_t, tag, _p) = read_one_frame(&mut device).await;
        write_frame(&mut device, events::TYPE_OK, tag, &[]).await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let td = donglora_protocol::TxDonePayload { result: TxResult::ChannelBusy, airtime_us: 0 };
        let mut td_buf = [0u8; TxDonePayload::WIRE_SIZE];
        td.encode(&mut td_buf).unwrap();
        write_frame(&mut device, events::TYPE_TX_DONE, tag, &td_buf).await;

        assert!(matches!(task.await.unwrap(), Err(ClientError::ChannelBusy)));
    }

    #[tokio::test]
    async fn async_rx_delivered_to_queue() {
        let (session_side, mut device) = tokio::io::duplex(1024);
        let session = Session::spawn(session_side);

        // Push an RX event (tag=0).
        let mut data = heapless::Vec::<u8, { donglora_protocol::MAX_OTA_PAYLOAD }>::new();
        data.extend_from_slice(b"rx-payload").unwrap();
        let rx = RxPayload {
            rssi_tenths_dbm: -720,
            snr_tenths_db: 95,
            freq_err_hz: -100,
            timestamp_us: 1_111_111,
            crc_valid: true,
            packets_dropped: 0,
            origin: RxOrigin::Ota,
            data,
        };
        let mut rx_buf = [0u8; RxPayload::METADATA_SIZE + donglora_protocol::MAX_OTA_PAYLOAD];
        let n = rx.encode(&mut rx_buf).unwrap();
        write_frame(&mut device, events::TYPE_RX, 0, &rx_buf[..n]).await;

        let got = session.recv_rx(Duration::from_secs(1)).await.unwrap();
        assert_eq!(&got.data[..], b"rx-payload");
        assert_eq!(got.rssi_tenths_dbm, -720);
        session.close().await;
    }

    /// Two out-of-order concurrent sends must correlate correctly by tag.
    ///
    /// Runs three futures via `tokio::join!` on the same task: two pings
    /// and a "device" task that drains both command frames and responds
    /// in reverse order. If tag correlation is broken, one or both pings
    /// resolve with the wrong response (or time out).
    #[tokio::test]
    async fn concurrent_sends_correlate_by_tag() {
        let (session_side, mut device) = tokio::io::duplex(1024);
        let session = Session::spawn(session_side);

        let ping_a = session.ping(Duration::from_secs(2));
        let ping_b = session.ping(Duration::from_secs(2));
        let device_drive = async {
            // Two PINGs may arrive in the same `device.read` or split
            // across reads — drain until we've seen both, regardless.
            let mut decoder = FrameDecoder::new();
            let mut buf = [0u8; 256];
            let mut frames: Vec<(u8, u16, Vec<u8>)> = Vec::new();
            while frames.len() < 2 {
                let n = device.read(&mut buf).await.unwrap();
                decoder.feed(&buf[..n], |res| {
                    if let FrameResult::Ok { type_id, tag, payload } = res {
                        frames.push((type_id, tag, payload.to_vec()));
                    }
                });
            }
            let (t1, tag1, _) = frames[0].clone();
            let (t2, tag2, _) = frames[1].clone();
            assert_eq!(t1, commands::TYPE_PING);
            assert_eq!(t2, commands::TYPE_PING);
            assert_ne!(tag1, tag2, "each command gets a fresh tag");
            // Ack in reverse order to prove correlation doesn't depend
            // on FIFO.
            write_frame(&mut device, events::TYPE_OK, tag2, &[]).await;
            write_frame(&mut device, events::TYPE_OK, tag1, &[]).await;
            (tag1, tag2)
        };

        let (a, b, (_tag1, _tag2)) = tokio::join!(ping_a, ping_b, device_drive);
        a.unwrap();
        b.unwrap();
        session.close().await;
    }

    #[tokio::test]
    async fn close_wakes_pending_commands() {
        let (session_side, _device) = tokio::io::duplex(512);
        let session = Arc::new(Session::spawn(session_side));

        let s = session.clone();
        let task = tokio::spawn(async move { s.ping(Duration::from_secs(5)).await });

        // Give the request a moment to hit the writer.
        tokio::time::sleep(Duration::from_millis(20)).await;

        session.close().await;

        let err = task.await.unwrap().unwrap_err();
        assert!(matches!(err, ClientError::TransportClosed(_)));
    }

    #[tokio::test]
    async fn timeout_removes_pending_slot() {
        let (session_side, _device) = tokio::io::duplex(512);
        let session = Session::spawn(session_side);
        let err = session.ping(Duration::from_millis(50)).await.unwrap_err();
        assert!(matches!(err, ClientError::Timeout { .. }));
        session.close().await;
    }

    #[tokio::test]
    async fn set_config_round_trip() {
        let (session_side, mut device) = tokio::io::duplex(1024);
        let session = Session::spawn(session_side);

        let cfg = lora_cfg();
        let task = tokio::spawn(async move {
            let r = session.set_config(cfg, Duration::from_secs(1)).await;
            session.close().await;
            r
        });

        let (type_id, tag, _p) = read_one_frame(&mut device).await;
        assert_eq!(type_id, commands::TYPE_SET_CONFIG);

        // Build the OK(SetConfigResult) response with APPLIED + MINE + echoed cfg.
        let result = SetConfigResult { result: SetConfigResultCode::Applied, owner: Owner::Mine, current: cfg };
        let mut rbuf = [0u8; donglora_protocol::MAX_SETCONFIG_OK_PAYLOAD];
        let n = result.encode(&mut rbuf).unwrap();
        write_frame(&mut device, events::TYPE_OK, tag, &rbuf[..n]).await;

        let got = task.await.unwrap().unwrap();
        assert_eq!(got.result, SetConfigResultCode::Applied);
        assert_eq!(got.owner, Owner::Mine);
    }

    #[tokio::test]
    async fn bad_frame_lands_in_async_err_queue() {
        let (session_side, mut device) = tokio::io::duplex(512);
        let session = Session::spawn(session_side);

        // Write a garbage byte then a zero sentinel — decoder emits Err.
        device.write_all(&[0x01, 0x01, 0x00]).await.unwrap();
        device.flush().await.unwrap();
        // Give the reader time to dispatch.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let errors = session.drain_async_errors().await;
        assert!(!errors.is_empty(), "expected at least one bad-frame error");
        session.close().await;
    }
}
