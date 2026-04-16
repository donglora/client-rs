//! The user-facing [`Dongle`] — a connected, configured, ready-to-TX radio.
//!
//! Built on top of the internal [`crate::session::Session`]. Hides the
//! frame codec, tag plumbing, CRC, and keepalive details behind an async
//! method surface. Mirrors `client-py/donglora/dongle.py`.

use std::sync::Arc;
use std::time::Duration;

use donglora_protocol::{Info, Modulation, RxPayload, TxDonePayload};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::errors::{ClientError, ClientResult};
use crate::retry::{RetryPolicy, TxAttempt, TxOutcome};
use crate::session::Session;

/// Default keepalive cadence. Spec §3.4 sets the inactivity timer to
/// 1000 ms — we ping at 500 ms for 2x margin. Host commands reset the
/// firmware's timer naturally, so we only emit a PING when the host
/// has been quiet for a full keepalive interval.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(500);

/// Live DongLoRa radio session.
///
/// Normally constructed via [`crate::connect`]. The happy path:
///
/// ```no_run
/// # async fn demo() -> Result<(), donglora_client::ClientError> {
/// use donglora_client as dl;
///
/// let dongle = dl::connect().await?;
/// dongle.tx(b"Hello").await?;
/// while let Some(pkt) = dongle.recv(std::time::Duration::from_secs(5)).await {
///     println!("{:.1} dBm {:?}", pkt.rssi_tenths_dbm as f32 / 10.0, pkt.data);
/// }
/// # Ok(()) }
/// ```
///
/// Thread-safe: multiple tasks can call `tx` / `recv` / `ping`
/// concurrently. Drop is best-effort — explicit `close().await` is
/// recommended when the caller can afford to await it.
/// Which transport `connect()` ended up using. Useful for logs / UIs
/// that want to distinguish "talking to the dongle directly" from
/// "talking to a mux that talks to the dongle".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportKind {
    /// Direct USB serial. String is the device path (e.g. `"/dev/ttyACM0"`).
    Serial(String),
    /// `donglora-mux` daemon over a Unix domain socket. String is the
    /// socket path.
    MuxUnix(String),
    /// `donglora-mux` daemon over TCP. String is `"host:port"`.
    MuxTcp(String),
}

impl TransportKind {
    /// Short human-readable label for this transport, suitable for a
    /// status line (`"ttyACM0"`, `"mux:unix"`, `"mux:tcp"`).
    #[must_use]
    pub fn short_label(&self) -> String {
        match self {
            Self::Serial(p) => {
                std::path::Path::new(p).file_name().map_or_else(|| p.clone(), |f| f.to_string_lossy().into_owned())
            }
            Self::MuxUnix(_) => "mux:unix".to_string(),
            Self::MuxTcp(_) => "mux:tcp".to_string(),
        }
    }

    /// True for any mux-backed transport.
    #[must_use]
    pub const fn is_mux(&self) -> bool {
        matches!(self, Self::MuxUnix(_) | Self::MuxTcp(_))
    }
}

pub struct Dongle {
    inner: Arc<Inner>,
    /// Keepalive task handle. `std::sync::Mutex` so `Drop` can abort
    /// without an async context.
    keepalive_handle: std::sync::Mutex<Option<JoinHandle<()>>>,
}

pub(crate) struct Inner {
    pub(crate) session: Session,
    info: Info,
    transport_kind: TransportKind,
    applied_config: Mutex<Option<Modulation>>,
    rx_started: Mutex<bool>,
    last_write_at: Mutex<tokio::time::Instant>,
    closed: Mutex<bool>,
}

