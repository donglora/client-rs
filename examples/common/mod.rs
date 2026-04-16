//! Shared defaults for the example binaries.
//!
//! Mirrors `examples/_common.py::MESHCORE_US`. See that file for the
//! full justification — short version: 910.525 MHz / BW 62.5 / SF 7 /
//! CR 4/5, sync 0x1424 (RADIOLIB_SX126X_SYNC_WORD_PRIVATE), 20 dBm.

use donglora_client::{LoRaBandwidth, LoRaCodingRate, LoRaConfig, LoRaHeaderMode, Modulation};

/// MeshCore's "USA/Canada (Recommended)" preset (October 2025 narrow).
pub fn meshcore_us() -> Modulation {
    Modulation::LoRa(LoRaConfig {
        freq_hz: 910_525_000,
        sf: 7,
        bw: LoRaBandwidth::Khz62,
        cr: LoRaCodingRate::Cr4_5,
        preamble_len: 16,
        sync_word: 0x1424,
        tx_power_dbm: 20,
        header_mode: LoRaHeaderMode::Explicit,
        payload_crc: true,
        iq_invert: false,
    })
}
