//! The bytes on the pipe.
//!
//! Hand-written rather than derived, for the reason ADR-0007 gives about
//! dependencies generally and for one specific to this file: a process boundary
//! is a parsing surface, and this one is reachable *from the sandboxed side*. A
//! compromised renderer's only remaining move is to send the parent something
//! malformed, so this decoder is the last thing standing between a contained
//! exploit and an uncontained one.
//!
//! Everything is length-prefixed, every length is checked against the bytes
//! actually present, and nothing is allocated on the strength of a length that
//! has not been validated first. A reader never panics; it returns
//! [`WireError`].
//!
//! Little-endian throughout. The two ends are the same binary on the same
//! machine, so there is no portability question to answer — but fixing the
//! order costs nothing and means a captured frame means one thing.

use std::fmt;

/// Largest frame either side will send or accept.
///
/// A pixmap is the big one: 4 bytes per pixel, and the renderer is bounded to a
/// canvas the window can show. 64 MiB is far past that and far below anything
/// that would trouble a machine — but the point of the cap is not the number,
/// it is that a length field cannot ask for an allocation nobody bounded.
pub const MAX_FRAME: usize = 64 * 1024 * 1024;

/// Why a frame could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// The frame ended in the middle of a value.
    Truncated,
    /// A length field was larger than the data behind it, or larger than
    /// [`MAX_FRAME`].
    BadLength,
    /// A string was not UTF-8.
    NotUtf8,
    /// A tag or enum discriminant that this version does not know.
    Unknown,
    /// Bytes were left over after the message was read.
    ///
    /// Not pedantry: trailing data means the two ends disagree about the shape
    /// of a message, and continuing would be acting on a guess.
    Trailing,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            WireError::Truncated => "frame ended mid-value",
            WireError::BadLength => "length field does not fit the frame",
            WireError::NotUtf8 => "string was not utf-8",
            WireError::Unknown => "unknown tag",
            WireError::Trailing => "trailing bytes after the message",
        };
        f.write_str(text)
    }
}

impl std::error::Error for WireError {}

/// Appends values to a frame.
#[derive(Debug, Default)]
pub struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    /// An empty frame.
    pub fn new() -> Self {
        Self::default()
    }

    /// The frame so far.
    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }

    /// Appends a single byte, used for tags and discriminants.
    pub fn tag(&mut self, tag: u8) {
        self.bytes.push(tag);
    }

    /// Appends a `u32`.
    pub fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// Appends a `u16`.
    pub fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// Appends an `f32`, by its bits.
    ///
    /// Bits rather than a decimal rendering: this carries geometry, and a
    /// round-trip through text would change it. `NaN` survives as `NaN`, which
    /// is correct — the reader's job is to transport what it was given, and the
    /// paint stage already refuses to draw non-finite coordinates.
    pub fn f32(&mut self, value: f32) {
        self.bytes.extend_from_slice(&value.to_bits().to_le_bytes());
    }

    /// Appends a length-prefixed byte string.
    pub fn bytes(&mut self, value: &[u8]) {
        self.u32(value.len() as u32);
        self.bytes.extend_from_slice(value);
    }

    /// Appends a length-prefixed string.
    pub fn str(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    /// Appends a presence flag, for an optional value.
    pub fn some(&mut self, present: bool) {
        self.bytes.push(u8::from(present));
    }
}

