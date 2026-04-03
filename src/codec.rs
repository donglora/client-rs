//! COBS framing: encode/decode frames and accumulate partial reads.
//!
//! Each frame is COBS-encoded and terminated with a `0x00` sentinel byte.
//! The sentinel never appears in the encoded data, so it unambiguously
//! marks frame boundaries.

use tracing::warn;

/// COBS-encode data and append the `0x00` sentinel.
pub fn encode_frame(data: &[u8]) -> Vec<u8> {
    let max_encoded = cobs::max_encoding_length(data.len());
    let mut buf = vec![0u8; max_encoded];
    let n = cobs::encode(data, &mut buf);
    buf.truncate(n);
    buf.push(0x00);
    buf
}

/// COBS-decode a frame (without the sentinel). Returns `None` on decode error.
pub fn decode_frame(encoded: &[u8]) -> Option<Vec<u8>> {
    if encoded.is_empty() {
        return None;
    }
    let mut buf = vec![0u8; encoded.len()];
    match cobs::decode(encoded, &mut buf) {
        Ok(n) => {
            buf.truncate(n);
            Some(buf)
        }
        Err(_) => None,
    }
}

/// Stateful COBS frame accumulator.
///
/// Feed raw byte chunks (from serial reads, socket reads, etc.) and extract
/// complete decoded frames. Handles partial reads, multiple frames per chunk,
/// and empty inter-frame gaps gracefully.
///
/// Used by both the sync client (`read_frame`) and the async mux to accumulate
/// data from serial and client socket streams.
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    /// Create a new empty frame reader.
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(512),
        }
    }

    /// Feed raw bytes and return all complete decoded frames.
    ///
    /// COBS decode errors are logged and the bad frame is skipped.
    /// Empty inter-frame gaps (consecutive `0x00` bytes) are ignored.
    pub fn feed(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(data);

        let mut frames = Vec::new();
        while let Some(sentinel_pos) = self.buf.iter().position(|&b| b == 0x00) {
            let encoded = &self.buf[..sentinel_pos];
            if !encoded.is_empty() {
                match decode_frame(encoded) {
                    Some(decoded) => frames.push(decoded),
                    None => warn!("bad COBS frame ({} bytes) — skipped", encoded.len()),
                }
            }
            // Remove the consumed bytes including the sentinel
            self.buf.drain(..=sentinel_pos);
        }
        frames
    }

    /// Return the number of buffered bytes not yet forming a complete frame.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }
}

impl Default for FrameReader {
    fn default() -> Self {
        Self::new()
    }
}

