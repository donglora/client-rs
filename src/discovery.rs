//! USB device discovery for DongLoRa dongles.
//!
//! Finds the serial port by matching USB VID:PID. Re-uses
//! `tokio_serial::available_ports` (which re-exports `mio-serial`'s
//! implementation) rather than pulling in `serialport` as a second
//! direct dep.

use std::time::Duration;

use tokio_serial::{SerialPortType, available_ports};
use tracing::info;

/// DongLoRa USB Vendor ID.
pub const USB_VID: u16 = 0x1209;

/// DongLoRa USB Product ID.
pub const USB_PID: u16 = 0x5741;

/// Known USB-UART bridge VID:PIDs found on some board revisions.
const BRIDGE_VID_PIDS: &[(u16, u16)] = &[
    (0x10C4, 0xEA60), // CP2102 (Silicon Labs)
    (0x1A86, 0x55D4), // CH9102
    (0x1A86, 0x7522), // CH340K — Elecrow ThinkNode-M2 ships with this
    (0x1A86, 0x7523), // CH340
    (0x0403, 0x6001), // FT232R (FTDI)
];

/// How long to wait for USB enumeration to settle after a device
/// appears but before we try to open the port. Empirically 300 ms is
/// enough on Linux and macOS to avoid the first-open race.
const USB_SETTLE: Duration = Duration::from_millis(300);

/// Poll interval for [`wait_for_device`].
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Non-blocking one-shot scan. Returns the port path of the first
/// DongLoRa-like device present, or `None`.
///
/// Native VID:PID wins over known USB-UART bridges, so if both an
/// original and a bridge-chip board are plugged in, the native one is
/// selected.
pub fn find_port() -> Option<String> {
    let ports = available_ports().ok()?;

    if let Some(port) = ports.iter().find(|p| {
        matches!(
            &p.port_type,
            SerialPortType::UsbPort(info) if info.vid == USB_VID && info.pid == USB_PID
        )
    }) {
        return Some(port.port_name.clone());
    }

    ports
        .into_iter()
        .find(|p| {
            matches!(
                &p.port_type,
                SerialPortType::UsbPort(info) if BRIDGE_VID_PIDS.contains(&(info.vid, info.pid))
            )
        })
        .map(|p| p.port_name)
}

/// Await a DongLoRa device on USB. Polls [`find_port`] every 500 ms and
/// returns when one appears. Does not time out — the caller is expected
/// to wrap the future with `tokio::time::timeout` if that matters.
pub async fn wait_for_device() -> String {
    info!("waiting for DongLoRa device...");
    loop {
        if let Some(port) = find_port() {
            info!("found device at {port}");
            tokio::time::sleep(USB_SETTLE).await;
            return port;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