/// Reads values from a frame, checking every length against what is there.
#[derive(Debug)]
pub struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    /// Reads from `bytes`.
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    /// How many bytes are left.
    pub fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.at)
    }

    /// Fails unless the frame has been read exactly to its end.
    pub fn finish(self) -> Result<(), WireError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(WireError::Trailing)
        }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], WireError> {
        // Checked addition: `at + count` on a length near `usize::MAX` would
        // wrap and turn an overlong read into an apparently valid short one.
        let end = self.at.checked_add(count).ok_or(WireError::BadLength)?;
        let slice = self.bytes.get(self.at..end).ok_or(WireError::Truncated)?;
        self.at = end;
        Ok(slice)
    }

    /// Reads a tag byte.
    pub fn tag(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }

    /// Reads a `u32`.
    pub fn u32(&mut self) -> Result<u32, WireError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Reads a `u16`.
    pub fn u16(&mut self) -> Result<u16, WireError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// Reads an `f32`.
    pub fn f32(&mut self) -> Result<f32, WireError> {
        Ok(f32::from_bits(self.u32()?))
    }

    /// Reads a length-prefixed byte string.
    ///
    /// The length is checked against the bytes actually present *before*
    /// anything is allocated, which is the whole point of doing this by hand: a
    /// four-byte length field must never be able to ask for four gigabytes.
    pub fn bytes(&mut self) -> Result<&'a [u8], WireError> {
        let len = self.u32()? as usize;
        if len > MAX_FRAME || len > self.remaining() {
            return Err(WireError::BadLength);
        }
        self.take(len)
    }

    /// Reads a length-prefixed string.
    pub fn str(&mut self) -> Result<String, WireError> {
        let bytes = self.bytes()?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| WireError::NotUtf8)
    }

    /// Reads a presence flag.
    pub fn some(&mut self) -> Result<bool, WireError> {
        match self.tag()? {
            0 => Ok(false),
            1 => Ok(true),
            // Not "anything non-zero is true": a byte outside the two defined
            // values means the sender is not what we think it is.
            _ => Err(WireError::Unknown),
        }
    }

    /// Reads a count that is about to drive an allocation.
    ///
    /// Bounded by the bytes remaining, because no sequence can have more
    /// elements than there are bytes left to hold them — one element is at
    /// least one byte. That turns "trust me, there are four billion rectangles"
    /// into an error instead of a four-billion-element `Vec`.
    pub fn count(&mut self) -> Result<usize, WireError> {
        let count = self.u32()? as usize;
        if count > self.remaining() {
            return Err(WireError::BadLength);
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip() {
        let mut writer = Writer::new();
        writer.tag(7);
        writer.u32(0xdead_beef);
        writer.u16(0x1234);
        writer.f32(1.5);
        writer.str("hello");
        writer.bytes(&[1, 2, 3]);
        writer.some(true);
        let frame = writer.finish();

        let mut reader = Reader::new(&frame);
        assert_eq!(reader.tag(), Ok(7));
        assert_eq!(reader.u32(), Ok(0xdead_beef));
        assert_eq!(reader.u16(), Ok(0x1234));
        assert_eq!(reader.f32(), Ok(1.5));
        assert_eq!(reader.str().as_deref(), Ok("hello"));
        assert_eq!(reader.bytes(), Ok(&[1u8, 2, 3][..]));
        assert_eq!(reader.some(), Ok(true));
        assert_eq!(reader.finish(), Ok(()));
    }

    #[test]
    fn a_truncated_frame_is_an_error_rather_than_a_panic() {
        // Every one of these would be an index-out-of-bounds if the reader
        // trusted its input, and a panic in the parent is the sandboxed side
        // reaching out of the sandbox.
        let mut writer = Writer::new();
        writer.str("some text");
        let full = writer.finish();

        for cut in 0..full.len() {
            let mut reader = Reader::new(&full[..cut]);
            assert!(reader.str().is_err(), "cut at {cut} was accepted");
        }
    }

    #[test]
    fn a_length_larger_than_the_frame_is_refused_before_allocating() {
        // The classic: a four-byte length field asking for four gigabytes.
        // Refused on the length check, so nothing is allocated at all.
        let mut frame = 0xffff_ffffu32.to_le_bytes().to_vec();
        frame.extend_from_slice(b"only a few bytes");
        let mut reader = Reader::new(&frame);
        assert_eq!(reader.bytes(), Err(WireError::BadLength));
    }

    #[test]
    fn a_count_cannot_exceed_the_bytes_that_could_hold_it() {
        // No sequence has more elements than there are bytes left: one element
        // is at least one byte. Without this a count drives a huge `Vec`
        // reservation before the first element fails to read.
        let mut frame = 1_000_000u32.to_le_bytes().to_vec();
        frame.extend_from_slice(&[0; 8]);
        let mut reader = Reader::new(&frame);
        assert_eq!(reader.count(), Err(WireError::BadLength));

        let mut small = 2u32.to_le_bytes().to_vec();
        small.extend_from_slice(&[0; 8]);
        assert_eq!(Reader::new(&small).count(), Ok(2));
    }

    #[test]
    fn a_length_that_would_overflow_the_cursor_is_refused() {
        // `at + count` near `usize::MAX` wraps, and an overlong read becomes an
        // apparently valid short one. Checked addition, hence.
        let frame = u32::MAX.to_le_bytes();
        let mut reader = Reader::new(&frame);
        assert!(reader.bytes().is_err());
    }

    #[test]
    fn invalid_utf8_is_an_error_not_a_replacement_character() {
        // Lossy decoding here would mean the parent acting on a URL the child
        // did not send.
        let mut frame = 2u32.to_le_bytes().to_vec();
        frame.extend_from_slice(&[0xff, 0xfe]);
        let mut reader = Reader::new(&frame);
        assert_eq!(reader.str(), Err(WireError::NotUtf8));
    }

    #[test]
    fn a_presence_flag_outside_its_two_values_is_refused() {
        let frame = [42u8];
        assert_eq!(Reader::new(&frame).some(), Err(WireError::Unknown));
    }

    #[test]
    fn trailing_bytes_are_refused() {
        // The two ends disagreeing about a message's shape is not something to
        // continue past.
        let mut writer = Writer::new();
        writer.u32(1);
        let mut frame = writer.finish();
        frame.push(0);

        let mut reader = Reader::new(&frame);
        assert_eq!(reader.u32(), Ok(1));
        assert_eq!(reader.finish(), Err(WireError::Trailing));
    }

    #[test]
    fn an_empty_frame_reads_as_nothing_rather_than_panicking() {
        let mut reader = Reader::new(&[]);
        assert_eq!(reader.tag(), Err(WireError::Truncated));
        assert_eq!(reader.remaining(), 0);
    }

    #[test]
    fn geometry_survives_the_trip_exactly() {
        // Carried as bits, so a round trip through the pipe is not a round trip
        // through a decimal rendering.
        for value in [0.0f32, -0.0, 1.0 / 3.0, f32::MIN, f32::MAX, f32::EPSILON] {
            let mut writer = Writer::new();
            writer.f32(value);
            let frame = writer.finish();
            assert_eq!(
                Reader::new(&frame).f32().map(f32::to_bits),
                Ok(value.to_bits())
            );
        }
    }
}