/// Read one complete COBS frame from a blocking `Read` source.
///
/// Reads byte-by-byte until a `0x00` sentinel is found, then COBS-decodes.
/// Returns `Ok(None)` on timeout (zero-length read from the underlying source).
/// Returns `Err` on I/O errors or COBS decode failures.
pub fn read_frame(reader: &mut dyn std::io::Read) -> anyhow::Result<Option<Vec<u8>>> {
    let mut buf = Vec::with_capacity(280);
    let mut byte = [0u8; 1];

    loop {
        match reader.read(&mut byte) {
            Ok(0) => {
                // Timeout or EOF
                return Ok(None);
            }
            Ok(_) => {
                if byte[0] == 0x00 {
                    break;
                }
                buf.push(byte[0]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        }
    }

    if buf.is_empty() {
        return Ok(None);
    }

    decode_frame(&buf).map(Some).ok_or_else(|| anyhow::anyhow!("COBS decode error"))
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let data = b"hello world";
        let encoded = encode_frame(data);
        // Encoded should end with 0x00 sentinel
        assert_eq!(encoded.last(), Some(&0x00));
        // Encoded should not contain 0x00 except the trailing sentinel
        assert!(!encoded[..encoded.len() - 1].contains(&0x00));
        // Decode (without sentinel)
        let decoded = decode_frame(&encoded[..encoded.len() - 1]);
        assert_eq!(decoded.as_deref(), Some(data.as_slice()));
    }

    #[test]
    fn encode_decode_empty() {
        // COBS encoding of empty data produces just the 0x00 sentinel.
        // This is equivalent to an empty inter-frame gap and is correctly
        // skipped by both FrameReader and read_frame.
        let encoded = encode_frame(b"");
        assert_eq!(encoded, vec![0x00]);
    }

    #[test]
    fn encode_decode_with_zeros() {
        let data = &[0x00, 0x01, 0x00, 0x02, 0x00];
        let encoded = encode_frame(data);
        let decoded = decode_frame(&encoded[..encoded.len() - 1]);
        assert_eq!(decoded.as_deref(), Some(data.as_slice()));
    }

    #[test]
    fn decode_frame_invalid() {
        // An invalid COBS sequence
        assert!(decode_frame(&[0x00]).is_none()); // empty after stripping would be caught
        // Actually the cobs crate might handle this differently, let's test empty
        assert!(decode_frame(&[]).is_none());
    }

    #[test]
    fn frame_reader_single_frame() {
        let mut reader = FrameReader::new();
        let data = b"test";
        let frame = encode_frame(data);
        let frames = reader.feed(&frame);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], data);
    }

    #[test]
    fn frame_reader_multiple_frames_at_once() {
        let mut reader = FrameReader::new();
        let f1 = encode_frame(b"one");
        let f2 = encode_frame(b"two");
        let mut combined = f1;
        combined.extend_from_slice(&f2);
        let frames = reader.feed(&combined);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], b"one");
        assert_eq!(frames[1], b"two");
    }

    #[test]
    fn frame_reader_partial_then_complete() {
        let mut reader = FrameReader::new();
        let frame = encode_frame(b"split");
        let mid = frame.len() / 2;

        // Feed first half — no complete frames yet
        let frames = reader.feed(&frame[..mid]);
        assert!(frames.is_empty());
        assert!(reader.buffered() > 0);

        // Feed second half — now we get the frame
        let frames = reader.feed(&frame[mid..]);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], b"split");
        assert_eq!(reader.buffered(), 0);
    }

    #[test]
    fn frame_reader_empty_gaps() {
        let mut reader = FrameReader::new();
        // Multiple sentinels in a row (empty gaps between frames)
        let f = encode_frame(b"data");
        let mut input = vec![0x00, 0x00]; // leading empty gaps
        input.extend_from_slice(&f);
        input.push(0x00); // trailing empty gap
        let frames = reader.feed(&input);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], b"data");
    }

    #[test]
    fn frame_reader_bad_cobs_skipped() {
        let mut reader = FrameReader::new();
        // Feed a known-bad COBS frame followed by a good one.
        // A frame where the first byte claims a run longer than the frame is invalid.
        let bad = &[0xFF, 0x01, 0x00]; // 0xFF says next 254 bytes, but only 1 present
        let good = encode_frame(b"ok");
        let mut input = bad.to_vec();
        input.extend_from_slice(&good);
        let frames = reader.feed(&input);
        // Bad frame skipped, good frame decoded
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], b"ok");
    }

    #[test]
    fn read_frame_from_bytes() {
        let data = b"frame";
        let encoded = encode_frame(data);
        let mut cursor = std::io::Cursor::new(encoded);
        let result = read_frame(&mut cursor);
        assert!(result.is_ok());
        assert_eq!(result.ok().flatten().as_deref(), Some(data.as_slice()));
    }

    #[test]
    fn read_frame_timeout() {
        // Empty reader → returns None (timeout)
        let mut cursor = std::io::Cursor::new(Vec::<u8>::new());
        let result = read_frame(&mut cursor);
        assert!(result.is_ok());
        assert!(result.ok().flatten().is_none());
    }

    #[test]
    fn read_frame_empty_gap_then_data() {
        let data = b"after_gap";
        let mut input = vec![0x00]; // empty gap
        input.extend_from_slice(&encode_frame(data));
        let mut cursor = std::io::Cursor::new(input);
        // First read_frame returns None for the empty gap
        let r1 = read_frame(&mut cursor);
        assert_eq!(r1.ok().flatten(), None);
        // Second read returns the actual frame
        let r2 = read_frame(&mut cursor);
        assert_eq!(r2.ok().flatten().as_deref(), Some(data.as_slice()));
    }

    #[test]
    fn encode_all_byte_values_roundtrip() {
        // Test that all possible byte values survive encode/decode
        let data: Vec<u8> = (0..=255).collect();
        let encoded = encode_frame(&data);
        let decoded = decode_frame(&encoded[..encoded.len() - 1]);
        assert_eq!(decoded, Some(data));
    }
}
