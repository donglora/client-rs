//! USB device discovery for DongLoRa dongles.
//!
//! Finds the serial port by matching USB VID:PID using the `serialport` crate.

use std::thread;
use std::time::Duration;

use tracing::info;

/// DongLoRa USB Vendor ID.
pub const USB_VID: u16 = 0x1209;

/// DongLoRa USB Product ID.
pub const USB_PID: u16 = 0x5741;

/// Known USB-UART bridge VID:PIDs found on some board revisions.
const BRIDGE_VID_PIDS: &[(u16, u16)] = &[
    (0x10C4, 0xEA60), // CP2102 (Silicon Labs)
    (0x1A86, 0x55D4), // CH9102
    (0x1A86, 0x7523), // CH340
    (0x0403, 0x6001), // FT232R (FTDI)
];

/// Find the DongLoRa serial port by USB VID:PID.
///
/// Checks for native USB CDC-ACM first (VID:PID 1209:5741), then falls
/// back to known USB-UART bridge chips found on some board revisions.
/// Returns the first matching port path, or `None` if no device is found.
pub fn find_port() -> Option<String> {
    let ports = serialport::available_ports().ok()?;

    // Prefer native USB (our custom VID:PID)
    if let Some(port) = ports.iter().find(|p| {
        matches!(
            &p.port_type,
            serialport::SerialPortType::UsbPort(info)
                if info.vid == USB_VID && info.pid == USB_PID
        )
    }) {
        return Some(port.port_name.clone());
    }

    // Fall back to known USB-UART bridge chips
    ports
        .into_iter()
        .find(|p| {
            matches!(
                &p.port_type,
                serialport::SerialPortType::UsbPort(info)
                    if BRIDGE_VID_PIDS.contains(&(info.vid, info.pid))
            )
        })
        .map(|p| p.port_name)
}

/// Block until a DongLoRa device appears on USB.
///
/// Polls [`find_port`] every 500ms and returns the port path once found.
pub fn wait_for_device() -> String {
    info!("waiting for DongLoRa device...");
    loop {
        if let Some(port) = find_port() {
            info!("found device at {port}");
            // Brief delay for USB enumeration to settle
            thread::sleep(Duration::from_millis(300));
            return port;
        }
        thread::sleep(Duration::from_millis(500));
    }
}
