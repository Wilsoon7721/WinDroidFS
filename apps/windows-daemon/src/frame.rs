//! Framing for messages sent over the USB accessory link.
//!
//! One frame looks like:
//! ```
//! [magic "WD": 2 bytes][payload length: u32 little-endian][payload][pad: 0 or 1 byte]
//! ```
//!
//! Copied over to Framing.kt in the Android app.

// Start of every frame
pub const MAGIC: [u8; 2] = *b"WD";
pub const HEADER_LEN: usize = 6;
pub const MAX_PAYLOAD: usize = 16 * 1024 * 1024;
const PACKET_ALIGN: usize = 512;

/// Determines how many padding bytes follow after a payload of `payload_len`
pub fn pad_len(payload_len: usize) -> usize {
    if (HEADER_LEN + payload_len).is_multiple_of(PACKET_ALIGN) {
        1
    } else {
        0
    }
}

/// Wrap `payload` in a frame, ready to send in a single write.
pub fn encode(payload: &[u8]) -> Vec<u8> {
    debug_assert!(payload.len() <= MAX_PAYLOAD, "payload too large to frame");
    let pad = pad_len(payload.len());
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len() + pad);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out.resize(out.len() + pad, 0);
    out
}

#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    /// A whole frame, has been removed from buffer
    Complete(Vec<u8>),
    /// Not enough bytes yet. Read more and try again.
    Incomplete,
    /// This many bytes of garbage were discarded while looking for the next frame. Safe to call again, there may be a frame right behind them.
    Skipped(usize),
}

/// Remove the next complete frame from the front of `buf`, if there is one.
///
/// `buf` is a reassembly buffer: append whatever each USB read returns, then call this in a loop until it says [`Frame::Incomplete`].
/// Bytes belonging to a frame that hasn't fully arrived are left in place for next time.
pub fn take_frame(buf: &mut Vec<u8>) -> Frame {
    if buf.len() < MAGIC.len() {
        return Frame::Incomplete;
    }

    if buf[..MAGIC.len()] != MAGIC {
        let skip = match find_magic(&buf[1..]) {
            Some(pos) => pos + 1,
            None if buf.last() == Some(&MAGIC[0]) => buf.len() - 1,
            None => buf.len(),
        };
        buf.drain(..skip);
        return Frame::Skipped(skip);
    }

    if buf.len() < HEADER_LEN {
        return Frame::Incomplete;
    }

    let len = u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]]) as usize;
    if len > MAX_PAYLOAD {
        // Impossible length. Skip and resync on the next one.
        buf.drain(..1);
        return Frame::Skipped(1);
    }

    let total = HEADER_LEN + len + pad_len(len);
    if buf.len() < total {
        return Frame::Incomplete;
    }

    let payload = buf[HEADER_LEN..HEADER_LEN + len].to_vec();
    buf.drain(..total);
    Frame::Complete(payload)
}