impl Dongle {
    /// Construct a Dongle on an already-connected session. The caller is
    /// responsible for running GET_INFO and (optionally) SET_CONFIG first;
    /// [`crate::connect`] does both.
    pub(crate) fn new(
        session: Session,
        info: Info,
        transport_kind: TransportKind,
        applied_config: Option<Modulation>,
        keepalive: bool,
    ) -> Self {
        let inner = Arc::new(Inner {
            session,
            info,
            transport_kind,
            applied_config: Mutex::new(applied_config),
            rx_started: Mutex::new(false),
            last_write_at: Mutex::new(tokio::time::Instant::now()),
            closed: Mutex::new(false),
        });

        let keepalive_handle = if keepalive {
            let weak = Arc::downgrade(&inner);
            let handle = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(KEEPALIVE_INTERVAL).await;
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    if *inner.closed.lock().await {
                        return;
                    }
                    let last = *inner.last_write_at.lock().await;
                    if last.elapsed() < KEEPALIVE_INTERVAL {
                        continue;
                    }
                    match inner.session.ping(Duration::from_secs(2)).await {
                        Ok(()) => {
                            *inner.last_write_at.lock().await = tokio::time::Instant::now();
                        }
                        Err(ClientError::NotConfigured) => {
                            let cfg = *inner.applied_config.lock().await;
                            if let Some(m) = cfg {
                                match inner.session.set_config(m, Duration::from_secs(2)).await {
                                    Ok(_) => {
                                        *inner.last_write_at.lock().await = tokio::time::Instant::now();
                                    }
                                    Err(e) => {
                                        tracing::debug!(?e, "keepalive recovery set_config failed");
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::debug!(?e, "keepalive ping failed");
                        }
                    }
                }
            });
            std::sync::Mutex::new(Some(handle))
        } else {
            std::sync::Mutex::new(None)
        };

        Self { inner, keepalive_handle }
    }

    /// Cached `GET_INFO` snapshot from connect-time.
    pub fn info(&self) -> &Info {
        &self.inner.info
    }

    /// Which transport this Dongle is backed by. Lets UIs distinguish
    /// a direct USB connection from a mux-backed one.
    pub fn transport_kind(&self) -> &TransportKind {
        &self.inner.transport_kind
    }

    /// The most recently applied config (if any).
    pub async fn config(&self) -> Option<Modulation> {
        *self.inner.applied_config.lock().await
    }

    /// Transmit *data*. Blocks until `TX_DONE` arrives or an error
    /// surfaces. Performs one auto-recovery retry on `NotConfigured`.
    ///
    /// Use [`Dongle::tx_with_retry`] if you want CAD-busy backoff.
    pub async fn tx(&self, data: &[u8]) -> ClientResult<TxDonePayload> {
        self.tx_with_timeout(data, false, Duration::from_secs(10)).await
    }

    /// Transmit with a configurable timeout and the `skip_cad` flag.
    pub async fn tx_with_timeout(&self, data: &[u8], skip_cad: bool, timeout: Duration) -> ClientResult<TxDonePayload> {
        self.check_open().await?;
        self.mark_write().await;
        match self.inner.session.transmit(data, skip_cad, timeout).await {
            Err(ClientError::NotConfigured) => self.recover_and_retry_tx(data, skip_cad, timeout).await,
            other => other,
        }
    }

    /// Transmit with automatic retry on `CHANNEL_BUSY` / `EBUSY` per
    /// spec §6.10. Returns a [`TxOutcome`] capturing the full per-attempt
    /// history (so callers — bridge's TUI especially — can surface retry
    /// counts).
    pub async fn tx_with_retry(&self, data: &[u8], policy: &RetryPolicy) -> ClientResult<TxOutcome> {
        self.check_open().await?;
        let mut attempts: Vec<TxAttempt> = Vec::with_capacity(policy.max_attempts as usize);
        let mut delay_ms = policy.backoff_ms_min;
        for attempt_num in 1..=policy.max_attempts {
            self.mark_write().await;
            let started = tokio::time::Instant::now();
            let result = self.inner.session.transmit(data, policy.skip_cad, policy.per_attempt_timeout).await;
            let elapsed = started.elapsed();
            match result {
                Ok(td) => {
                    attempts.push(TxAttempt { attempt: attempt_num, result: Ok(td), elapsed });
                    return Ok(TxOutcome { final_airtime_us: td.airtime_us, attempts });
                }
                Err(e) => {
                    let retryable = e.is_retryable();
                    let attempt_log = TxAttempt { attempt: attempt_num, result: Err(e.clone_kind()), elapsed };
                    attempts.push(attempt_log);
                    if !retryable || attempt_num == policy.max_attempts {
                        return Err(e);
                    }
                    let jitter_ms = policy.jitter_ms();
                    let wait = Duration::from_millis(u64::from(delay_ms) + u64::from(jitter_ms));
                    tokio::time::sleep(wait).await;
                    delay_ms = (delay_ms as f32 * policy.backoff_multiplier) as u32;
                    delay_ms = delay_ms.min(policy.backoff_cap_ms);
                }
            }
        }
        // Unreachable — loop either returns on success or errors on the
        // final attempt. Surface a clean error just in case.
        Err(ClientError::Other("tx_with_retry exited without result".into()))
    }

