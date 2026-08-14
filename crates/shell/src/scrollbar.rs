//! The page's scrollbar: where its thumb sits, and where a grab on it lands.
//!
//! A browser that scrolls but never says how far through the page you are is
//! missing the oldest piece of feedback there is. The wheel worked; nothing
//! told a reader whether the article ran on for two more screens or twenty, and
//! nothing gave them a way to get to the end in one movement.
//!
//! Separated from `window.rs` for the reason every other testable piece of the
//! shell is — [`crate::history`], [`crate::tabs`], [`crate::field`]. CI has no
//! display server, so anything left inside the event loop is exercised by hand
//! and by nothing else. The arithmetic lives here and `draw` only paints it.

/// How wide the bar is.
///
/// Exactly the page gutter, so the bar sits in the margin the page already
/// keeps clear rather than on top of the first column of text. An overlay bar
/// rather than a reserved column: whether a page needs one is not known until
/// it has been laid out, and laying every page out for a bar it may not need
/// would leave a dead strip down the side of every short one.
pub const WIDTH: u32 = 8;

/// Shortest the thumb may be drawn.
///
/// A thumb proportional to a very long page shrinks to nothing, and a target
/// two pixels tall cannot be grabbed. Below this it stops shrinking and starts
/// lying slightly about how much of the page is on screen, which is the trade
/// every scrollbar makes.
const MIN_THUMB: f32 = 24.0;

/// Where the thumb sits in a track `track` pixels tall, as `(top, height)`.
///
/// `None` when the page fits: there is nothing to scroll, so there is nothing
/// to say, and a full-length thumb would only be furniture.
pub fn thumb(scroll: f32, content: f32, track: f32) -> Option<(f32, f32)> {
    if track <= 0.0 || content <= track {
        return None;
    }
    // The thumb is as long a share of the track as the window is of the page,
    // which is what makes its length mean something.
    let height = (track * track / content).clamp(MIN_THUMB.min(track), track);
    // The travel is the track less the thumb, not the whole track: at the
    // bottom of the page the *bottom* of the thumb is at the bottom of the
    // track. Measuring against the whole track would run it off the end.
    let travel = track - height;
    let furthest = content - track;
    let progress = if furthest > 0.0 {
        (scroll / furthest).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some((travel * progress, height))
}

/// The scroll offset that puts the top of the thumb at `top` in the track.
///
/// The inverse of [`thumb`], for dragging: the pointer moves the thumb and the
/// page has to follow it exactly, or the thumb slides out from under the finger
/// holding it.
pub fn scroll_at(top: f32, content: f32, track: f32) -> f32 {
    let Some((_, height)) = thumb(0.0, content, track) else {
        return 0.0;
    };
    let travel = track - height;
    let furthest = (content - track).max(0.0);
    if travel <= 0.0 {
        return furthest;
    }
    (top / travel).clamp(0.0, 1.0) * furthest
}

/// What a press at `y` in the track means.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Grab {
    /// On the thumb, `f32` pixels down from its top. Dragging from here has to
    /// keep that distance, or the thumb jumps under the pointer on the first
    /// press.
    Thumb(f32),
    /// On the track, which scrolls to put the thumb's middle there — one
    /// movement to anywhere in the document, which is most of why a scrollbar
    /// is worth being able to click at all.
    Track,
}

