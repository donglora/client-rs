//! Wire protocol types and fixed-size little-endian serialization.
//!
//! Mirrors the firmware's `protocol.rs` using std types. Every integer is
//! fixed-width LE.

/// Maximum LoRa payload size in bytes.
pub const MAX_PAYLOAD: usize = 256;

/// RadioConfig wire size (fixed).
pub const RADIO_CONFIG_SIZE: usize = 13;

/// Sentinel value for `tx_power_dbm`: use the board's maximum TX power.
pub const TX_POWER_MAX: i8 = i8::MIN; // -128 on the wire

/// Sentinel value for `preamble_len`: use the firmware default (16 symbols).
pub const PREAMBLE_DEFAULT: u16 = 0;

// ── Tag constants ──────────────────────────────────────────────────

/// Command tag for SetConfig.
pub const CMD_TAG_SET_CONFIG: u8 = 2;
/// Command tag for StartRx.
pub const CMD_TAG_START_RX: u8 = 3;
/// Command tag for StopRx.
pub const CMD_TAG_STOP_RX: u8 = 4;

/// Response tag for RxPacket.
pub const RESP_TAG_RX_PACKET: u8 = 2;
/// Response tag for Ok.
pub const RESP_TAG_OK: u8 = 4;
/// Response tag for Error.
pub const RESP_TAG_ERROR: u8 = 5;

/// Error code for InvalidConfig.
pub const ERROR_INVALID_CONFIG: u8 = 0;

// ── Bandwidth ──────────────────────────────────────────────────────

/// LoRa signal bandwidth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Bandwidth {
    Khz7 = 0,
    Khz10 = 1,
    Khz15 = 2,
    Khz20 = 3,
    Khz31 = 4,
    Khz41 = 5,
    Khz62 = 6,
    Khz125 = 7,
    Khz250 = 8,
    Khz500 = 9,
}

impl Bandwidth {
    /// Convert from raw byte. Returns `None` for invalid values.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Khz7),
            1 => Some(Self::Khz10),
            2 => Some(Self::Khz15),
            3 => Some(Self::Khz20),
            4 => Some(Self::Khz31),
            5 => Some(Self::Khz41),
            6 => Some(Self::Khz62),
            7 => Some(Self::Khz125),
            8 => Some(Self::Khz250),
            9 => Some(Self::Khz500),
            _ => None,
        }
    }
}

// ── RadioConfig ────────────────────────────────────────────────────

/// Complete LoRa radio configuration.
///
/// Wire layout (13 bytes, all little-endian):
/// ```text
/// [freq_hz:4] [bw:1] [sf:1] [cr:1] [sync_word:2] [tx_power_dbm:1] [preamble_len:2] [cad:1]
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RadioConfig {
    /// Frequency in Hz (150_000_000 - 960_000_000 for SX1262).
    pub freq_hz: u32,
    pub bw: Bandwidth,
    /// Spreading factor (5-12).
    pub sf: u8,
    /// Coding rate denominator (5-8). E.g. 5 = CR 4/5, 8 = CR 4/8.
    pub cr: u8,
    pub sync_word: u16,
    /// Transmit power in dBm. Set to [`TX_POWER_MAX`] (-128) for the board's maximum.
    pub tx_power_dbm: i8,
    /// Preamble length in symbols. Set to [`PREAMBLE_DEFAULT`] (0) for firmware default (16).
    pub preamble_len: u16,
    /// Channel Activity Detection (listen-before-talk). 0 = disabled, non-zero = enabled.
    pub cad: u8,
}

impl RadioConfig {
    /// Serialize to fixed-size LE bytes.
    pub fn to_bytes(&self) -> [u8; RADIO_CONFIG_SIZE] {
        let mut buf = [0u8; RADIO_CONFIG_SIZE];
        buf[0..4].copy_from_slice(&self.freq_hz.to_le_bytes());
        buf[4] = self.bw as u8;
        buf[5] = self.sf;
        buf[6] = self.cr;
        buf[7..9].copy_from_slice(&self.sync_word.to_le_bytes());
        buf[9] = self.tx_power_dbm as u8;
        buf[10..12].copy_from_slice(&self.preamble_len.to_le_bytes());
        buf[12] = self.cad;
        buf
    }

