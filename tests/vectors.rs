//! Smoke-level vector round-trips through the `donglora-client` re-exports.
//!
//! The authoritative Appendix C vectors live in
//! `donglora-protocol/tests/vectors.rs`. Here we check a representative
//! subset just to confirm the client crate's re-exports cover the shapes
//! a real application would use (so a refactor that accidentally hides
//! one of them fails here rather than in downstream code).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use donglora_client::{
    Command, LoRaBandwidth, LoRaCodingRate, LoRaConfig, LoRaHeaderMode, Modulation, RxOrigin, RxPayload, TxDonePayload,
    TxResult,
};
use donglora_protocol::{FrameDecoder, FrameResult, MAX_WIRE_FRAME, encode_frame};

fn meshcore_us() -> Modulation {
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

/// `SET_CONFIG(MeshCore US)` should encode/decode byte-for-byte through
/// the public API.
#[test]
fn set_config_roundtrip() {
    let cmd = Command::SetConfig(meshcore_us());
    let mut payload = [0u8; 256];
    let n = cmd.encode_payload(&mut payload).unwrap();
    let mut wire = [0u8; MAX_WIRE_FRAME];
    let wire_n = encode_frame(cmd.type_id(), 42, &payload[..n], &mut wire).unwrap();

    let mut decoder = FrameDecoder::new();
    let mut saw: Option<(u8, u16, Vec<u8>)> = None;
    decoder.feed(&wire[..wire_n], |res| match res {
        FrameResult::Ok { type_id, tag, payload } => {
            saw = Some((type_id, tag, payload.to_vec()));
        }
        FrameResult::Err(e) => panic!("decode error: {e:?}"),
    });
    let (type_id, tag, payload) = saw.expect("frame");
    assert_eq!(type_id, cmd.type_id());
    assert_eq!(tag, 42);

    let parsed = Command::parse(type_id, &payload).unwrap();
    match parsed {
        Command::SetConfig(Modulation::LoRa(c)) => {
            assert_eq!(c.freq_hz, 910_525_000);
            assert_eq!(c.sync_word, 0x1424);
            assert_eq!(c.tx_power_dbm, 20);
        }
        _ => panic!("expected LoRa SET_CONFIG"),
    }
}

/// `TX_DONE` round-trips with airtime preserved.
#[test]
fn tx_done_roundtrip() {
    let td = TxDonePayload { result: TxResult::Transmitted, airtime_us: 98_765 };
    let mut buf = [0u8; TxDonePayload::WIRE_SIZE];
    td.encode(&mut buf).unwrap();
    let got = TxDonePayload::decode(&buf).unwrap();
    assert_eq!(got, td);
}

/// `RX` event round-trips with metadata preserved.
#[test]
fn rx_roundtrip() {
    let mut data = heapless::Vec::<u8, { donglora_protocol::MAX_OTA_PAYLOAD }>::new();
    data.extend_from_slice(b"hello, lora!").unwrap();
    let rx = RxPayload {
        rssi_tenths_dbm: -812,
        snr_tenths_db: 73,
        freq_err_hz: -211,
        timestamp_us: 1_000_000_000_000,
        crc_valid: true,
        packets_dropped: 3,
        origin: RxOrigin::Ota,
        data,
    };
    let mut buf = [0u8; RxPayload::METADATA_SIZE + 64];
    let n = rx.encode(&mut buf).unwrap();
    let got = RxPayload::decode(&buf[..n]).unwrap();
    assert_eq!(got.rssi_tenths_dbm, -812);
    assert_eq!(got.snr_tenths_db, 73);
    assert_eq!(got.freq_err_hz, -211);
    assert_eq!(got.packets_dropped, 3);
    assert_eq!(&got.data[..], b"hello, lora!");
}
