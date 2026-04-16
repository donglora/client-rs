//! Connect to a DongLoRa device, ping it, and print its info.
//!
//! Run with: `cargo run --example ping`

use donglora_client::connect;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber_init();

    let dongle = connect().await?;
    println!("connected!");

    dongle.ping().await?;
    println!("ping ok");

    let info = dongle.info();
    println!("firmware: v{}.{}.{} ({:?})", info.fw_major, info.fw_minor, info.fw_patch, info.chip_id());
    println!("TX power range: {} .. {} dBm", info.tx_power_min_dbm, info.tx_power_max_dbm);
    println!("max OTA payload: {} bytes", info.max_payload_bytes);

    dongle.close().await;
    Ok(())
}

fn tracing_subscriber_init() {
    // Best-effort subscriber; ignore errors if one is already set.
    use tracing_subscriber::{EnvFilter, fmt};
    let _ = fmt().with_env_filter(EnvFilter::from_default_env()).try_init();
}
