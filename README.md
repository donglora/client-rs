# DongLoRa Rust Client

[![Crates.io](https://img.shields.io/crates/v/donglora-client.svg)](https://crates.io/crates/donglora-client)
[![Docs.rs](https://docs.rs/donglora-client/badge.svg)](https://docs.rs/donglora-client)

Async Rust client library for talking to a DongLoRa device, either
directly over USB serial or through a mux daemon. Speaks DongLoRa
Protocol v2 via the [`donglora-protocol`][dp] crate.

[dp]: https://crates.io/crates/donglora-protocol

## Install

```toml
[dependencies]
donglora-client = "1"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

## Quick Start

```rust
use donglora_client::connect;
use donglora_protocol::{LoRaBandwidth, LoRaCodingRate, LoRaConfig, Modulation};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut dongle = connect(None, std::time::Duration::from_secs(5)).await?;

    dongle.set_config(&Modulation::LoRa(LoRaConfig {
        freq_hz: 910_525_000,
        bw: LoRaBandwidth::Khz62,
        sf: 7,
        cr: LoRaCodingRate::Cr4_5,
        sync_word: 0x1424,
        tx_power_dbm: 20,
        ..Default::default()
    })).await?;

    dongle.start_rx().await?;
    while let Some(rx) = dongle.next_rx().await? {
        println!("RX rssi={} snr={} len={}", rx.rssi_tenths_dbm, rx.snr_tenths_db, rx.data.len());
    }
    Ok(())
}
```

All `connect` functions automatically validate the device by pinging
it, so you get a clear error immediately if the port is not a real
DongLoRa.

## What's in Here

- `src/dongle.rs`: high-level `Dongle` API (connect, set_config,
  transmit, start_rx, next_rx, tx_with_retry).
- `src/session.rs`: reader task, tag-allocation, per-command response
  routing, async event fan-out.
- `src/retry.rs`: `RetryPolicy` + randomized backoff for
  `CHANNEL_BUSY` / `EBUSY` per `PROTOCOL.md §6.10`.
- `src/connect.rs`: auto-detection pipeline (TCP mux env var, unix
  socket mux, direct USB serial).
- `src/transport.rs`: tokio transports (`tokio-serial` for USB,
  `tokio::net::{UnixStream, TcpStream}` for mux).
- `src/discovery.rs`: USB VID:PID discovery + ping validation.
- `src/errors.rs`: `ClientError` / `ClientResult<T>` (thiserror-based
  taxonomy mirroring the Python client's `DongloraError`).

## Connection Priority

`connect(None, timeout)` tries these in order:

1. **TCP mux**: via `DONGLORA_MUX_TCP` env var (host:port).
2. **Unix socket mux**: `$XDG_RUNTIME_DIR/donglora/mux.sock` or
   `/tmp/donglora-mux.sock`.
3. **Direct USB serial**: auto-detects by VID:PID (CP210x, CH9102,
   CH340, FT232R bridge chips too).

Once a mux path wins, subsequent reconnects commit to that path and
will not fall back to USB, preventing port-steal races during mux
restart.

`try_connect(timeout)` returns an error immediately if no device is
found; `connect` blocks until one appears.

## TX with Retry

```rust
use donglora_client::retry::RetryPolicy;

let policy = RetryPolicy::default(); // 3 attempts, 20-100 ms backoff, 500 ms cap
let outcome = dongle.tx_with_retry(payload, &policy).await?;
println!("sent in {} attempt(s)", outcome.attempts.len());
```

## Keepalive

A background task pings the device every 500 ms when the host has
been idle, tracking the 1 s inactivity window from `PROTOCOL.md §3.4`
with a 2x safety margin. Disable via `ConnectOptions::keepalive(false)`
if you want to manage liveness yourself.

## License

MIT, see [`LICENSE`](LICENSE).
