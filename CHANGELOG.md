# Changelog

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
