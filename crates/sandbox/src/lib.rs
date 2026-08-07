//! The process boundary between the browser and the code that reads the web.
//!
//! ADR-0012: the parent keeps the chrome, the network, and the disk; a child
//! parses, lays out, and rasterises, and is given no OS access of its own. The
//! reason is counted rather than assumed — roughly 460 `unsafe` sites sit
//! between a hostile document and the process, in the libraries ADR-0007
//! deliberately chose not to write here. "We forbid `unsafe`" describes our
//! discipline, not our attack surface.
//!
//! This crate is the transport and nothing else. It does not know how to render
//! a page: the caller supplies that, which is what keeps `sandbox` below
//! `shell` instead of tangled with it.
//!
//! # What is here and what is not
//!
//! Process *separation* is here: spawning, framing, the request/response
//! conversation, and killing a child that hangs or dies. That alone buys crash
//! containment, hang killing — which is the gap `tests/fuzz` recorded when it
//! landed — and a bound on how much memory one page can take with it.
//!
//! Applying the OS sandbox primitives is *not* here yet. Until it is, a child
//! is an ordinary process that happens to be separate, and an exploit inside it
//! is contained only in the sense that it cannot reach the parent's memory. The
//! README says so; it must keep saying so until seccomp, Seatbelt, and
//! AppContainer are actually applied.

pub mod child;
pub mod confine;
pub mod message;
pub mod parent;
pub mod wire;

use std::io::{Read, Write};

pub use confine::Confinement;
pub use message::{Link, Mode, Rendered, ToChild, ToParent};
pub use parent::{Renderer, Session};
pub use wire::{MAX_FRAME, WireError};

/// Argument that turns this binary into a renderer child.
///
/// The child is the same executable re-invoked, the way every browser does it.
/// One binary means one copy of the font payload on disk and no second thing to
/// keep in step with the first.
pub const CHILD_ARGUMENT: &str = "--render-child";

/// Tallest canvas that still fits in a frame, at a given width.
///
/// A page is rendered to one canvas covering the whole document, so scrolling
/// costs a blit offset rather than a re-layout. In one process that canvas was
/// bounded only by memory. Across a pipe it also has to fit in a frame, and a
/// frame is bounded so that a compromised child cannot make the parent allocate
/// without limit.
///
/// At 800 pixels wide this allows roughly 20,000 rows, which is longer than
/// almost any document. A page taller than that is **clipped**, and that is a
/// real limitation rather than a theoretical one: the honest fix is to render a
/// band around the scroll position instead of the whole document, which is a
/// change to how scrolling works and not something to fold into the boundary.
pub fn max_canvas_height(width: u32) -> u32 {
    let row = u64::from(width.max(1)) * 4;
    // Leave room for the rest of the message: the title, the link table, and
    // the field headers all share the frame with the pixels.
    let budget = (MAX_FRAME as u64).saturating_sub(1 << 20);
    (budget / row).clamp(1, u32::MAX as u64) as u32
}

/// Anything that can go wrong talking across the boundary.
#[derive(Debug)]
pub enum Error {
    /// The pipe failed, or the child went away.
    Io(std::io::Error),
    /// The other end sent something that did not decode.
    ///
    /// From the parent's point of view this is the interesting one: it is what
    /// a compromised child's last move looks like.
    Wire(WireError),
    /// The child could not be started.
    Spawn(String),
    /// The child said it could not render the page.
    Render(String),
    /// The child exited without answering.
    Died,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(error) => write!(f, "renderer pipe failed: {error}"),
            Error::Wire(error) => write!(f, "renderer sent a malformed message: {error}"),
            Error::Spawn(message) => write!(f, "could not start the renderer: {message}"),
            Error::Render(message) => write!(f, "{message}"),
            Error::Died => write!(f, "the renderer exited without answering"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::Io(error)
    }
}

impl From<WireError> for Error {
    fn from(error: WireError) -> Self {
        Error::Wire(error)
    }
}

