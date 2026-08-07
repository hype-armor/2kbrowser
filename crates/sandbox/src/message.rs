//! What the two processes say to each other.
//!
//! Deliberately small. The renderer is a pure function of bytes and a viewport
//! (ADR-0012), so the conversation is short: the parent asks for a page, the
//! child asks for whatever subresources the page turns out to reference, and
//! the child hands back pixels and the geometry needed to click on them.
//!
//! Everything the parent needs *after* rendering has to be in [`Rendered`],
//! because the document and the box tree stay on the far side of the boundary
//! and are never sent. That is the point: the parent should not be parsing
//! anything a stranger wrote.

use layout::Rect;
use net::{Origin, RequestKind, Scheme};

use crate::wire::{Reader, WireError, Writer};

/// How a page was rendered, as it crosses the boundary.
///
/// Mirrors `layout::RenderMode` rather than being it: this crate sits below the
/// one that owns that type's meaning, and a wire format that changed shape when
/// an unrelated enum gained a variant would be a trap.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    /// The author's layout.
    Authored,
    /// The document fallback, with the share of the page that needed layout we
    /// do not implement.
    Document {
        /// Fraction of the page that could not be laid out as authored.
        unsupported_share: f32,
    },
    /// The document fallback, because the page needs scripting.
    RequiresScripting,
}

impl Mode {
    fn write(&self, writer: &mut Writer) {
        match self {
            Mode::Authored => writer.tag(0),
            Mode::Document { unsupported_share } => {
                writer.tag(1);
                writer.f32(*unsupported_share);
            }
            Mode::RequiresScripting => writer.tag(2),
        }
    }

    fn read(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        match reader.tag()? {
            0 => Ok(Mode::Authored),
            1 => Ok(Mode::Document {
                unsupported_share: reader.f32()?,
            }),
            2 => Ok(Mode::RequiresScripting),
            _ => Err(WireError::Unknown),
        }
    }
}

/// A link's rectangle and where it leads.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    /// Where it is, in canvas coordinates.
    pub rect: Rect,
    /// The absolute URL it leads to, already resolved by the child.
    pub url: String,
}

fn write_rect(writer: &mut Writer, rect: &Rect) {
    writer.f32(rect.x);
    writer.f32(rect.y);
    writer.f32(rect.width);
    writer.f32(rect.height);
}

fn read_rect(reader: &mut Reader<'_>) -> Result<Rect, WireError> {
    Ok(Rect {
        x: reader.f32()?,
        y: reader.f32()?,
        width: reader.f32()?,
        height: reader.f32()?,
    })
}

fn write_origin(writer: &mut Writer, origin: &Origin) {
    writer.tag(match origin.scheme {
        Scheme::Http => 0,
        Scheme::Https => 1,
        Scheme::File => 2,
    });
    writer.str(&origin.host);
    writer.u16(origin.port);
}

fn read_origin(reader: &mut Reader<'_>) -> Result<Origin, WireError> {
    let scheme = match reader.tag()? {
        0 => Scheme::Http,
        1 => Scheme::Https,
        2 => Scheme::File,
        _ => return Err(WireError::Unknown),
    };
    Ok(Origin {
        scheme,
        host: reader.str()?,
        port: reader.u16()?,
    })
}

/// Parent to child.
#[derive(Debug, Clone, PartialEq)]
pub enum ToChild {
    /// Render this document.
    Render {
        /// The document's bytes, undecoded — the child does the decoding, so
        /// the encoding sniffer stays on the sandboxed side with every other
        /// parser.
        body: Vec<u8>,
        /// `Content-Type`, when the transport supplied one.
        content_type: Option<String>,
        /// Viewport width.
        width: u32,
        /// Maximum canvas height.
        max_height: u32,
        /// The document's own origin, for resolving what it references.
        origin: Option<Origin>,
        /// The document's path within that origin.
        path: String,
        /// Whether to overrule the document fallback (ADR-0009).
        force_authored: bool,
    },
    /// The answer to a [`ToParent::Fetch`].
    ///
    /// A refusal and a failure are the same shape on purpose: the child has no
    /// business knowing whether a resource was blocked by policy or merely
    /// missing, and telling it would leak the parent's configuration to the
    /// untrusted side.
    Resource {
        /// The bytes, or empty when it could not be had.
        body: Vec<u8>,
        /// The `Content-Type` it was served with, when there was one.
        ///
        /// Carried because a stylesheet's character set can come from the
        /// header, and dropping it here would silently change how a legacy
        /// stylesheet decodes on the far side.
        content_type: Option<String>,
        /// Whether it was retrieved at all.
        ok: bool,
    },
    /// Asks where `query` appears on the page most recently rendered.
    ///
    /// Find has to be a *question asked of a live child* rather than something
    /// the parent works out. The text and the box tree it searches never cross
    /// the boundary — that restraint is the point of ADR-0012 — so the only
    /// thing that can answer is the process holding them.
    Find {
        /// What to look for.
        query: String,
    },
}

