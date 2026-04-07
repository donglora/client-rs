# DongLoRa Rust Client

Rust client library for talking to a DongLoRa device — either directly
over USB or through a mux daemon.

## Install

```toml
[dependencies]
donglora-client = "0.2"
```

## Quick Start

```rust
use donglora_client::*;

let mut client = connect_default()?;
client.set_config(RadioConfig::default())?;
client.start_rx()?;

loop {
    if let Some(Response::RxPacket { rssi, snr, payload }) = client.recv()? {
        println!("RX rssi={rssi} snr={snr} len={}", payload.len());
    }
}
```

All `connect` functions automatically validate the device by pinging it,
so you'll get a clear error immediately if the port isn't a real DongLoRa.

## What's in Here

- `src/protocol.rs` — wire protocol types (`RadioConfig`, `Command`, `Response`, `ErrorCode`)
- `src/codec.rs` — COBS framing, frame accumulator
- `src/discovery.rs` — USB VID:PID device discovery
- `src/transport.rs` — serial and mux socket transports
- `src/client.rs` — high-level `Client<T>` with send/recv
- `src/connect.rs` — auto-detection (mux socket, TCP, direct USB)

## Connection Priority

[`connect`] and [`try_connect`] try these in order:

1. **TCP mux** — via `DONGLORA_MUX_TCP` env var
2. **Unix socket mux** — checks `$XDG_RUNTIME_DIR/donglora/mux.sock` or `/tmp/donglora-mux.sock`
3. **Direct USB serial** — auto-detects by VID:PID

`try_connect` returns an error immediately if no device is found.
`connect` blocks until a USB device appears.

## Dependencies

- `ucobs` — COBS framing (same implementation as the firmware)
- `serialport` — USB serial communication
- `anyhow` — error handling
- `tracing` — logging
