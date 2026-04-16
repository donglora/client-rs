//! Connect, apply the MeshCore USA preset, and transmit a greeting
//! every 5 seconds. Run with:
//!
//! ```sh
//! cargo run --example tx
//! ```

#[path = "common/mod.rs"]
mod common;

use std::time::Duration;

use donglora_client::{ConnectOptions, connect_with};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).try_init();

    let dongle = connect_with(ConnectOptions::new().config(common::meshcore_us())).await?;
    println!("connected; TX ready");

    let mut seq: u32 = 0;
    loop {
        seq += 1;
        let payload = format!("hello #{seq}");
        match dongle.tx(payload.as_bytes()).await {
            Ok(td) => println!("TX #{seq} sent ({} us airtime)", td.airtime_us),
            Err(e) => eprintln!("TX #{seq} failed: {e}"),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