impl ToChild {
    /// Encodes to a frame.
    pub fn encode(&self) -> Vec<u8> {
        let mut writer = Writer::new();
        match self {
            ToChild::Render {
                body,
                content_type,
                width,
                max_height,
                origin,
                path,
                force_authored,
            } => {
                writer.tag(0);
                writer.bytes(body);
                writer.some(content_type.is_some());
                if let Some(content_type) = content_type {
                    writer.str(content_type);
                }
                writer.u32(*width);
                writer.u32(*max_height);
                writer.some(origin.is_some());
                if let Some(origin) = origin {
                    write_origin(&mut writer, origin);
                }
                writer.str(path);
                writer.some(*force_authored);
            }
            ToChild::Find { query } => {
                writer.tag(2);
                writer.str(query);
            }
            ToChild::Resource {
                body,
                content_type,
                ok,
            } => {
                writer.tag(1);
                writer.bytes(body);
                writer.some(content_type.is_some());
                if let Some(content_type) = content_type {
                    writer.str(content_type);
                }
                writer.some(*ok);
            }
        }
        writer.finish()
    }

    /// Decodes a frame.
    pub fn decode(frame: &[u8]) -> Result<Self, WireError> {
        let mut reader = Reader::new(frame);
        let message = match reader.tag()? {
            0 => {
                let body = reader.bytes()?.to_vec();
                let content_type = if reader.some()? {
                    Some(reader.str()?)
                } else {
                    None
                };
                let width = reader.u32()?;
                let max_height = reader.u32()?;
                let origin = if reader.some()? {
                    Some(read_origin(&mut reader)?)
                } else {
                    None
                };
                ToChild::Render {
                    body,
                    content_type,
                    width,
                    max_height,
                    origin,
                    path: reader.str()?,
                    force_authored: reader.some()?,
                }
            }
            1 => {
                let body = reader.bytes()?.to_vec();
                let content_type = if reader.some()? {
                    Some(reader.str()?)
                } else {
                    None
                };
                ToChild::Resource {
                    body,
                    content_type,
                    ok: reader.some()?,
                }
            }
            2 => ToChild::Find {
                query: reader.str()?,
            },
            _ => return Err(WireError::Unknown),
        };
        reader.finish()?;
        Ok(message)
    }
}

/// Child to parent.
#[derive(Debug, Clone, PartialEq)]
pub enum ToParent {
    /// Asks for a subresource.
    ///
    /// The child cannot reach the network itself, so this is the only way it
    /// gets anything — and the parent applies ADR-0006's policy to every one of
    /// them, somewhere a compromised renderer cannot reach.
    Fetch {
        /// Absolute URL, resolved by the child against the document.
        url: String,
        /// What it is for, so the policy can tell a navigation from a
        /// subresource.
        kind: RequestKind,
    },
    /// The finished page.
    Rendered(Box<Rendered>),
    /// Rendering failed.
    Failed {
        /// What went wrong, for the chrome to show.
        message: String,
    },
    /// Where a [`ToChild::Find`] query appears, in canvas coordinates.
    Matches {
        /// One rectangle per match, in document order.
        rects: Vec<Rect>,
    },
}

/// A rendered page, as it crosses the boundary.
///
/// The pixels plus exactly the geometry the parent needs to be a viewport onto
/// them. Notably absent: the document and the box tree, which stay on the far
/// side and are what the parent must never parse.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    /// Premultiplied RGBA, `width * height * 4` bytes.
    pub pixels: Vec<u8>,
    /// Canvas width.
    pub width: u32,
    /// Canvas height.
    pub height: u32,
    /// Height of the content, which may exceed the canvas.
    pub content_height: f32,
    /// How it was rendered (ADR-0009).
    pub mode: Mode,
    /// The page's `<title>`, when it had one.
    pub title: Option<String>,
    /// Every link, with its rectangles already resolved to absolute URLs.
    pub links: Vec<Link>,
    /// Whether there is a fallback decision to overrule.
    pub can_toggle_layout: bool,
}

