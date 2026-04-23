# Changelog

## 1.0.2 — 2026-04-23

### Changed

- `donglora-protocol` dependency spec loosened from `"1.0.0"` to `"1"`.
  The resolver already accepted any `1.x.y` (both specs mean `^1.0.0`),
  but the `"1"` form signals intent: any 1.x release of the protocol
  crate is accepted without requiring a fresh `donglora-client`
  publication. Behavior unchanged.

## 1.0.1 — 2026-04-22

### Fixed

- `discovery::find_port` / `wait_for_device` now match the WCH CH340K
  USB-UART bridge (`1a86:7522`) and standard CH340 (`1a86:7523`) in
  the `BRIDGE_VID_PIDS` list. The Elecrow ThinkNode-M2 board ships
  with a CH340K bridge; previously only CP2102, CH9102, and FTDI
  bridges were recognized, so `donglora-mux` and any other
  `donglora-client` consumer reported "no device" on that board
  even when the serial port was clearly present.

## 1.0.0 — 2026-04-22

### Breaking

- **Migrated to the DongLoRa Protocol v2 wire format.** All message
  types, tag correlation, and framing now follow `PROTOCOL.md` v1.0. This is a
  wire-incompatible break with 0.x clients and firmware. `donglora-client` now
  depends on `donglora-protocol` for the normative type definitions.
- **Full tokio-async rewrite.** Public API is `async`; the blocking
  `Client<T: Transport>` is gone. Replace with `Dongle` from
  `donglora_client::connect()` (or the new `ConnectOptions` builder +
  `connect_with`). Serial I/O is now `tokio-serial`; mux connections are
  `tokio::net::UnixStream` / `tokio::net::TcpStream`.
- **`thiserror`-based error taxonomy.** `anyhow::Result` on public APIs
  is replaced by `donglora_client::ClientResult<T> = Result<T, ClientError>`.
  Variants mirror the Python client's `DongloraError` hierarchy plus
  transport-level conditions (`Timeout`, `TransportClosed`, `ReaderExited`,
  `BadFrame`).

### Added

- **`Dongle::tx_with_retry(data, &RetryPolicy)`** — randomized backoff +
  exponential retry for `TX_DONE(CHANNEL_BUSY)` and `ERR(EBUSY)`, per spec
  §6.10. Returns a [`TxOutcome`] capturing every per-attempt result so callers
  (the bridge TUI especially) can display retry state.
- **Auto-recovery from `ERR(ENOTCONFIGURED)`.** `Dongle` caches the applied
  config from the most recent `SET_CONFIG` and silently re-applies it + retries
  once when the firmware's inactivity timer expires mid-session.
- **Background keepalive task.** Spawned at connect time, pings the device
  every 500 ms when the host has been idle, tracking the 1 s inactivity window
  from spec §3.4 with 2× safety margin. Disable via
  `ConnectOptions::keepalive(false)`.
- **Rich `RxPayload` metadata.** Bridge-relevant fields (`rssi_tenths_dbm`,
  `snr_tenths_db`, `freq_err_hz`, `timestamp_us`, `crc_valid`,
  `packets_dropped`, `origin`) are surfaced directly from the wire.
- **New examples:** `cargo run --example tx` / `--example rx` alongside the
  existing `ping`, all using the MeshCore US preset
  (910.525 MHz / BW 62.5 / SF 7 / CR 4/5 / sync 0x1424 / 20 dBm).

### Removed

- The `Client<T>`, `Response`, `RadioConfig`, `Bandwidth`, and `TX_POWER_MAX`
  types. `Modulation` / `LoRaConfig` / `LoRaBandwidth` (re-exported from
  `donglora-protocol`) replace them.

## 0.2.1 — 2026-04-07

### Fixed

- **`connect()` no longer falls through from mux to USB serial.** If a mux
  socket file exists, `connect(None, timeout)` and `try_connect(timeout)` now
  commit to the mux and return an error if the mux is unreachable — instead of
  silently bypassing it and grabbing the USB port directly. This prevents a
  race condition where a client could steal the serial port from the mux during
  reconnect. Callers with explicit `port = Some(...)` are unaffected.

## 0.2.0 — 2026-04-07

### Features

- **Ping-on-connect validation** — all connect functions automatically ping the
  device after connecting and reject non-DongLoRa devices within 200ms. No more
  accidentally talking LoRa protocol to an Arduino.
- **`try_connect(timeout)`** — non-blocking alternative to `connect()`. Returns
  an error immediately if no DongLoRa device is found instead of blocking
  indefinitely.
- **USB-UART bridge chip fallback** — `find_port()` now detects boards using
  CP2102, CH9102, CH340, or FT232R bridge chips (validated via ping).
- **Payload size validation** — `transmit()` rejects payloads exceeding 256
  bytes with a clear error before hitting the wire.
- **`ErrorCode` implements `std::error::Error`** — usable in error chains.

### Breaking changes

- Wire-level tag constants (`CMD_TAG_*`, `RESP_TAG_*`, `ERROR_INVALID_CONFIG`)
  removed from the top-level re-exports. Access them via
  `donglora_client::protocol::*` if needed.
- `connect()` and friends now validate the device on connect. Code that
  previously connected to non-DongLoRa serial devices will now get an error.

### Fixes

- Fixed repository URL in crate metadata.
- `drain_rx` no longer swallows the original error if timeout restore also fails.
- Removed dead `firmware/PROTOCOL.md` reference from docs.

## 0.1.0 — 2026-04-06

Initial release.

### Features

- High-level `Client<T>` with send/recv and command helpers (ping, set_config,
  start_rx, stop_rx, transmit, get_config, get_mac, display_on, display_off)
- COBS wire framing via `ucobs` (matches firmware implementation)
- Auto-detection connection: TCP mux, Unix socket mux, direct USB serial
- Bounded RX packet buffering (256 packets, FIFO eviction)
- `FrameReader` accumulator for streaming byte sources
- USB device discovery by VID:PID with blocking wait

### Resilience

- Cross-platform timeout handling: `TimedOut` (Windows) and `WouldBlock`
  (Linux/macOS) both treated as clean timeouts in `read_frame`
- `EINTR`/`Interrupted` signals retried automatically in `read_frame`
- `drain_rx` always restores the original timeout, even on I/O errors
- TCP mux connections use `connect_timeout` (bounded by caller's timeout)
- Mux sockets set both read and write timeouts
- `SerialTransport` tracks timeout accurately for save/restore
- Unexpected unsolicited frames logged via `tracing::warn` instead of silently
  discarded
