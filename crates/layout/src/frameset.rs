//! Frameset geometry.
//!
//! `<frameset>` was everywhere on the era's web and exists nowhere on the
//! modern one, so it is in scope precisely because ADR-0004 targets that era.
//!
//! This module is only the arithmetic: turning a `rows`/`cols` specification
//! into pixel tracks. Loading each frame's document is the shell's job, because
//! a frame is a whole separate page.

/// One entry in a `rows` or `cols` specification.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Track {
    /// A fixed pixel size.
    Pixels(f32),
    /// A percentage of the available space.
    Percent(f32),
    /// A share of whatever is left over, from `*` or `2*`.
    Relative(f32),
}

/// Parses a `rows` or `cols` attribute.
///
/// An absent or empty specification means a single full-size track, which is
/// what a `<frameset>` with only one dimension declared relies on.
pub fn parse_spec(spec: &str) -> Vec<Track> {
    let tracks: Vec<Track> = spec
        .split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            if entry.is_empty() {
                return None;
            }
            if let Some(stripped) = entry.strip_suffix('*') {
                // A bare `*` is one share; `3*` is three.
                let share = if stripped.is_empty() {
                    1.0
                } else {
                    stripped.parse().unwrap_or(1.0)
                };
                return Some(Track::Relative(share));
            }
            if let Some(stripped) = entry.strip_suffix('%') {
                return stripped.trim().parse().ok().map(Track::Percent);
            }
            // Trailing junk is common; take the leading number.
            let digits: String = entry
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            digits.parse().ok().map(Track::Pixels)
        })
        .collect();

    if tracks.is_empty() {
        vec![Track::Relative(1.0)]
    } else {
        tracks
    }
}

/// Resolves tracks to pixel sizes filling `total`.
///
/// Fixed sizes are honoured first, then percentages of the original total, and
/// whatever remains is split between the relative tracks in proportion to their
/// shares. When there is nothing left over, relative tracks collapse to zero
/// rather than pushing the frameset past its container.
pub fn distribute(tracks: &[Track], total: f32) -> Vec<f32> {
    let mut sizes: Vec<f32> = tracks
        .iter()
        .map(|track| match track {
            Track::Pixels(value) => value.max(0.0),
            Track::Percent(value) => (value / 100.0 * total).max(0.0),
            Track::Relative(_) => 0.0,
        })
        .collect();

    let fixed: f32 = sizes.iter().sum();
    let remaining = (total - fixed).max(0.0);
    let shares: f32 = tracks
        .iter()
        .filter_map(|track| match track {
            Track::Relative(share) => Some(*share),
            _ => None,
        })
        .sum();

    if shares > 0.0 {
        for (size, track) in sizes.iter_mut().zip(tracks) {
            if let Track::Relative(share) = track {
                *size = remaining * share / shares;
            }
        }
    } else if fixed > total && fixed > 0.0 {
        // Over-specified with no relative track to absorb the excess: scale
        // everything down so the frameset still fits its window.
        let scale = total / fixed;
        for size in &mut sizes {
            *size *= scale;
        }
    }
    sizes
}

/// Converts row and column sizes into frame rectangles, in reading order.
pub fn cells(rows: &[f32], columns: &[f32]) -> Vec<(f32, f32, f32, f32)> {
    let mut out = Vec::new();
    let mut y = 0.0;
    for height in rows {
        let mut x = 0.0;
        for width in columns {
            out.push((x, y, *width, *height));
            x += width;
        }
        y += height;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_track_forms() {
        assert_eq!(
            parse_spec("100,25%,*"),
            vec![
                Track::Pixels(100.0),
                Track::Percent(25.0),
                Track::Relative(1.0)
            ]
        );
    }

    #[test]
    fn a_weighted_star_takes_a_larger_share() {
        assert_eq!(
            parse_spec("*,3*"),
            vec![Track::Relative(1.0), Track::Relative(3.0)]
        );
        let sizes = distribute(&parse_spec("*,3*"), 400.0);
        assert_eq!(sizes, vec![100.0, 300.0]);
    }

    #[test]
    fn an_empty_spec_is_one_full_size_track() {
        assert_eq!(parse_spec(""), vec![Track::Relative(1.0)]);
        assert_eq!(distribute(&parse_spec(""), 500.0), vec![500.0]);
    }

    #[test]
    fn relative_tracks_take_what_fixed_and_percentage_ones_leave() {
        let sizes = distribute(&parse_spec("100,25%,*"), 400.0);
        // 100 fixed, 100 for 25%, 200 left for the star.
        assert_eq!(sizes, vec![100.0, 100.0, 200.0]);
    }

    #[test]
    fn a_relative_track_collapses_when_nothing_is_left() {
        let sizes = distribute(&parse_spec("300,200,*"), 400.0);
        assert_eq!(sizes[2], 0.0, "no space left for the star");
    }

    #[test]
    fn over_specified_fixed_tracks_are_scaled_to_fit() {
        // Without a relative track to absorb it, an over-long spec would push
        // the frameset past its window; scaling keeps every frame visible.
        let sizes = distribute(&parse_spec("300,300"), 400.0);
        assert_eq!(sizes, vec![200.0, 200.0]);
        assert!((sizes.iter().sum::<f32>() - 400.0).abs() < 0.01);
    }

    #[test]
    fn trailing_junk_after_a_number_is_ignored() {
        // The era's markup is full of `rows="80px,*"`, which is not valid but
        // is obviously meant.
        assert_eq!(parse_spec("80px,*")[0], Track::Pixels(80.0));
    }

    #[test]
    fn cells_are_laid_out_in_reading_order() {
        let cells = cells(&[10.0, 20.0], &[30.0, 40.0]);
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[0], (0.0, 0.0, 30.0, 10.0));
        assert_eq!(cells[1], (30.0, 0.0, 40.0, 10.0));
        assert_eq!(cells[2], (0.0, 10.0, 30.0, 20.0));
        assert_eq!(cells[3], (30.0, 10.0, 40.0, 20.0));
    }
}