    /// Enter continuous receive mode if not already active.
    pub async fn rx_start(&self) -> ClientResult<()> {
        self.check_open().await?;
        self.mark_write().await;
        let mut started = self.inner.rx_started.lock().await;
        if *started {
            return Ok(());
        }
        match self.inner.session.rx_start(Duration::from_secs(2)).await {
            Err(ClientError::NotConfigured) => {
                drop(started);
                self.reapply_config().await?;
                self.inner.session.rx_start(Duration::from_secs(2)).await?;
                *self.inner.rx_started.lock().await = true;
            }
            Err(e) => return Err(e),
            Ok(()) => {
                *started = true;
            }
        }
        Ok(())
    }

    /// Exit continuous receive mode.
    pub async fn rx_stop(&self) -> ClientResult<()> {
        self.check_open().await?;
        self.mark_write().await;
        self.inner.session.rx_stop(Duration::from_secs(2)).await?;
        *self.inner.rx_started.lock().await = false;
        Ok(())
    }

    /// Wait up to *timeout* for the next RX event. Lazily starts continuous
    /// RX on the first call. Returns `None` on timeout.
    pub async fn recv(&self, timeout: Duration) -> Option<RxPayload> {
        if self.check_open().await.is_err() {
            return None;
        }
        if !*self.inner.rx_started.lock().await && self.rx_start().await.is_err() {
            return None;
        }
        self.inner.session.recv_rx(timeout).await
    }

    /// Long-running listener: yields every RX event until the session
    /// closes. Lazily starts continuous RX on first call.
    pub async fn next_rx(&self) -> Option<RxPayload> {
        if self.check_open().await.is_err() {
            return None;
        }
        if !*self.inner.rx_started.lock().await && self.rx_start().await.is_err() {
            return None;
        }
        self.inner.session.next_rx().await
    }

    /// Apply a new radio configuration (possibly a different modulation).
    pub async fn set_config(&self, modulation: Modulation) -> ClientResult<()> {
        self.check_open().await?;
        self.mark_write().await;
        let result = self.inner.session.set_config(modulation, Duration::from_secs(2)).await?;
        // Cache the config for auto-recovery. We store what the firmware
        // echoed back as `current` (authoritative), not what we sent.
        *self.inner.applied_config.lock().await = Some(result.current);
        // SET_CONFIG aborts any continuous RX on the device side.
        *self.inner.rx_started.lock().await = false;
        Ok(())
    }

    /// Send a `PING`. Rarely needed directly — the keepalive task covers
    /// session liveness unless you constructed the Dongle with `keepalive = false`.
    pub async fn ping(&self) -> ClientResult<()> {
        self.check_open().await?;
        self.mark_write().await;
        self.inner.session.ping(Duration::from_secs(2)).await
    }

    /// Drain any async error frames (`ERR(EFRAME)`, `ERR(ERADIO)` with
    /// tag 0, etc.) the reader observed since the last drain.
    pub async fn drain_async_errors(&self) -> Vec<ClientError> {
        self.inner.session.drain_async_errors().await
    }