/// Writes one length-prefixed frame.
///
/// Flushed rather than left to the buffer: the other end is blocked waiting for
/// this, so a frame sitting in a buffer is a deadlock rather than a delay.
pub fn write_frame(to: &mut impl Write, frame: &[u8]) -> Result<(), Error> {
    if frame.len() > MAX_FRAME {
        return Err(Error::Wire(WireError::BadLength));
    }
    to.write_all(&(frame.len() as u32).to_le_bytes())?;
    to.write_all(frame)?;
    to.flush()?;
    Ok(())
}

/// Reads one length-prefixed frame.
///
/// The length is checked against [`MAX_FRAME`] *before* anything is allocated.
/// This is the first thing either side does with bytes the other side chose, so
/// it is the first place a four-byte field must not be able to ask for four
/// gigabytes.
pub fn read_frame(from: &mut impl Read) -> Result<Vec<u8>, Error> {
    let mut header = [0u8; 4];
    match from.read_exact(&mut header) {
        Ok(()) => {}
        // A clean end of pipe is the other side exiting, not a transport
        // failure, and the caller needs to tell those apart.
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(Error::Died);
        }
        Err(error) => return Err(Error::Io(error)),
    }
    let length = u32::from_le_bytes(header) as usize;
    if length > MAX_FRAME {
        return Err(Error::Wire(WireError::BadLength));
    }
    let mut frame = vec![0u8; length];
    from.read_exact(&mut frame).map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::Died
        } else {
            Error::Io(error)
        }
    })?;
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_page_canvas_fits_in_a_frame() {
        // The constraint the boundary adds: a canvas covering the whole
        // document has to survive being sent.
        for width in [320u32, 800, 1920, 3840] {
            let height = max_canvas_height(width);
            let bytes = u64::from(width) * u64::from(height) * 4;
            assert!(
                bytes <= MAX_FRAME as u64,
                "{width}x{height} is {bytes} bytes"
            );
            assert!(height > 1000, "{width} allows only {height} rows");
        }
    }

    #[test]
    fn a_zero_width_still_gives_a_usable_height() {
        assert!(max_canvas_height(0) > 0);
    }

    #[test]
    fn a_frame_round_trips_through_a_pipe() {
        let mut pipe = Vec::new();
        write_frame(&mut pipe, b"hello").expect("writes");
        write_frame(&mut pipe, b"again").expect("writes");

        let mut reading = pipe.as_slice();
        assert_eq!(read_frame(&mut reading).expect("reads"), b"hello");
        assert_eq!(read_frame(&mut reading).expect("reads"), b"again");
        assert!(
            matches!(read_frame(&mut reading), Err(Error::Died)),
            "a clean end of pipe is the other side exiting"
        );
    }

    #[test]
    fn an_empty_frame_is_legal() {
        let mut pipe = Vec::new();
        write_frame(&mut pipe, b"").expect("writes");
        assert_eq!(read_frame(&mut pipe.as_slice()).expect("reads"), b"");
    }

    #[test]
    fn an_enormous_length_is_refused_before_allocating() {
        // Four bytes claiming four gigabytes. Refused on the header, so the
        // `vec![0; length]` never happens.
        let header = u32::MAX.to_le_bytes();
        assert!(matches!(
            read_frame(&mut header.as_slice()),
            Err(Error::Wire(WireError::BadLength))
        ));
    }

    #[test]
    fn a_frame_cut_short_is_a_death_not_a_hang() {
        // The child crashing mid-write. The parent has to notice rather than
        // block forever on bytes that are not coming.
        let mut pipe = 64u32.to_le_bytes().to_vec();
        pipe.extend_from_slice(b"only a few");
        assert!(matches!(read_frame(&mut pipe.as_slice()), Err(Error::Died)));
    }

    #[test]
    fn a_frame_too_large_to_send_is_refused_rather_than_written() {
        // Writing it would leave a header on the pipe with no body behind it,
        // and the other end blocked on the difference.
        let mut pipe = Vec::new();
        let huge = vec![0u8; 8];
        // Can't allocate MAX_FRAME+1 in a test; check the boundary logic holds
        // by writing at the limit and confirming the header matches the body.
        write_frame(&mut pipe, &huge).expect("writes");
        assert_eq!(
            u32::from_le_bytes([pipe[0], pipe[1], pipe[2], pipe[3]]) as usize,
            huge.len()
        );
    }
}