impl ToParent {
    /// Encodes to a frame.
    pub fn encode(&self) -> Vec<u8> {
        let mut writer = Writer::new();
        match self {
            ToParent::Fetch { url, kind } => {
                writer.tag(0);
                writer.str(url);
                writer.tag(match kind {
                    RequestKind::Navigation => 0,
                    RequestKind::Subresource => 1,
                });
            }
            ToParent::Rendered(page) => {
                writer.tag(1);
                writer.bytes(&page.pixels);
                writer.u32(page.width);
                writer.u32(page.height);
                writer.f32(page.content_height);
                page.mode.write(&mut writer);
                writer.some(page.title.is_some());
                if let Some(title) = &page.title {
                    writer.str(title);
                }
                writer.u32(page.links.len() as u32);
                for link in &page.links {
                    write_rect(&mut writer, &link.rect);
                    writer.str(&link.url);
                }
                writer.some(page.can_toggle_layout);
            }
            ToParent::Failed { message } => {
                writer.tag(2);
                writer.str(message);
            }
            ToParent::Matches { rects } => {
                writer.tag(3);
                writer.u32(rects.len() as u32);
                for rect in rects {
                    write_rect(&mut writer, rect);
                }
            }
        }
        writer.finish()
    }

    /// Decodes a frame.
    pub fn decode(frame: &[u8]) -> Result<Self, WireError> {
        let mut reader = Reader::new(frame);
        let message = match reader.tag()? {
            0 => ToParent::Fetch {
                url: reader.str()?,
                kind: match reader.tag()? {
                    0 => RequestKind::Navigation,
                    1 => RequestKind::Subresource,
                    _ => return Err(WireError::Unknown),
                },
            },
            1 => {
                let pixels = reader.bytes()?.to_vec();
                let width = reader.u32()?;
                let height = reader.u32()?;
                let content_height = reader.f32()?;
                let mode = Mode::read(&mut reader)?;
                let title = if reader.some()? {
                    Some(reader.str()?)
                } else {
                    None
                };
                // A count, not a plain `u32`: bounded by the bytes left, so a
                // claim of four billion links cannot reserve for four billion
                // links.
                let count = reader.count()?;
                let mut links = Vec::with_capacity(count.min(1024));
                for _ in 0..count {
                    links.push(Link {
                        rect: read_rect(&mut reader)?,
                        url: reader.str()?,
                    });
                }
                let can_toggle_layout = reader.some()?;
                // The pixel buffer has to match the dimensions it is labelled
                // with, or every reader of it indexes out of bounds. Checked
                // here rather than trusted, because the sender is the
                // untrusted side.
                let expected = u64::from(width)
                    .checked_mul(u64::from(height))
                    .and_then(|pixels| pixels.checked_mul(4))
                    .ok_or(WireError::BadLength)?;
                if pixels.len() as u64 != expected {
                    return Err(WireError::BadLength);
                }
                ToParent::Rendered(Box::new(Rendered {
                    pixels,
                    width,
                    height,
                    content_height,
                    mode,
                    title,
                    links,
                    can_toggle_layout,
                }))
            }
            2 => ToParent::Failed {
                message: reader.str()?,
            },
            3 => {
                // A count, so a claim of four billion matches cannot reserve
                // for four billion matches.
                let count = reader.count()?;
                let mut rects = Vec::with_capacity(count.min(4096));
                for _ in 0..count {
                    rects.push(read_rect(&mut reader)?);
                }
                ToParent::Matches { rects }
            }
            _ => return Err(WireError::Unknown),
        };
        reader.finish()?;
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(width: u32, height: u32) -> Rendered {
        Rendered {
            pixels: vec![0; (width * height * 4) as usize],
            width,
            height,
            content_height: 123.5,
            mode: Mode::Document {
                unsupported_share: 0.42,
            },
            title: Some("A Page".to_owned()),
            links: vec![Link {
                rect: Rect {
                    x: 1.0,
                    y: 2.0,
                    width: 3.0,
                    height: 4.0,
                },
                url: "https://example.com/".to_owned(),
            }],
            can_toggle_layout: true,
        }
    }

    #[test]
    fn a_render_request_round_trips() {
        let message = ToChild::Render {
            body: b"<p>hello</p>".to_vec(),
            content_type: Some("text/html; charset=utf-8".to_owned()),
            width: 800,
            max_height: 2000,
            origin: Some(
                net::parse_url("https://example.com/a.html")
                    .expect("parses")
                    .0,
            ),
            path: "/a.html".to_owned(),
            force_authored: true,
        };
        assert_eq!(ToChild::decode(&message.encode()), Ok(message));
    }

    #[test]
    fn a_render_request_without_a_base_round_trips() {
        let message = ToChild::Render {
            body: Vec::new(),
            content_type: None,
            width: 1,
            max_height: 1,
            origin: None,
            path: String::new(),
            force_authored: false,
        };
        assert_eq!(ToChild::decode(&message.encode()), Ok(message));
    }

    #[test]
    fn every_scheme_survives_the_trip() {
        for url in [
            "https://example.com:8443/a",
            "http://example.org/b",
            "file:///tmp/c.html",
        ] {
            let (origin, path) = net::parse_url(url).expect("parses");
            let message = ToChild::Render {
                body: Vec::new(),
                content_type: None,
                width: 10,
                max_height: 10,
                origin: Some(origin),
                path,
                force_authored: false,
            };
            assert_eq!(ToChild::decode(&message.encode()), Ok(message), "{url}");
        }
    }

    #[test]
    fn a_rendered_page_round_trips() {
        let message = ToParent::Rendered(Box::new(rendered(4, 3)));
        assert_eq!(ToParent::decode(&message.encode()), Ok(message));
    }

    #[test]
    fn a_fetch_and_a_failure_round_trip() {
        for message in [
            ToParent::Fetch {
                url: "https://example.com/x.png".to_owned(),
                kind: RequestKind::Subresource,
            },
            ToParent::Fetch {
                url: "https://example.com/".to_owned(),
                kind: RequestKind::Navigation,
            },
            ToParent::Failed {
                message: "could not render".to_owned(),
            },
        ] {
            assert_eq!(ToParent::decode(&message.encode()), Ok(message));
        }
    }

    #[test]
    fn a_pixel_buffer_must_match_the_size_it_claims() {
        // The sender is the untrusted side. A buffer shorter than its
        // dimensions would have every later reader indexing past the end.
        let mut page = rendered(4, 3);
        page.pixels.truncate(4);
        let frame = ToParent::Rendered(Box::new(page)).encode();
        assert_eq!(ToParent::decode(&frame), Err(WireError::BadLength));
    }

    #[test]
    fn dimensions_that_would_overflow_are_refused() {
        // `width * height * 4` in `u32` wraps, and a wrapped product can be
        // made to match a short buffer exactly.
        let mut page = rendered(1, 1);
        page.width = u32::MAX;
        page.height = u32::MAX;
        let frame = ToParent::Rendered(Box::new(page)).encode();
        assert_eq!(ToParent::decode(&frame), Err(WireError::BadLength));
    }

    #[test]
    fn an_unknown_tag_is_refused_rather_than_ignored() {
        assert_eq!(ToChild::decode(&[9]), Err(WireError::Unknown));
        assert_eq!(ToParent::decode(&[9]), Err(WireError::Unknown));
        assert_eq!(ToParent::decode(&[]), Err(WireError::Truncated));
    }

    #[test]
    fn a_truncated_message_never_panics() {
        // Every prefix of every message. The parent decoding a frame from a
        // compromised child is the last boundary there is.
        let frames = [
            ToChild::Render {
                body: b"<p>x</p>".to_vec(),
                content_type: Some("text/html".to_owned()),
                width: 800,
                max_height: 600,
                origin: Some(net::parse_url("https://example.com/").expect("parses").0),
                path: "/".to_owned(),
                force_authored: false,
            }
            .encode(),
            ToParent::Rendered(Box::new(rendered(3, 2))).encode(),
            ToParent::Fetch {
                url: "https://example.com/a".to_owned(),
                kind: RequestKind::Subresource,
            }
            .encode(),
        ];
        for frame in frames {
            for cut in 0..frame.len() {
                let _ = ToChild::decode(&frame[..cut]);
                let _ = ToParent::decode(&frame[..cut]);
            }
        }
    }

    #[test]
    fn a_link_count_larger_than_the_frame_is_refused() {
        // Hand-built: claim a billion links in a frame with room for none.
        let mut writer = Writer::new();
        writer.tag(1);
        writer.bytes(&[]);
        writer.u32(0);
        writer.u32(0);
        writer.f32(0.0);
        writer.tag(0);
        writer.some(false);
        writer.u32(1_000_000_000);
        assert_eq!(
            ToParent::decode(&writer.finish()),
            Err(WireError::BadLength)
        );
    }
}