    /// Close the session gracefully. Aborts the keepalive task and the
    /// reader task. Idempotent.
    pub async fn close(&self) {
        let mut closed = self.inner.closed.lock().await;
        if *closed {
            return;
        }
        *closed = true;
        drop(closed);
        self.abort_keepalive();
        self.inner.session.close().await;
    }

    fn abort_keepalive(&self) {
        #[allow(clippy::unwrap_used)] // std::sync::Mutex poisoning is unrecoverable
        let mut guard = self.keepalive_handle.lock().unwrap();
        if let Some(h) = guard.take() {
            h.abort();
        }
    }

    // ── internals ─────────────────────────────────────────────────

    async fn check_open(&self) -> ClientResult<()> {
        if *self.inner.closed.lock().await {
            return Err(ClientError::TransportClosed("dongle closed".into()));
        }
        Ok(())
    }

    async fn mark_write(&self) {
        *self.inner.last_write_at.lock().await = tokio::time::Instant::now();
    }

    async fn reapply_config(&self) -> ClientResult<()> {
        let cfg = (*self.inner.applied_config.lock().await).ok_or(ClientError::NotConfigured)?;
        let result = self.inner.session.set_config(cfg, Duration::from_secs(2)).await?;
        *self.inner.applied_config.lock().await = Some(result.current);
        Ok(())
    }

    async fn recover_and_retry_tx(
        &self,
        data: &[u8],
        skip_cad: bool,
        timeout: Duration,
    ) -> ClientResult<TxDonePayload> {
        tracing::info!("dongle: auto-recovering after inactivity timeout");
        self.reapply_config().await?;
        self.inner.session.transmit(data, skip_cad, timeout).await
    }
}

impl Drop for Dongle {
    fn drop(&mut self) {
        // Best-effort cleanup. If the caller wanted a clean async
        // shutdown they should have called `close().await` first; we
        // can only abort the keepalive task here.
        self.abort_keepalive();
    }
}