fn find_magic(haystack: &[u8]) -> Option<usize> {
    haystack.windows(MAGIC.len()).position(|w| w == MAGIC)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drain every complete frame out of `buf`, ignoring skips
    fn drain_all(buf: &mut Vec<u8>) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            match take_frame(buf) {
                Frame::Complete(p) => out.push(p),
                Frame::Skipped(_) => continue,
                Frame::Incomplete => return out,
            }
        }
    }

    /// Pinned to the same bytes as `knownBytes` in the phone's FramingTest.kt. This is what proves the two implementations agree on the format, not just each with itself.
    #[test]
    fn known_bytes() {
        assert_eq!(encode(b""), [0x57, 0x44, 0, 0, 0, 0]);
        assert_eq!(
            encode(b"hello"),
            [0x57, 0x44, 5, 0, 0, 0, b'h', b'e', b'l', b'l', b'o']
        );
        let padded = encode(&[1u8; 506]);
        assert_eq!(padded.len(), 513);
        assert_eq!(padded.last(), Some(&0));
    }

    #[test]
    fn round_trip() {
        let mut buf = encode(b"hello");
        assert_eq!(take_frame(&mut buf), Frame::Complete(b"hello".to_vec()));
        assert!(buf.is_empty());
    }

    #[test]
    fn empty_payload() {
        let mut buf = encode(b"");
        assert_eq!(take_frame(&mut buf), Frame::Complete(Vec::new()));
        assert!(buf.is_empty());
    }

    #[test]
    fn frame_split_across_reads() {
        let frame = encode(&[7u8; 3000]);
        let mut buf = Vec::new();
        for chunk in frame.chunks(700) {
            assert_eq!(take_frame(&mut buf), Frame::Incomplete);
            buf.extend_from_slice(chunk);
        }
        assert_eq!(take_frame(&mut buf), Frame::Complete(vec![7u8; 3000]));
    }

    #[test]
    fn two_frames_in_one_read() {
        let mut buf = encode(b"first");
        buf.extend(encode(b"second"));
        assert_eq!(
            drain_all(&mut buf),
            vec![b"first".to_vec(), b"second".to_vec()]
        );
        assert!(buf.is_empty());
    }

    #[test]
    fn partial_frame_is_kept_for_the_next_read() {
        let mut buf = encode(b"whole");
        let second = encode(b"split");
        buf.extend_from_slice(&second[..4]);
        assert_eq!(take_frame(&mut buf), Frame::Complete(b"whole".to_vec()));
        assert_eq!(take_frame(&mut buf), Frame::Incomplete);
        buf.extend_from_slice(&second[4..]);
        assert_eq!(take_frame(&mut buf), Frame::Complete(b"split".to_vec()));
    }

    #[test]
    fn pads_exactly_when_the_frame_would_end_on_a_packet_boundary() {
        // 6-byte header + 506 = 512: would end on a full packet, so it must be padded.
        assert_eq!(pad_len(506), 1);
        assert_eq!(encode(&[0u8; 506]).len(), 513);
        assert_eq!(pad_len(1018), 1); // 1024: covers USB 3's packet size too
        assert_eq!(pad_len(505), 0);
        assert_eq!(pad_len(507), 0);
    }

    #[test]
    fn no_frame_ever_ends_on_a_packet_boundary() {
        for len in 0..5000 {
            let total = encode(&vec![0u8; len]).len();
            assert!(
                !total.is_multiple_of(512),
                "length {len} produced a {total}-byte frame"
            );
        }
    }

    #[test]
    fn padded_frame_round_trips_and_leaves_nothing_behind() {
        let mut buf = encode(&[9u8; 506]);
        buf.extend(encode(b"next"));
        assert_eq!(drain_all(&mut buf), vec![vec![9u8; 506], b"next".to_vec()]);
        assert!(buf.is_empty());
    }

    #[test]
    fn resyncs_after_leading_garbage() {
        let mut buf = vec![1, 2, 3, 4];
        buf.extend(encode(b"after"));
        assert_eq!(take_frame(&mut buf), Frame::Skipped(4));
        assert_eq!(take_frame(&mut buf), Frame::Complete(b"after".to_vec()));
    }

    #[test]
    fn false_magic_with_absurd_length_is_skipped() {
        // "WD" followed by a length far past MAX_PAYLOAD, then a real frame.
        let mut buf = b"WD".to_vec();
        buf.extend_from_slice(&u32::MAX.to_le_bytes());
        buf.extend(encode(b"real"));
        assert_eq!(drain_all(&mut buf), vec![b"real".to_vec()]);
    }

    #[test]
    fn magic_split_across_reads_is_not_thrown_away() {
        let frame = encode(b"x");
        // Garbage, then only the first byte of the magic.
        let mut buf = vec![0xAA, frame[0]];
        assert_eq!(take_frame(&mut buf), Frame::Skipped(1));
        assert_eq!(buf, vec![frame[0]]);
        buf.extend_from_slice(&frame[1..]);
        assert_eq!(take_frame(&mut buf), Frame::Complete(b"x".to_vec()));
    }

    #[test]
    fn rejects_length_just_over_the_limit() {
        let mut buf = b"WD".to_vec();
        buf.extend_from_slice(&((MAX_PAYLOAD + 1) as u32).to_le_bytes());
        assert_eq!(take_frame(&mut buf), Frame::Skipped(1));
    }
}