    /// Deserialize from fixed-size LE bytes. Returns `None` if too short or invalid.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < RADIO_CONFIG_SIZE {
            return None;
        }
        Some(Self {
            freq_hz: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            bw: Bandwidth::from_u8(buf[4])?,
            sf: buf[5],
            cr: buf[6],
            sync_word: u16::from_le_bytes([buf[7], buf[8]]),
            tx_power_dbm: buf[9] as i8,
            preamble_len: u16::from_le_bytes([buf[10], buf[11]]),
            cad: buf[12],
        })
    }
}

/// Default: 915 MHz, 125 kHz BW, SF7, CR 4/5, sync 0x1424, max power, default preamble, CAD on.
impl Default for RadioConfig {
    fn default() -> Self {
        Self {
            freq_hz: 915_000_000,
            bw: Bandwidth::Khz125,
            sf: 7,
            cr: 5,
            sync_word: 0x1424,
            tx_power_dbm: TX_POWER_MAX,
            preamble_len: PREAMBLE_DEFAULT,
            cad: 1,
        }
    }
}

// ── Command ────────────────────────────────────────────────────────

/// Host → firmware commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Ping,
    GetConfig,
    SetConfig(RadioConfig),
    StartRx,
    StopRx,
    Transmit { config: Option<RadioConfig>, payload: Vec<u8> },
    DisplayOn,
    DisplayOff,
    GetMac,
}

impl Command {
    /// Returns the wire tag byte for this command.
    pub fn tag(&self) -> u8 {
        match self {
            Self::Ping => 0,
            Self::GetConfig => 1,
            Self::SetConfig(_) => 2,
            Self::StartRx => 3,
            Self::StopRx => 4,
            Self::Transmit { .. } => 5,
            Self::DisplayOn => 6,
            Self::DisplayOff => 7,
            Self::GetMac => 8,
        }
    }

    /// Serialize to fixed-size LE bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Ping => vec![0],
            Self::GetConfig => vec![1],
            Self::SetConfig(cfg) => {
                let mut out = vec![2];
                out.extend_from_slice(&cfg.to_bytes());
                out
            }
            Self::StartRx => vec![3],
            Self::StopRx => vec![4],
            Self::Transmit { config, payload } => {
                let mut out = vec![5];
                match config {
                    None => out.push(0),
                    Some(cfg) => {
                        out.push(1);
                        out.extend_from_slice(&cfg.to_bytes());
                    }
                }
                out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
                out.extend_from_slice(payload);
                out
            }
            Self::DisplayOn => vec![6],
            Self::DisplayOff => vec![7],
            Self::GetMac => vec![8],
        }
    }

    /// Deserialize from a decoded frame. Returns `None` on malformed input.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        let tag = *buf.first()?;
        let rest = &buf[1..];
        match tag {
            0 => Some(Self::Ping),
            1 => Some(Self::GetConfig),
            2 => Some(Self::SetConfig(RadioConfig::from_bytes(rest)?)),
            3 => Some(Self::StartRx),
            4 => Some(Self::StopRx),
            5 => {
                if rest.is_empty() {
                    return None;
                }
                let (config, pos) = if rest[0] == 0 {
                    (None, 1)
                } else if rest[0] == 1 && rest.len() > RADIO_CONFIG_SIZE {
                    (Some(RadioConfig::from_bytes(&rest[1..])?), 1 + RADIO_CONFIG_SIZE)
                } else {
                    return None;
                };
                if rest.len() < pos + 2 {
                    return None;
                }
                let len = u16::from_le_bytes([rest[pos], rest[pos + 1]]) as usize;
                let data_start = pos + 2;
                if rest.len() < data_start + len || len > MAX_PAYLOAD {
                    return None;
                }
                Some(Self::Transmit { config, payload: rest[data_start..data_start + len].to_vec() })
            }
            6 => Some(Self::DisplayOn),
            7 => Some(Self::DisplayOff),
            8 => Some(Self::GetMac),
            _ => None,
        }
    }
}

// ── ErrorCode ──────────────────────────────────────────────────────

/// Error codes reported by the firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ErrorCode {
    InvalidConfig = 0,
    RadioBusy = 1,
    TxTimeout = 2,
    CrcError = 3,
    NotConfigured = 4,
    NoDisplay = 5,
}

