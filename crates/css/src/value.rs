//! Parsed value types, and the component-value reader everything else uses.

use cssparser::{Parser, Token};

/// An owned component value.
///
/// `cssparser` hands out borrowed tokens tied to the input buffer. Declarations
/// outlive their parse, so component values are copied into this form once and
/// interpreted afterwards, per property.
#[derive(Debug, Clone, PartialEq)]
pub enum Raw {
    /// A bare identifier, e.g. `block`.
    Ident(String),
    /// A quoted string, e.g. `"Helvetica"`.
    Str(String),
    /// A unitless number.
    Number(f32),
    /// A number with a unit, e.g. `12px`.
    Dimension {
        /// Numeric part.
        value: f32,
        /// Lowercased unit.
        unit: String,
    },
    /// A percentage, stored as the literal number (`50%` is `50.0`).
    Percentage(f32),
    /// A `#rrggbb`-style token, without the `#`.
    Hash(String),
    /// A function call and its arguments, e.g. `rgb(1, 2, 3)`.
    Function(String, Vec<Raw>),
    /// A `url(...)` token, with the quotes and parentheses removed.
    Url(String),
    /// A comma separator, kept because some properties are comma-delimited.
    Comma,
    /// Any token we do not model. Its presence usually invalidates a value.
    Other,
}

/// Reads every component value from `input` until it is exhausted.
pub fn read_components(input: &mut Parser<'_, '_>) -> Vec<Raw> {
    let mut out = Vec::new();
    loop {
        // Skip whitespace and comments; a value's structure is carried by the
        // significant tokens, and callers would only have to filter these out.
        let token = match input.next_including_whitespace_and_comments() {
            Ok(token) => token.clone(),
            Err(_) => return out,
        };
        let raw = match token {
            Token::WhiteSpace(_) | Token::Comment(_) => continue,
            Token::Ident(name) => Raw::Ident(name.as_ref().to_ascii_lowercase()),
            Token::QuotedString(s) => Raw::Str(s.as_ref().to_owned()),
            Token::Number { value, .. } => Raw::Number(value),
            // `int_value` where the source wrote a whole number, because
            // `unit_value` is the fraction and multiplying it back up does not
            // land where it started: `30%` arrives as 0.3, and 0.3 × 100 is
            // 30.000002 in an `f32`. Harmless in most places and not in all —
            // table columns are sized in percentages, and a width that is a
            // fraction of a pixel wide of where the author put it rounds the
            // wrong way often enough to be visible.
            Token::Percentage {
                unit_value,
                int_value,
                ..
            } => Raw::Percentage(int_value.map_or(unit_value * 100.0, |whole| whole as f32)),
            Token::Dimension {
                value, ref unit, ..
            } => Raw::Dimension {
                value,
                unit: unit.as_ref().to_ascii_lowercase(),
            },
            Token::Hash(h) | Token::IDHash(h) => Raw::Hash(h.as_ref().to_owned()),
            Token::UnquotedUrl(url) => Raw::Url(url.as_ref().to_owned()),
            Token::Comma => Raw::Comma,
            Token::Function(name) => {
                let name = name.as_ref().to_ascii_lowercase();
                let args = input
                    .parse_nested_block(|inner| {
                        Ok::<_, cssparser::ParseError<'_, ()>>(read_components(inner))
                    })
                    .unwrap_or_default();
                Raw::Function(name, args)
            }
            _ => Raw::Other,
        };
        out.push(raw);
    }
}

/// An sRGB colour with straight alpha.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    /// Red, 0–255.
    pub r: u8,
    /// Green, 0–255.
    pub g: u8,
    /// Blue, 0–255.
    pub b: u8,
    /// Alpha, 0–255.
    pub a: u8,
}

impl Color {
    /// An opaque colour.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Fully transparent.
    pub const TRANSPARENT: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };
    /// Opaque black, the initial value of `color`.
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    /// Opaque white.
    pub const WHITE: Self = Self::rgb(255, 255, 255);

    /// Whether the colour would paint nothing.
    pub fn is_transparent(&self) -> bool {
        self.a == 0
    }

    /// This colour composited over an opaque `backdrop`.
    ///
    /// Source-over with a straight (non-premultiplied) source, which is how
    /// these are stored. Used where a colour must be flattened before it can be
    /// handed to something that only takes opaque values.
    pub fn over(self, backdrop: Color) -> Color {
        if self.a == 255 {
            return self;
        }
        let alpha = f32::from(self.a) / 255.0;
        let mix = |source: u8, under: u8| {
            (f32::from(source) * alpha + f32::from(under) * (1.0 - alpha)).round() as u8
        };
        Color {
            r: mix(self.r, backdrop.r),
            g: mix(self.g, backdrop.g),
            b: mix(self.b, backdrop.b),
            a: 255,
        }
    }
}

