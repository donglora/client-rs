//! Connect, apply the MeshCore USA preset, and print every received
//! packet until interrupted. Run with:
//!
//! ```sh
//! cargo run --example rx
//! ```

#[path = "common/mod.rs"]
mod common;

use donglora_client::{ConnectOptions, connect_with};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).try_init();

    let dongle = connect_with(ConnectOptions::new().config(common::meshcore_us())).await?;
    println!("connected; listening...");

    while let Some(pkt) = dongle.next_rx().await {
        let rssi_dbm = pkt.rssi_tenths_dbm as f32 / 10.0;
        let snr_db = pkt.snr_tenths_db as f32 / 10.0;
        let text = String::from_utf8_lossy(&pkt.data);
        println!(
            "RX {:>4} bytes  rssi={:6.1} dBm  snr={:5.1} dB  crc={}  \"{}\"",
            pkt.data.len(),
            rssi_dbm,
            snr_db,
            if pkt.crc_valid { "ok" } else { "bad" },
            text
        );
    }

    Ok(())
}