impl ErrorCode {
    /// Convert from raw byte. Returns `None` for unknown codes.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::InvalidConfig),
            1 => Some(Self::RadioBusy),
            2 => Some(Self::TxTimeout),
            3 => Some(Self::CrcError),
            4 => Some(Self::NotConfigured),
            5 => Some(Self::NoDisplay),
            _ => None,
        }
    }
}

impl std::error::Error for ErrorCode {}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfig => write!(f, "InvalidConfig"),
            Self::RadioBusy => write!(f, "RadioBusy"),
            Self::TxTimeout => write!(f, "TxTimeout"),
            Self::CrcError => write!(f, "CrcError"),
            Self::NotConfigured => write!(f, "NotConfigured"),
            Self::NoDisplay => write!(f, "NoDisplay"),
        }
    }
}

// ── Response ───────────────────────────────────────────────────────

/// Firmware → host responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Pong,
    Config(RadioConfig),
    RxPacket { rssi: i16, snr: i16, payload: Vec<u8> },
    TxDone,
    Ok,
    Error(ErrorCode),
    MacAddress([u8; 6]),
}

impl Response {
    /// Returns the wire tag byte for this response.
    pub fn tag(&self) -> u8 {
        match self {
            Self::Pong => 0,
            Self::Config(_) => 1,
            Self::RxPacket { .. } => 2,
            Self::TxDone => 3,
            Self::Ok => 4,
            Self::Error(_) => 5,
            Self::MacAddress(_) => 6,
        }
    }

    /// Whether this is an unsolicited RxPacket.
    pub fn is_rx_packet(&self) -> bool {
        matches!(self, Self::RxPacket { .. })
    }