#[cfg(test)]
mod color_tests {
    use super::Color;

    #[test]
    fn compositing_over_a_backdrop() {
        let half_red = Color {
            r: 255,
            g: 0,
            b: 0,
            a: 128,
        };
        assert_eq!(half_red.over(Color::WHITE), Color::rgb(255, 127, 127));
        assert_eq!(
            Color::TRANSPARENT.over(Color::WHITE),
            Color::WHITE,
            "nothing over white is white"
        );
        assert_eq!(
            Color::rgb(1, 2, 3).over(Color::WHITE),
            Color::rgb(1, 2, 3),
            "an opaque colour is unchanged"
        );
    }
}

/// The seventeen named colours of CSS 2.1, plus `transparent`.
///
/// Deliberately not the extended 148-name list: ADR-0004 scopes this engine to
/// CSS 2.1, and the extended set arrives with the rest of CSS Color in M2.
fn named_color(name: &str) -> Option<Color> {
    let color = match name {
        "black" => Color::rgb(0, 0, 0),
        "silver" => Color::rgb(192, 192, 192),
        "gray" | "grey" => Color::rgb(128, 128, 128),
        "white" => Color::rgb(255, 255, 255),
        "maroon" => Color::rgb(128, 0, 0),
        "red" => Color::rgb(255, 0, 0),
        "purple" => Color::rgb(128, 0, 128),
        "fuchsia" | "magenta" => Color::rgb(255, 0, 255),
        "green" => Color::rgb(0, 128, 0),
        "lime" => Color::rgb(0, 255, 0),
        "olive" => Color::rgb(128, 128, 0),
        "yellow" => Color::rgb(255, 255, 0),
        "navy" => Color::rgb(0, 0, 128),
        "blue" => Color::rgb(0, 0, 255),
        "teal" => Color::rgb(0, 128, 128),
        "aqua" | "cyan" => Color::rgb(0, 255, 255),
        "orange" => Color::rgb(255, 165, 0),
        "transparent" => Color::TRANSPARENT,
        _ => return None,
    };
    Some(color)
}

fn hex_pair(bytes: &[u8]) -> Option<u8> {
    let hi = (bytes[0] as char).to_digit(16)?;
    let lo = (bytes[1] as char).to_digit(16)?;
    Some((hi * 16 + lo) as u8)
}

/// Parses a colour from a single component value.
pub fn parse_color(raw: &Raw) -> Option<Color> {
    match raw {
        Raw::Ident(name) => named_color(name),
        Raw::Hash(hex) => {
            let bytes = hex.as_bytes();
            match bytes.len() {
                // #rgb expands by doubling each digit, not by padding.
                3 => {
                    let expand = |c: u8| hex_pair(&[c, c]);
                    Some(Color::rgb(
                        expand(bytes[0])?,
                        expand(bytes[1])?,
                        expand(bytes[2])?,
                    ))
                }
                6 => Some(Color::rgb(
                    hex_pair(&bytes[0..2])?,
                    hex_pair(&bytes[2..4])?,
                    hex_pair(&bytes[4..6])?,
                )),
                _ => None,
            }
        }
        Raw::Function(name, args) if name == "rgb" || name == "rgba" => {
            let numbers: Vec<f32> = args
                .iter()
                .filter_map(|a| match a {
                    Raw::Number(n) => Some(*n),
                    // Percentages are legal in rgb(): 100% is 255.
                    Raw::Percentage(p) => Some(p / 100.0 * 255.0),
                    _ => None,
                })
                .collect();
            let channel = |v: f32| v.clamp(0.0, 255.0) as u8;
            match numbers.len() {
                3 => Some(Color::rgb(
                    channel(numbers[0]),
                    channel(numbers[1]),
                    channel(numbers[2]),
                )),
                4 => Some(Color {
                    r: channel(numbers[0]),
                    g: channel(numbers[1]),
                    b: channel(numbers[2]),
                    // Alpha is 0–1 here, unlike the colour channels.
                    a: (numbers[3].clamp(0.0, 1.0) * 255.0).round() as u8,
                }),
                _ => None,
            }
        }
        _ => None,
    }
}