/// Where a press at `y` in a track `track` tall landed, if anywhere.
pub fn grab(y: f32, scroll: f32, content: f32, track: f32) -> Option<Grab> {
    let (top, height) = thumb(scroll, content, track)?;
    if y < 0.0 || y > track {
        return None;
    }
    if y >= top && y < top + height {
        Some(Grab::Thumb(y - top))
    } else {
        Some(Grab::Track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_that_fits_has_no_thumb() {
        assert_eq!(thumb(0.0, 400.0, 400.0), None);
        assert_eq!(thumb(0.0, 100.0, 400.0), None, "shorter than the window");
    }

    #[test]
    fn the_thumb_is_as_long_a_share_of_the_track_as_the_window_is_of_the_page() {
        // Half the page on screen, half the track filled.
        let (top, height) = thumb(0.0, 800.0, 400.0).expect("a scrollable page");
        assert_eq!(top, 0.0);
        assert_eq!(height, 200.0);
    }

    #[test]
    fn the_thumb_reaches_the_bottom_of_the_track_at_the_bottom_of_the_page() {
        // The travel is the track less the thumb. Measured against the whole
        // track instead, the thumb runs off the end of the bar exactly when the
        // reader most wants to see that they have arrived.
        let (top, height) = thumb(400.0, 800.0, 400.0).expect("a scrollable page");
        assert_eq!(top + height, 400.0, "the thumb stopped short or overran");
    }

    #[test]
    fn the_thumb_stops_shrinking_before_it_becomes_ungrabbable() {
        let (_, height) = thumb(0.0, 1_000_000.0, 400.0).expect("a scrollable page");
        assert!(height >= MIN_THUMB, "a {height}px thumb cannot be grabbed");
    }

    #[test]
    fn a_track_shorter_than_the_minimum_thumb_still_gets_one_that_fits() {
        // A very short window: the floor cannot be allowed to make a thumb
        // longer than the bar holding it.
        let (top, height) = thumb(0.0, 1000.0, 10.0).expect("a scrollable page");
        assert!(height <= 10.0, "a {height}px thumb in a 10px track");
        assert!(top + height <= 10.0);
    }

    #[test]
    fn dragging_the_thumb_is_the_inverse_of_drawing_it() {
        // What a drag needs: put the thumb somewhere, ask what scroll that is,
        // and the thumb must come back to where it was put. Anything else and
        // the thumb slides away from the pointer holding it.
        let (content, track) = (2000.0, 500.0);
        for scroll in [0.0, 1.0, 375.0, 750.0, 1200.0, 1500.0] {
            let (top, _) = thumb(scroll, content, track).expect("a scrollable page");
            let round_tripped = scroll_at(top, content, track);
            assert!(
                (round_tripped - scroll).abs() < 0.01,
                "a thumb at {top} came back as {round_tripped}, not {scroll}"
            );
        }
    }

    #[test]
    fn dragging_past_either_end_stops_at_it() {
        let (content, track) = (2000.0, 500.0);
        assert_eq!(scroll_at(-100.0, content, track), 0.0);
        assert_eq!(scroll_at(9999.0, content, track), 1500.0);
    }

    #[test]
    fn a_press_on_the_thumb_remembers_where_on_it() {
        // Without the offset the thumb jumps so its top is under the pointer
        // the instant it is pressed, which moves the page before the drag has
        // begun.
        let (content, track) = (800.0, 400.0);
        let (top, height) = thumb(200.0, content, track).expect("a scrollable page");
        assert_eq!(
            grab(top + 10.0, 200.0, content, track),
            Some(Grab::Thumb(10.0))
        );
        // The last pixel of the thumb is still the thumb.
        assert_eq!(
            grab(top + height - 0.5, 200.0, content, track),
            Some(Grab::Thumb(height - 0.5))
        );
    }

    #[test]
    fn a_press_off_the_thumb_is_a_press_on_the_track() {
        let (content, track) = (800.0, 400.0);
        let (top, height) = thumb(200.0, content, track).expect("a scrollable page");
        assert_eq!(grab(top - 1.0, 200.0, content, track), Some(Grab::Track));
        assert_eq!(
            grab(top + height + 1.0, 200.0, content, track),
            Some(Grab::Track)
        );
    }

    #[test]
    fn a_press_on_a_page_that_fits_is_not_a_press_on_anything() {
        assert_eq!(grab(10.0, 0.0, 300.0, 400.0), None);
    }
}
