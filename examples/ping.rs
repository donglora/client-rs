//! Connect to a DongLoRa device, ping it, and print the MAC address.
//!
//! Run with: cargo run --example ping

use donglora_client::{RadioConfig, try_connect};
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let mut client = try_connect(Duration::from_secs(2))?;
    println!("connected!");

    client.ping()?;
    println!("ping ok");

    let mac = client.get_mac()?;
    println!("MAC: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);

    let config = client.get_config()?;
    println!("current config: {config:?}");

    client.set_config(RadioConfig::default())?;
    println!("config set to defaults");

    Ok(())
}
