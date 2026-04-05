# Changelog

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
