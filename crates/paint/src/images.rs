//! Image decoding.
//!
//! Restricted to GIF, JPEG, and PNG — the era's formats, and the whole of what
//! ADR-0008 commits to. Decoding is `image`'s job rather than ours: it parses
//! attacker-controlled bytes, which is precisely the category ADR-0007 says not
//! to hand-roll.

use std::collections::HashMap;

use dom::NodeId;
use tiny_skia::{Pixmap, PremultipliedColorU8};

/// A decoded image, ready to paint.
#[derive(Debug, Clone)]
pub struct DecodedImage {
    /// Pixels, premultiplied, as tiny-skia wants them.
    pub pixmap: Pixmap,
}

impl DecodedImage {
    /// Intrinsic width in CSS pixels.
    pub fn width(&self) -> f32 {
        self.pixmap.width() as f32
    }

    /// Intrinsic height in CSS pixels.
    pub fn height(&self) -> f32 {
        self.pixmap.height() as f32
    }
}

/// Largest image we will decode, in pixels.
///
/// A decompression bomb is a small file that decodes to an enormous buffer, so
/// the guard has to be on the decoded dimensions rather than the byte count.
const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// Decodes image bytes, or returns `None` if they are not a supported image.
///
/// Never panics on malformed input: a broken image is an ordinary thing to find
/// on the web, and it must degrade to "no image" rather than take the page down.
pub fn decode(bytes: &[u8]) -> Option<DecodedImage> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;

    let (width, height) = reader.into_dimensions().ok()?;
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }

    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let decoded = reader.decode().ok()?.into_rgba8();

    let mut pixmap = Pixmap::new(width.max(1), height.max(1))?;
    for (source, target) in decoded.pixels().zip(pixmap.pixels_mut()) {
        let [r, g, b, a] = source.0;
        // tiny-skia composites premultiplied; `image` hands back straight alpha.
        let scale = |channel: u8| ((u32::from(channel) * u32::from(a)) / 255) as u8;
        *target =
            PremultipliedColorU8::from_rgba(scale(r), scale(g), scale(b), a).unwrap_or_else(|| {
                PremultipliedColorU8::from_rgba(0, 0, 0, 0).expect("transparent is valid")
            });
    }
    Some(DecodedImage { pixmap })
}

/// Which of an element's two possible images this is.
///
/// One element can have both: an `<img>` with a background behind it, or more
/// commonly a `<td>` whose content and tile are separate images. Keying on the
/// node alone would let one silently replace the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageSlot {
    /// The element's own content, as for `<img src>`.
    Content,
    /// The element's `background-image`.
    Background,
}

/// Identifies one decoded image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageKey {
    /// The element that referenced it.
    pub node: NodeId,
    /// Which of that element's images it is.
    pub slot: ImageSlot,
}

impl ImageKey {
    /// The element's content image.
    pub fn content(node: NodeId) -> Self {
        Self {
            node,
            slot: ImageSlot::Content,
        }
    }

    /// The element's background image.
    pub fn background(node: NodeId) -> Self {
        Self {
            node,
            slot: ImageSlot::Background,
        }
    }
}

/// Decoded images, keyed by the element that referenced them and the slot.
pub type ImageStore = HashMap<ImageKey, DecodedImage>;

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x2 PNG: red, green, blue, transparent.
    fn sample_png() -> Vec<u8> {
        let mut buffer = Vec::new();
        let mut image = image::RgbaImage::new(2, 2);
        image.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        image.put_pixel(1, 0, image::Rgba([0, 255, 0, 255]));
        image.put_pixel(0, 1, image::Rgba([0, 0, 255, 255]));
        image.put_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
        image::DynamicImage::ImageRgba8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut buffer),
                image::ImageFormat::Png,
            )
            .expect("encode");
        buffer
    }

    #[test]
    fn decodes_a_png_with_its_intrinsic_size() {
        let decoded = decode(&sample_png()).expect("decodes");
        assert_eq!((decoded.width(), decoded.height()), (2.0, 2.0));
    }

    #[test]
    fn colours_and_alpha_survive_decoding() {
        let decoded = decode(&sample_png()).expect("decodes");
        let pixels = decoded.pixmap.pixels();
        assert_eq!(
            (pixels[0].red(), pixels[0].green()),
            (255, 0),
            "top-left is red"
        );
        assert_eq!(pixels[1].green(), 255, "top-right is green");
        assert_eq!(pixels[2].blue(), 255, "bottom-left is blue");
        assert_eq!(pixels[3].alpha(), 0, "bottom-right is transparent");
    }

    #[test]
    fn malformed_bytes_decode_to_nothing_rather_than_panicking() {
        // Broken images are ordinary on the web; they must not take a page down.
        assert!(decode(b"not an image at all").is_none());
        assert!(decode(&[]).is_none());
        // A valid PNG header followed by rubbish.
        let mut truncated = sample_png();
        truncated.truncate(20);
        assert!(decode(&truncated).is_none());
    }
}