// Give ClientError a cheap clone-like helper for recording it into a
// TxAttempt (which is owned), without requiring Clone on the whole
// error type (std::io::Error isn't Clone).
impl ClientError {
    pub(crate) fn clone_kind(&self) -> ClientError {
        match self {
            Self::NotConfigured => Self::NotConfigured,
            Self::Busy => Self::Busy,
            Self::Param => Self::Param,
            Self::Length => Self::Length,
            Self::Modulation => Self::Modulation,
            Self::UnknownCmd => Self::UnknownCmd,
            Self::Radio => Self::Radio,
            Self::Frame => Self::Frame,
            Self::Internal => Self::Internal,
            Self::UnknownCode(c) => Self::UnknownCode(*c),
            Self::ChannelBusy => Self::ChannelBusy,
            Self::Cancelled => Self::Cancelled,
            Self::Timeout { what } => Self::Timeout { what },
            Self::TransportClosed(s) => Self::TransportClosed(s.clone()),
            Self::ReaderExited => Self::ReaderExited,
            Self::BadFrame(s) => Self::BadFrame(s.clone()),
            Self::EncodeFailed(s) => Self::EncodeFailed(s.clone()),
            Self::Io(e) => Self::Other(format!("io: {e}")),
            Self::Other(s) => Self::Other(s.clone()),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::retry::RetryPolicy;
    use donglora_protocol::{
        FrameDecoder, FrameResult, LoRaBandwidth, LoRaCodingRate, LoRaHeaderMode, Owner, RadioChipId, SetConfigResult,
        SetConfigResultCode, TxResult, commands, encode_frame, events,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    fn lora_cfg() -> Modulation {
        Modulation::LoRa(donglora_protocol::LoRaConfig {
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

    fn sample_info() -> Info {
        Info {
            proto_major: 1,
            proto_minor: 0,
            fw_major: 0,
            fw_minor: 1,
            fw_patch: 0,
            radio_chip_id: RadioChipId::Sx1262.as_u16(),
            capability_bitmap: donglora_protocol::cap::LORA | donglora_protocol::cap::CAD_BEFORE_TX,
            supported_sf_bitmap: 0x1FE0,
            supported_bw_bitmap: 0x03FF,
            max_payload_bytes: 255,
            rx_queue_capacity: 64,
            tx_queue_capacity: 16,
            freq_min_hz: 150_000_000,
            freq_max_hz: 960_000_000,
            tx_power_min_dbm: -9,
            tx_power_max_dbm: 22,
            mcu_uid_len: 0,
            mcu_uid: [0u8; donglora_protocol::MAX_MCU_UID_LEN],
            radio_uid_len: 0,
            radio_uid: [0u8; donglora_protocol::MAX_RADIO_UID_LEN],
        }
    }

    /// Spawn a fake device task on one side of a tokio duplex and return
    /// both (a) a [`Dongle`] wired to the other side and (b) a handle to
    /// the device so the test can drive responses on cue.
    async fn drain_one_frame(device: &mut DuplexStream) -> (u8, u16, Vec<u8>) {
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 256];
        loop {
            let n = device.read(&mut buf).await.unwrap();
            let mut out: Option<(u8, u16, Vec<u8>)> = None;
            decoder.feed(&buf[..n], |res| match res {
                FrameResult::Ok { type_id, tag, payload } => {
                    out = Some((type_id, tag, payload.to_vec()));
                }
                FrameResult::Err(e) => panic!("frame decode: {e:?}"),
            });
            if let Some(t) = out {
                return t;
            }
        }
    }

    async fn send_frame(device: &mut DuplexStream, type_id: u8, tag: u16, payload: &[u8]) {
        let mut wire = [0u8; donglora_protocol::MAX_WIRE_FRAME];
        let n = encode_frame(type_id, tag, payload, &mut wire).unwrap();
        device.write_all(&wire[..n]).await.unwrap();
        device.flush().await.unwrap();
    }

    #[tokio::test]
    async fn tx_success_path() {
        let (host, mut device) = tokio::io::duplex(1024);
        let session = Session::spawn(host);
        let dongle =
            Dongle::new(session, sample_info(), TransportKind::Serial("/dev/test".into()), Some(lora_cfg()), false);

        let tx_task = tokio::spawn({
            let dongle = dongle;
            async move {
                let td = dongle.tx(b"hello").await;
                dongle.close().await;
                td
            }
        });

        let (type_id, tag, _p) = drain_one_frame(&mut device).await;
        assert_eq!(type_id, commands::TYPE_TX);
        send_frame(&mut device, events::TYPE_OK, tag, &[]).await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let td = donglora_protocol::TxDonePayload { result: TxResult::Transmitted, airtime_us: 7_777 };
        let mut td_buf = [0u8; donglora_protocol::TxDonePayload::WIRE_SIZE];
        td.encode(&mut td_buf).unwrap();
        send_frame(&mut device, events::TYPE_TX_DONE, tag, &td_buf).await;

        let result = tx_task.await.unwrap().unwrap();
        assert_eq!(result.airtime_us, 7_777);
    }

    #[tokio::test]
    async fn tx_with_retry_succeeds_after_channel_busy() {
        let (host, mut device) = tokio::io::duplex(2048);
        let session = Session::spawn(host);
        let dongle =
            Dongle::new(session, sample_info(), TransportKind::Serial("/dev/test".into()), Some(lora_cfg()), false);

        let policy = RetryPolicy {
            max_attempts: 3,
            backoff_ms_min: 1,
            backoff_ms_max: 1,
            backoff_multiplier: 1.0,
            backoff_cap_ms: 10,
            per_attempt_timeout: Duration::from_secs(1),
            skip_cad: false,
        };

        let tx_task = tokio::spawn({
            let dongle = dongle;
            let p = policy.clone();
            async move {
                let outcome = dongle.tx_with_retry(b"hi", &p).await;
                dongle.close().await;
                outcome
            }
        });

        // Attempt 1 — CHANNEL_BUSY.
        let (_t1, tag1, _) = drain_one_frame(&mut device).await;
        send_frame(&mut device, events::TYPE_OK, tag1, &[]).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        let mut td_buf = [0u8; donglora_protocol::TxDonePayload::WIRE_SIZE];
        donglora_protocol::TxDonePayload { result: TxResult::ChannelBusy, airtime_us: 0 }.encode(&mut td_buf).unwrap();
        send_frame(&mut device, events::TYPE_TX_DONE, tag1, &td_buf).await;

        // Attempt 2 — TRANSMITTED.
        let (_t2, tag2, _) = drain_one_frame(&mut device).await;
        assert_ne!(tag1, tag2, "retry should use a fresh tag");
        send_frame(&mut device, events::TYPE_OK, tag2, &[]).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        donglora_protocol::TxDonePayload { result: TxResult::Transmitted, airtime_us: 9_000 }
            .encode(&mut td_buf)
            .unwrap();
        send_frame(&mut device, events::TYPE_TX_DONE, tag2, &td_buf).await;

        let outcome = tx_task.await.unwrap().unwrap();
        assert_eq!(outcome.attempts_used(), 2);
        assert!(outcome.had_retries());
        assert_eq!(outcome.final_airtime_us, 9_000);
    }

    #[tokio::test]
    async fn tx_auto_recovers_from_not_configured() {
        let (host, mut device) = tokio::io::duplex(2048);
        let session = Session::spawn(host);
        let cfg = lora_cfg();
        let dongle = Dongle::new(session, sample_info(), TransportKind::Serial("/dev/test".into()), Some(cfg), false);

        let task = tokio::spawn({
            let dongle = dongle;
            async move {
                let r = dongle.tx(b"abc").await;
                dongle.close().await;
                r
            }
        });

        // First TX → ERR(ENOTCONFIGURED).
        let (_t, tag1, _) = drain_one_frame(&mut device).await;
        let mut err_buf = [0u8; 2];
        donglora_protocol::events::encode_err_payload(donglora_protocol::ErrorCode::ENotConfigured, &mut err_buf)
            .unwrap();
        send_frame(&mut device, events::TYPE_ERR, tag1, &err_buf).await;

        // Auto-recovery: SET_CONFIG goes out. Reply APPLIED/MINE.
        let (set_type, tag_set, _) = drain_one_frame(&mut device).await;
        assert_eq!(set_type, commands::TYPE_SET_CONFIG);
        let result = SetConfigResult { result: SetConfigResultCode::Applied, owner: Owner::Mine, current: cfg };
        let mut rbuf = [0u8; donglora_protocol::MAX_SETCONFIG_OK_PAYLOAD];
        let n = result.encode(&mut rbuf).unwrap();
        send_frame(&mut device, events::TYPE_OK, tag_set, &rbuf[..n]).await;

        // Retry TX → succeed.
        let (_t, tag2, _) = drain_one_frame(&mut device).await;
        send_frame(&mut device, events::TYPE_OK, tag2, &[]).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        let mut td_buf = [0u8; donglora_protocol::TxDonePayload::WIRE_SIZE];
        donglora_protocol::TxDonePayload { result: TxResult::Transmitted, airtime_us: 1234 }
            .encode(&mut td_buf)
            .unwrap();
        send_frame(&mut device, events::TYPE_TX_DONE, tag2, &td_buf).await;

        let td = task.await.unwrap().unwrap();
        assert_eq!(td.airtime_us, 1234);
    }

    #[tokio::test]
    async fn drop_aborts_keepalive() {
        let (host, _device) = tokio::io::duplex(512);
        let session = Session::spawn(host);
        let dongle = Dongle::new(session, sample_info(), TransportKind::Serial("/dev/test".into()), None, true);
        // Keepalive is running. Dropping the Dongle must abort it.
        assert!(dongle.keepalive_handle.lock().unwrap().is_some());
        drop(dongle);
        // Give the abort a tick to land.
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