/// A length that may still depend on the element's own font size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    /// An absolute length in CSS pixels.
    Px(f32),
    /// A multiple of the element's font size.
    Em(f32),
    /// A percentage of the containing block's width.
    Percent(f32),
    /// `auto`.
    Auto,
}

impl Length {
    /// Resolves to pixels. `auto` and percentages need context the caller has.
    pub fn to_px(self, font_size: f32, percent_basis: f32) -> f32 {
        match self {
            Length::Px(v) => v,
            Length::Em(v) => v * font_size,
            Length::Percent(v) => v / 100.0 * percent_basis,
            Length::Auto => 0.0,
        }
    }
}

/// Parses a length, accepting the quirks-mode forms as well.
///
/// In quirks mode a bare number is a pixel length. Pages of this era wrote
/// `width: 100` constantly, and in standards mode that declaration is simply
/// invalid — so honouring it is the difference between a laid-out page and a
/// collapsed one.
pub fn parse_length_quirky(raw: &Raw, quirks: bool) -> Option<Length> {
    if quirks && let Raw::Number(value) = raw {
        return Some(Length::Px(*value));
    }
    parse_length(raw)
}

/// Parses a colour, accepting the quirks-mode forms as well.
///
/// In quirks mode a hash-less hex colour is accepted: `color: ffffff` and
/// `bgcolor`-style values were widespread before authors settled on `#`.
pub fn parse_color_quirky(raw: &Raw, quirks: bool) -> Option<Color> {
    if let Some(color) = parse_color(raw) {
        return Some(color);
    }
    if !quirks {
        return None;
    }
    match raw {
        // An identifier that is entirely hex digits, e.g. `ffffff` or `fff`.
        Raw::Ident(name) => hex_string(name),
        // `00ff00` tokenises as a dimension: the number `00` with unit `ff00`.
        // The parsed number has lost its leading zeros, so they are restored by
        // padding to whichever total length — six or three — fits the unit.
        Raw::Dimension { value, unit } => {
            let number = format!("{}", *value as i64);
            [6usize, 3]
                .into_iter()
                .filter_map(|total| {
                    let digits = total.checked_sub(unit.len())?;
                    (digits >= number.len()).then(|| format!("{number:0>digits$}{unit}"))
                })
                .find_map(|candidate| hex_string(&candidate))
        }
        Raw::Number(value) => {
            let number = format!("{}", *value as i64);
            [6usize, 3]
                .into_iter()
                .filter(|total| *total >= number.len())
                .find_map(|total| hex_string(&format!("{number:0>total$}")))
        }
        _ => None,
    }
}

/// Parses a bare hex colour string of three or six digits.
fn hex_string(text: &str) -> Option<Color> {
    let text = text.trim();
    if !text.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match text.len() {
        3 | 6 => parse_color(&Raw::Hash(text.to_owned())),
        _ => None,
    }
}

/// Parses a length from a single component value.
pub fn parse_length(raw: &Raw) -> Option<Length> {
    match raw {
        Raw::Ident(name) if name == "auto" => Some(Length::Auto),
        Raw::Percentage(p) => Some(Length::Percent(*p)),
        // A bare 0 is a valid length; any other unitless number is not.
        Raw::Number(n) if *n == 0.0 => Some(Length::Px(0.0)),
        Raw::Dimension { value, unit } => match unit.as_str() {
            "px" => Some(Length::Px(*value)),
            "em" => Some(Length::Em(*value)),
            // Absolute units, converted at the CSS-standard 96dpi.
            "pt" => Some(Length::Px(value * 96.0 / 72.0)),
            "pc" => Some(Length::Px(value * 16.0)),
            "in" => Some(Length::Px(value * 96.0)),
            "cm" => Some(Length::Px(value * 96.0 / 2.54)),
            "mm" => Some(Length::Px(value * 96.0 / 25.4)),
            _ => None,
        },
        _ => None,
    }
}