    /// Serialize to fixed-size LE bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Pong => vec![0],
            Self::Config(cfg) => {
                let mut out = vec![1];
                out.extend_from_slice(&cfg.to_bytes());
                out
            }
            Self::RxPacket { rssi, snr, payload } => {
                let mut out = vec![2];
                out.extend_from_slice(&rssi.to_le_bytes());
                out.extend_from_slice(&snr.to_le_bytes());
                out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
                out.extend_from_slice(payload);
                out
            }
            Self::TxDone => vec![3],
            Self::Ok => vec![4],
            Self::Error(code) => vec![5, *code as u8],
            Self::MacAddress(mac) => {
                let mut out = vec![6];
                out.extend_from_slice(mac);
                out
            }
        }
    }

    /// Deserialize from a decoded frame. Returns `None` on malformed input.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        let tag = *buf.first()?;
        let rest = &buf[1..];
        match tag {
            0 => Some(Self::Pong),
            1 => Some(Self::Config(RadioConfig::from_bytes(rest)?)),
            2 => {
                if rest.len() < 6 {
                    return None;
                }
                let rssi = i16::from_le_bytes([rest[0], rest[1]]);
                let snr = i16::from_le_bytes([rest[2], rest[3]]);
                let len = u16::from_le_bytes([rest[4], rest[5]]) as usize;
                if rest.len() < 6 + len || len > MAX_PAYLOAD {
                    return None;
                }
                Some(Self::RxPacket { rssi, snr, payload: rest[6..6 + len].to_vec() })
            }
            3 => Some(Self::TxDone),
            4 => Some(Self::Ok),
            5 => Some(Self::Error(ErrorCode::from_u8(*rest.first()?)?)),
            6 => {
                if rest.len() < 6 {
                    return None;
                }
                let mut mac = [0u8; 6];
                mac.copy_from_slice(&rest[..6]);
                Some(Self::MacAddress(mac))
            }
            _ => None,
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn make_config() -> RadioConfig {
        RadioConfig {
            freq_hz: 915_000_000,
            bw: Bandwidth::Khz125,
            sf: 7,
            cr: 5,
            sync_word: 0x3444,
            tx_power_dbm: 22,
            preamble_len: 16,
            cad: 1,
        }
    }

    // ── RadioConfig ────────────────────────────────────────────────

    #[test]
    fn radio_config_roundtrip() {
        let cfg = make_config();
        let bytes = cfg.to_bytes();
        assert_eq!(bytes.len(), RADIO_CONFIG_SIZE);
        assert_eq!(RadioConfig::from_bytes(&bytes), Some(cfg));
    }

    #[test]
    fn radio_config_default_roundtrip() {
        let cfg = RadioConfig::default();
        let bytes = cfg.to_bytes();
        assert_eq!(RadioConfig::from_bytes(&bytes), Some(cfg));
    }

    #[test]
    fn radio_config_all_bandwidths() {
        for bw_val in 0u8..=9 {
            let bw = Bandwidth::from_u8(bw_val);
            assert!(bw.is_some(), "bandwidth {bw_val} should be valid");
            let cfg = RadioConfig { bw: bw.unwrap(), ..make_config() };
            let bytes = cfg.to_bytes();
            assert_eq!(RadioConfig::from_bytes(&bytes), Some(cfg));
        }
    }

    #[test]
    fn radio_config_invalid_bandwidth() {
        let mut buf = make_config().to_bytes();
        buf[4] = 255;
        assert!(RadioConfig::from_bytes(&buf).is_none());
    }

    #[test]
    fn radio_config_negative_power() {
        let cfg = RadioConfig { tx_power_dbm: TX_POWER_MAX, ..make_config() };
        let bytes = cfg.to_bytes();
        assert_eq!(RadioConfig::from_bytes(&bytes), Some(cfg));
    }

    #[test]
    fn radio_config_short_buffer() {
        assert!(RadioConfig::from_bytes(&[0u8; 12]).is_none());
        assert!(RadioConfig::from_bytes(&[]).is_none());
    }

    // ── Command ────────────────────────────────────────────────────

    #[test]
    fn command_simple_roundtrips() {
        for cmd in [
            Command::Ping,
            Command::GetConfig,
            Command::StartRx,
            Command::StopRx,
            Command::DisplayOn,
            Command::DisplayOff,
            Command::GetMac,
        ] {
            let bytes = cmd.to_bytes();
            assert_eq!(Command::from_bytes(&bytes), Some(cmd));
        }
    }

    #[test]
    fn command_set_config_roundtrip() {
        let cmd = Command::SetConfig(make_config());
        let bytes = cmd.to_bytes();
        assert_eq!(Command::from_bytes(&bytes), Some(cmd));
    }

    #[test]
    fn command_transmit_no_config_roundtrip() {
        let cmd = Command::Transmit { config: None, payload: b"hello".to_vec() };
        let bytes = cmd.to_bytes();
        assert_eq!(Command::from_bytes(&bytes), Some(cmd));
    }

    #[test]
    fn command_transmit_with_config_roundtrip() {
        let cmd = Command::Transmit { config: Some(make_config()), payload: b"test".to_vec() };
        let bytes = cmd.to_bytes();
        assert_eq!(Command::from_bytes(&bytes), Some(cmd));
    }

    #[test]
    fn command_transmit_empty_payload() {
        let cmd = Command::Transmit { config: None, payload: vec![] };
        let bytes = cmd.to_bytes();
        assert_eq!(Command::from_bytes(&bytes), Some(cmd));
    }

    #[test]
    fn command_transmit_truncated() {
        assert!(Command::from_bytes(&[5]).is_none()); // tag only
        assert!(Command::from_bytes(&[5, 1]).is_none()); // has_config=1, no config
        assert!(Command::from_bytes(&[5, 0]).is_none()); // has_config=0, no length
    }

    #[test]
    fn command_invalid_tag() {
        assert!(Command::from_bytes(&[9]).is_none());
        assert!(Command::from_bytes(&[255]).is_none());
    }

    #[test]
    fn command_empty_buffer() {
        assert!(Command::from_bytes(&[]).is_none());
    }

    #[test]
    fn command_tags() {
        assert_eq!(Command::Ping.tag(), 0);
        assert_eq!(Command::GetConfig.tag(), 1);
        assert_eq!(Command::SetConfig(make_config()).tag(), 2);
        assert_eq!(Command::StartRx.tag(), 3);
        assert_eq!(Command::StopRx.tag(), 4);
        assert_eq!(Command::Transmit { config: None, payload: vec![] }.tag(), 5);
        assert_eq!(Command::DisplayOn.tag(), 6);
        assert_eq!(Command::DisplayOff.tag(), 7);
        assert_eq!(Command::GetMac.tag(), 8);
    }

    // ── Response ───────────────────────────────────────────────────

    #[test]
    fn response_simple_roundtrips() {
        for resp in [Response::Pong, Response::TxDone, Response::Ok] {
            let bytes = resp.to_bytes();
            assert_eq!(Response::from_bytes(&bytes), Some(resp));
        }
    }

    #[test]
    fn response_config_roundtrip() {
        let resp = Response::Config(make_config());
        let bytes = resp.to_bytes();
        assert_eq!(Response::from_bytes(&bytes), Some(resp));
    }

    #[test]
    fn response_rx_packet_roundtrip() {
        let resp = Response::RxPacket { rssi: -80, snr: 10, payload: b"data".to_vec() };
        let bytes = resp.to_bytes();
        assert_eq!(Response::from_bytes(&bytes), Some(resp));
    }

    #[test]
    fn response_rx_packet_empty_payload() {
        let resp = Response::RxPacket { rssi: -120, snr: -5, payload: vec![] };
        let bytes = resp.to_bytes();
        assert_eq!(Response::from_bytes(&bytes), Some(resp));
    }

    #[test]
    fn response_error_codes() {
        for code in [
            ErrorCode::InvalidConfig,
            ErrorCode::RadioBusy,
            ErrorCode::TxTimeout,
            ErrorCode::CrcError,
            ErrorCode::NotConfigured,
            ErrorCode::NoDisplay,
        ] {
            let resp = Response::Error(code);
            let bytes = resp.to_bytes();
            assert_eq!(Response::from_bytes(&bytes), Some(resp));
        }
    }

    #[test]
    fn response_mac_address_roundtrip() {
        let resp = Response::MacAddress([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        let bytes = resp.to_bytes();
        assert_eq!(Response::from_bytes(&bytes), Some(resp));
    }

    #[test]
    fn response_tags() {
        assert_eq!(Response::Pong.tag(), 0);
        assert_eq!(Response::Config(make_config()).tag(), 1);
        assert_eq!(Response::RxPacket { rssi: 0, snr: 0, payload: vec![] }.tag(), 2);
        assert_eq!(Response::TxDone.tag(), 3);
        assert_eq!(Response::Ok.tag(), 4);
        assert_eq!(Response::Error(ErrorCode::RadioBusy).tag(), 5);
        assert_eq!(Response::MacAddress([0; 6]).tag(), 6);
    }

    #[test]
    fn response_is_rx_packet() {
        assert!(Response::RxPacket { rssi: 0, snr: 0, payload: vec![] }.is_rx_packet());
        assert!(!Response::Ok.is_rx_packet());
        assert!(!Response::Pong.is_rx_packet());
    }

    #[test]
    fn response_invalid_tag() {
        assert!(Response::from_bytes(&[7]).is_none());
        assert!(Response::from_bytes(&[255]).is_none());
    }

    #[test]
    fn response_truncated() {
        assert!(Response::from_bytes(&[]).is_none());
        assert!(Response::from_bytes(&[5]).is_none()); // Error with no code
        assert!(Response::from_bytes(&[2, 0, 0]).is_none()); // RxPacket too short
        assert!(Response::from_bytes(&[6, 0, 0, 0]).is_none()); // MacAddress too short
    }

    // ── ErrorCode ──────────────────────────────────────────────────

    #[test]
    fn error_code_from_u8() {
        for v in 0..=5 {
            assert!(ErrorCode::from_u8(v).is_some());
        }
        assert!(ErrorCode::from_u8(6).is_none());
        assert!(ErrorCode::from_u8(255).is_none());
    }

    #[test]
    fn error_code_display() {
        assert_eq!(ErrorCode::InvalidConfig.to_string(), "InvalidConfig");
        assert_eq!(ErrorCode::RadioBusy.to_string(), "RadioBusy");
    }

    // ── Cross-compatibility with firmware encoding ─────────────────

    #[test]
    fn firmware_worked_example() {
        // From PROTOCOL.md: 915 MHz, 125 kHz BW, SF7, CR 4/5, sync 0x1424, max power
        let cfg = RadioConfig {
            freq_hz: 915_000_000,
            bw: Bandwidth::Khz125,
            sf: 7,
            cr: 5,
            sync_word: 0x1424,
            tx_power_dbm: TX_POWER_MAX,
            preamble_len: PREAMBLE_DEFAULT,
            cad: 1,
        };
        let cmd = Command::SetConfig(cfg);
        let bytes = cmd.to_bytes();
        // Expected: 02 C0 CA 89 36 07 07 05 24 14 80 00 00 01
        assert_eq!(bytes, [0x02, 0xC0, 0xCA, 0x89, 0x36, 0x07, 0x07, 0x05, 0x24, 0x14, 0x80, 0x00, 0x00, 0x01]);
    }
}
