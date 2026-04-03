# DongLoRa Rust Client

Rust client library for talking to a DongLoRa device — either directly
over USB or through the [mux daemon](../../mux/).

## What's in Here

- `src/protocol.rs` — wire protocol types (`RadioConfig`, `Command`, `Response`, `ErrorCode`)
- `src/codec.rs` — COBS framing, frame accumulator
- `src/discovery.rs` — USB VID:PID device discovery
- `src/transport.rs` — serial and mux socket transports
- `src/client.rs` — high-level `Client<T>` with send/recv
- `src/connect.rs` — auto-detection (mux socket, TCP, direct USB)

## Usage

```rust
use donglora_client::*;

let mut client = connect_default()?;
client.ping()?;
client.set_config(RadioConfig::default())?;
client.start_rx()?;

loop {
    if let Some(Response::RxPacket { rssi, snr, payload }) = client.recv()? {
        println!("RX rssi={rssi} snr={snr} len={}", payload.len());
    }
}
```

## Dependencies

- `cobs` — COBS framing
- `serialport` — USB serial communication
- `anyhow` — error handling
- `tracing` — logging
