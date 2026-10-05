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
//!
//! One message is no longer that shape, and it is worth saying so here rather
//! than letting the sentence above quietly stop being true. ADR-0019 decided
//! that the accessibility tree crosses as data, because the alternative is
//! handing platform API handles to the sandboxed process. It is still not the
//! document and still not the box tree — it is a flat list of roles, labels and
//! rectangles, derived from both — but it is an arbitrary-depth structure with
//! attacker-chosen text in it, which nothing else here is. [`crate::access`]
//! holds it, and holds the four bounds that make it affordable.

use layout::Rect;
use net::{Origin, RequestKind, Scheme};

use crate::access::Tree;
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
    /// The document fallback, because the page's *frame* needs layout we do
    /// not implement even though its text does not.
    DocumentFrame {
        /// How many containers would have laid their children out in a row.
        containers: u32,
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
            Mode::DocumentFrame { containers } => {
                writer.tag(3);
                writer.u32(*containers);
            }
        }
    }

    fn read(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        match reader.tag()? {
            0 => Ok(Mode::Authored),
            1 => Ok(Mode::Document {
                unsupported_share: reader.f32()?,
            }),
            2 => Ok(Mode::RequiresScripting),
            3 => Ok(Mode::DocumentFrame {
                containers: reader.u32()?,
            }),
            _ => Err(WireError::Unknown),
        }
    }
}

/// A box where an image was going to be, and did not arrive (#118).
///
/// The reader presses it and the parent decides what that means — retry, or,
/// if the parent's own policy is what refused it, offer the exception. The
/// child neither knows nor is told which; it reports what the page asked for
/// and stops there.
#[derive(Debug, Clone, PartialEq)]
pub struct Missing {
    /// Where the placeholder is, in canvas coordinates.
    pub rect: Rect,
    /// The absolute URL the page asked for, already resolved by the child.
    pub url: String,
}

/// A picture on the page, and the address it was fetched from (#205).
///
/// Travels outward with the page for the same reason the links do: the parent
/// has no box tree — it is on the other side of the boundary — so a picture
/// missing from this list is a picture the right-hand button has nothing to say
/// about. The URL is already resolved, because resolving it needs the document
/// that named it.
#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    /// Where it is, in canvas coordinates.
    pub rect: Rect,
    /// The absolute URL it came from.
    pub url: String,
}

/// A form the reader asked to send (#110).
///
/// Assembled by the child, because the form is part of the document and the
/// document never leaves that side (ADR-0012). What crosses is only this: a
/// destination as the markup wrote it, a method, and the encoded pairs.
///
/// The parent decides everything that follows — where the destination resolves
/// to, whether the policy allows it, how big a body may be, and whether to
/// navigate at all. That split is the point: a compromised renderer can ask for
/// a request, exactly as it can already ask for a navigation by claiming a link
/// is under the pointer, and it cannot make one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    /// The form's `action`, as written. Empty means the document's own URL.
    pub action: String,
    /// Whether the pairs go in the query string or in a body.
    pub post: bool,
    /// The successful controls, `application/x-www-form-urlencoded`.
    pub body: String,
}

/// What has the keyboard on the page, in as little detail as the parent needs.
///
/// This was one bit — "a control is taking the typing" — which was all the
/// parent needed while only text fields could be focused. It cannot stay one
/// bit now that a checkbox can be (#151): a focused checkbox takes keystrokes
/// while having nothing to type, and Up and Down mean something to a focused
/// `<select>` and nothing to a field, where they still scroll the page.
///
/// Three states and no more. *Which* control it is, what is in it and what it
/// would send stay on the side that holds the document — what a reader is
/// answering is the page's business (ADR-0012).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focused {
    /// Nothing on the page. Every key is the window's.
    Nothing,
    /// A control being typed in, so a character is a character.
    Typing,
    /// A control that is pressed rather than typed in, so a space presses it
    /// and the arrows move through what it offers.
    Pressable,
}

/// A dropdown the reader has opened, and what is in it.
///
/// A closed `<select>` is a list nobody can see until it is opened, and there
/// is nowhere on the page to open it — the list has to float over whatever is
/// below. So the child says what the list holds and where the box is, and the
/// parent draws it in the chrome's buffer beside the menu and the site panel,
/// which is where everything that floats over a page already lives.
///
/// `node` is the child's own id for the `<select>`, echoed back untouched when
/// a row is chosen. The parent does not read it and could not use it: it is an
/// index into an arena on the other side of the boundary, and the child checks
/// what it gets back rather than trusting it (ADR-0012).
#[derive(Debug, Clone, PartialEq)]
pub struct Dropdown {
    /// The `<select>`'s box, in canvas coordinates, so the list opens under it.
    pub rect: Rect,
    /// The child's id for the `<select>`.
    pub node: u32,
    /// What each option reads as, in document order.
    pub options: Vec<String>,
    /// Which one it is currently open on.
    pub on: u32,
}

/// A link's rectangle and where it leads.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    /// Where it is, in canvas coordinates.
    pub rect: Rect,
    /// The absolute URL it leads to, already resolved by the child.
    pub url: String,
    /// Which link this rectangle belongs to.
    ///
    /// A link that wraps across a line break is several rectangles and one
    /// destination, and keyboard focus moves link by link — so the grouping has
    /// to cross the boundary rather than be guessed at afterwards. Guessing by
    /// URL would be wrong: two different links on a page may lead to the same
    /// place.
    pub group: u32,
    /// Where on this page it goes, for a link that does not leave it.
    ///
    /// A fragment link is a place on the page already open rather than a page
    /// to fetch, and the answer is in the box tree — which lives on this side
    /// of the boundary. Sent with the link so that following one costs no
    /// round trip.
    pub jump_to: Option<f32>,
    /// Whether the link sits in a `position: fixed` subtree, so its rectangle
    /// is in *window* coordinates rather than document ones (#108).
    ///
    /// The parent turns a click into a document point by adding the scroll.
    /// For a pinned link that is exactly wrong: the box stayed where it was,
    /// so adding the scroll misses it by however far the page has moved.
    pub pinned: bool,
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

/// One subresource the parent supplied, inside a [`ToChild::Resources`].
///
/// A refusal and a failure are the same shape on purpose: the child has no
/// business knowing whether a resource was blocked by policy or merely
/// missing, and telling it would leak the parent's configuration to the
/// untrusted side.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Supplied {
    /// The bytes, or empty when it could not be had.
    pub body: Vec<u8>,
    /// The `Content-Type` it was served with, when there was one.
    ///
    /// Carried because a stylesheet's character set can come from the header,
    /// and dropping it here would silently change how a legacy stylesheet
    /// decodes on the far side.
    pub content_type: Option<String>,
    /// Whether it was retrieved at all.
    pub ok: bool,
}

/// A keystroke aimed at a form control on the page (#110).
///
/// Named rather than raw: the parent turns winit's key events into these, so
/// the child never sees a keyboard. What crosses the boundary is "the reader
/// asked to delete a word", not a scancode and a modifier mask — which keeps
/// the untrusted side from having to interpret anything, and keeps every
/// platform's idea of a key on the platform's own side of the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// Text to put in at the cursor, replacing the selection.
    Insert(String),
    /// Delete backwards.
    Backspace,
    /// Delete forwards.
    Delete,
    /// Move left, by a word if asked, extending the selection if asked.
    Left {
        /// Extend the selection rather than collapsing it.
        extend: bool,
        /// Move a word rather than a character.
        word: bool,
    },
    /// Move right, by the same rules.
    Right {
        /// Extend the selection rather than collapsing it.
        extend: bool,
        /// Move a word rather than a character.
        word: bool,
    },
    /// To the front of the line.
    Home {
        /// Extend the selection rather than collapsing it.
        extend: bool,
    },
    /// To the end of the line.
    End {
        /// Extend the selection rather than collapsing it.
        extend: bool,
    },
    /// Select the whole field.
    SelectAll,
    /// Move to the next control, or the previous one.
    Tab {
        /// Backwards.
        back: bool,
    },
    /// Give up the focus.
    Escape,
    /// Move up: through a `<select>`'s options, without opening its list.
    ///
    /// Only sent when the page has a control focused that is pressed rather
    /// than typed in, because otherwise the arrows are the window's and the
    /// page scrolls under the caret (#151).
    Up,
    /// Move down, by the same rule.
    Down,
}

impl Key {
    fn write(&self, writer: &mut Writer) {
        match self {
            Key::Insert(text) => {
                writer.tag(0);
                writer.str(text);
            }
            Key::Backspace => writer.tag(1),
            Key::Delete => writer.tag(2),
            Key::Left { extend, word } => {
                writer.tag(3);
                writer.some(*extend);
                writer.some(*word);
            }
            Key::Right { extend, word } => {
                writer.tag(4);
                writer.some(*extend);
                writer.some(*word);
            }
            Key::Home { extend } => {
                writer.tag(5);
                writer.some(*extend);
            }
            Key::End { extend } => {
                writer.tag(6);
                writer.some(*extend);
            }
            Key::SelectAll => writer.tag(7),
            Key::Tab { back } => {
                writer.tag(8);
                writer.some(*back);
            }
            Key::Escape => writer.tag(9),
            Key::Up => writer.tag(10),
            Key::Down => writer.tag(11),
        }
    }

    fn read(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok(match reader.tag()? {
            0 => Key::Insert(reader.str()?),
            1 => Key::Backspace,
            2 => Key::Delete,
            3 => Key::Left {
                extend: reader.some()?,
                word: reader.some()?,
            },
            4 => Key::Right {
                extend: reader.some()?,
                word: reader.some()?,
            },
            5 => Key::Home {
                extend: reader.some()?,
            },
            6 => Key::End {
                extend: reader.some()?,
            },
            7 => Key::SelectAll,
            8 => Key::Tab {
                back: reader.some()?,
            },
            9 => Key::Escape,
            10 => Key::Up,
            11 => Key::Down,
            _ => return Err(WireError::Unknown),
        })
    }
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
        /// First document row to paint.
        top: u32,
        /// How many rows to paint, clipped to what the document has below
        /// `top`. A page shorter than this gets a canvas its own height, which
        /// is what every page got before bands existed.
        height: u32,
        /// The document's own origin, for resolving what it references.
        origin: Option<Origin>,
        /// The document's path within that origin.
        path: String,
        /// Whether to overrule the document fallback (ADR-0009).
        force_authored: bool,
        /// Whether to ask for the document fallback on a page that classified
        /// as authored (ADR-0009).
        ///
        /// Not the absence of `force_authored`, which is why it is a second
        /// field rather than the other value of one: a page with no fallback
        /// has nothing to return to, so wanting one is its own request. The two
        /// are never both true, but that is the parent's business — the child
        /// reads whatever arrives, and `render` gives `force_authored`
        /// precedence rather than trusting a stranger to have kept the rule.
        force_document: bool,
        /// How much bigger than its own pixels to draw the page. 1.0 is the
        /// page as written.
        zoom: f32,
    },
    /// Which of the URLs just asked about have been followed (#181).
    ///
    /// One flag per URL, matched by position, exactly as [`ToChild::Resources`]
    /// answers a fetch. A reply of the wrong length is a parent that is not
    /// what we think it is, and the child then treats every link as unvisited
    /// rather than guessing which flag belonged to which URL.
    Followed {
        /// One per URL, in the order they were asked about.
        visited: Vec<bool>,
    },
    /// Paint a different band of the page already held.
    ///
    /// The point of the whole arrangement: the parse, the cascade, and the
    /// layout stay done, so moving down a long document costs only the pixels
    /// asked for. A band request never fetches anything.
    Band {
        /// First document column to paint (#204).
        left: u32,
        /// First document row to paint.
        top: u32,
        /// How many rows to paint.
        height: u32,
    },
    /// What lies between two points of the page already held.
    ///
    /// Asked of the child rather than worked out by the parent for the same
    /// reason find is: the text and where it sits are in the box tree, which
    /// never crosses the boundary. Only the two points do.
    Select {
        /// Where the drag started, in canvas coordinates.
        from: (f32, f32),
        /// Where it is now.
        to: (f32, f32),
    },
    /// Hand back whatever is selected inside the focused control, for the
    /// clipboard.
    ///
    /// Asked only when the reader presses a copy chord, never on a render. What
    /// is in a control is the page's business (ADR-0012) and the [`Focused`]
    /// states are deliberately three bits of nothing; this is the one way text
    /// leaves a control, and it leaves because a person asked for it — the same
    /// shape as a form submission, where the untrusted side proposes and the
    /// trusted side disposes.
    CopyFocused,
    /// The answers to a [`ToParent::Fetch`], one per URL and in the same order.
    ///
    /// Order is the whole matching rule: the parent answers a batch with
    /// exactly as many resources as were asked for, so nothing needs a request
    /// id and neither side has to hold a map. A batch of one is an ordinary
    /// single fetch, which is what a stylesheet is — its `@import` chain is
    /// only discoverable a link at a time.
    Resources {
        /// One per URL asked for, positionally.
        resources: Vec<Supplied>,
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
    /// The reader pressed a point on the page.
    ///
    /// The child decides what is there and focuses it, because the box tree is
    /// the only thing that knows and it never leaves this process. A point on
    /// nothing gives up whatever focus there was, which is what pressing the
    /// margin of a page means everywhere (#110).
    Focus {
        /// Where, in canvas coordinates.
        at: (f32, f32),
    },
    /// Keystrokes for whatever control is focused.
    ///
    /// A run of them rather than one, because a person types faster than a
    /// page re-renders (#207). Every keystroke costs the child a whole
    /// re-render — parse, cascade, layout, paint — so sending them one at a
    /// time made a word cost as many renders as it had letters, and each of
    /// those renders was work the next keystroke immediately invalidated.
    /// Applied in order, then rendered once.
    Type {
        /// What was pressed, oldest first. Never empty.
        keys: Vec<Key>,
    },
    /// The reader picked a row out of a dropdown the child opened.
    ///
    /// `node` is the child's own id for the `<select>`, echoed back exactly as
    /// it was sent. The parent never reads it and could not use it if it did.
    /// The child checks that it still names a `<select>` and that the index is
    /// one of its options, rather than trusting either: the parent is not the
    /// untrusted side here, but a message is a message, and the check costs a
    /// comparison.
    Choose {
        /// The `<select>` this is about, as the child named it.
        node: u32,
        /// Which of its options, in document order.
        index: u32,
    },
    /// Asks for the page's accessibility tree (ADR-0019, #9).
    ///
    /// Asked rather than sent with every render, and that is a security
    /// property as much as a saving: the tree is much the largest
    /// attacker-chosen structure that crosses this boundary, so the parsing
    /// surface it adds is not exercised at all until an assistive technology
    /// has actually attached. A page costs nothing when nothing is using it.
    ///
    /// Answered from the page already held, like [`ToChild::Find`]. The
    /// document and the box tree it is built from never cross, which is why
    /// the only thing that can answer is the process holding them.
    Accessibility,
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
                top,
                height,
                origin,
                path,
                force_authored,
                force_document,
                zoom,
            } => {
                writer.tag(0);
                writer.bytes(body);
                writer.some(content_type.is_some());
                if let Some(content_type) = content_type {
                    writer.str(content_type);
                }
                writer.u32(*width);
                writer.u32(*top);
                writer.u32(*height);
                writer.some(origin.is_some());
                if let Some(origin) = origin {
                    write_origin(&mut writer, origin);
                }
                writer.str(path);
                writer.some(*force_authored);
                writer.some(*force_document);
                writer.f32(*zoom);
            }
            ToChild::Find { query } => {
                writer.tag(2);
                writer.str(query);
            }
            ToChild::Focus { at } => {
                writer.tag(6);
                writer.f32(at.0);
                writer.f32(at.1);
            }
            ToChild::Accessibility => writer.tag(9),
            ToChild::Choose { node, index } => {
                writer.tag(8);
                writer.u32(*node);
                writer.u32(*index);
            }
            ToChild::Type { keys } => {
                writer.tag(7);
                writer.u32(keys.len() as u32);
                for key in keys {
                    key.write(&mut writer);
                }
            }
            ToChild::CopyFocused => writer.tag(11),
            ToChild::Select { from, to } => {
                writer.tag(4);
                writer.f32(from.0);
                writer.f32(from.1);
                writer.f32(to.0);
                writer.f32(to.1);
            }
            ToChild::Band { left, top, height } => {
                writer.tag(3);
                writer.u32(*left);
                writer.u32(*top);
                writer.u32(*height);
            }
            ToChild::Followed { visited } => {
                writer.tag(10);
                writer.u32(visited.len() as u32);
                for followed in visited {
                    writer.some(*followed);
                }
            }
            ToChild::Resources { resources } => {
                writer.tag(1);
                writer.u32(resources.len() as u32);
                for resource in resources {
                    writer.bytes(&resource.body);
                    writer.some(resource.content_type.is_some());
                    if let Some(content_type) = &resource.content_type {
                        writer.str(content_type);
                    }
                    writer.some(resource.ok);
                }
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
                let top = reader.u32()?;
                let height = reader.u32()?;
                let origin = if reader.some()? {
                    Some(read_origin(&mut reader)?)
                } else {
                    None
                };
                ToChild::Render {
                    body,
                    content_type,
                    width,
                    top,
                    height,
                    origin,
                    path: reader.str()?,
                    force_authored: reader.some()?,
                    force_document: reader.some()?,
                    zoom: reader.f32()?,
                }
            }
            1 => {
                // A count, not a plain `u32`: bounded by the bytes left, so a
                // claim of four billion resources cannot reserve for four
                // billion resources.
                let count = reader.count()?;
                let mut resources = Vec::with_capacity(count.min(64));
                for _ in 0..count {
                    let body = reader.bytes()?.to_vec();
                    let content_type = if reader.some()? {
                        Some(reader.str()?)
                    } else {
                        None
                    };
                    resources.push(Supplied {
                        body,
                        content_type,
                        ok: reader.some()?,
                    });
                }
                ToChild::Resources { resources }
            }
            2 => ToChild::Find {
                query: reader.str()?,
            },
            3 => ToChild::Band {
                left: reader.u32()?,
                top: reader.u32()?,
                height: reader.u32()?,
            },
            6 => ToChild::Focus {
                at: (reader.f32()?, reader.f32()?),
            },
            7 => {
                // A count, not a plain `u32`: bounded by the bytes left, so a
                // claim of four billion keystrokes cannot reserve for four
                // billion keystrokes.
                let count = reader.count()?;
                let mut keys = Vec::with_capacity(count.min(256));
                for _ in 0..count {
                    keys.push(Key::read(&mut reader)?);
                }
                ToChild::Type { keys }
            }
            8 => ToChild::Choose {
                node: reader.u32()?,
                index: reader.u32()?,
            },
            9 => ToChild::Accessibility,
            10 => {
                let count = reader.count()?;
                let mut visited = Vec::with_capacity(count.min(4096));
                for _ in 0..count {
                    visited.push(reader.some()?);
                }
                ToChild::Followed { visited }
            }
            11 => ToChild::CopyFocused,
            4 => ToChild::Select {
                from: (reader.f32()?, reader.f32()?),
                to: (reader.f32()?, reader.f32()?),
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
    /// Asks for subresources.
    ///
    /// The child cannot reach the network itself, so this is the only way it
    /// gets anything — and the parent applies ADR-0006's policy to every one of
    /// them, somewhere a compromised renderer cannot reach.
    ///
    /// Several at once rather than one, because the parent can then fetch them
    /// concurrently. Asking one at a time made a page wait for the sum of every
    /// subresource's latency rather than the longest, which on a page of twenty
    /// images is twenty round trips in a row. The child asks for as many as it
    /// knows about — every image on the page, once the cascade has run — and
    /// asks for one where one is all it can know, which is a stylesheet, whose
    /// `@import` chain is only discoverable a link at a time.
    Fetch {
        /// Absolute URLs, resolved by the child against the document.
        urls: Vec<String>,
        /// What they are for, so the policy can tell a navigation from a
        /// subresource.
        kind: RequestKind,
    },
    /// Asks which of these links the reader has already followed (#181).
    ///
    /// The direction is the point. The parent holds the history and could
    /// simply send it, and must not: the child is rendering a stranger's
    /// document, and a list of everywhere its reader has been is the last thing
    /// it should hold. So the child asks about the URLs *it parsed out of this
    /// page*, and the answer tells it nothing it did not already know existed.
    ///
    /// Bounded by the page: a document with ten thousand links asks about ten
    /// thousand URLs, and each one was already in the bytes the child was sent.
    Visited {
        /// Absolute URLs, resolved by the child against the document.
        urls: Vec<String>,
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
    /// The page's accessibility tree (ADR-0019, #9).
    ///
    /// Boxed for the reason `Rendered` is: this is by far the largest variant,
    /// and every other one would be as big as it in every message otherwise.
    Accessible(Box<Tree>),
    /// What a [`ToChild::Select`] drag covers.
    Selected {
        /// One rectangle per line it touches, to draw the highlight with.
        rects: Vec<Rect>,
        /// The text itself, for the clipboard.
        text: String,
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
    /// Canvas height: how many document rows `pixels` holds.
    pub height: u32,
    /// The document row `pixels` starts at.
    pub top: u32,
    /// The document column `pixels` starts at (#204).
    pub left: u32,
    /// Height of the content, which may exceed the canvas.
    pub content_height: f32,
    /// Width of the content, which may exceed the canvas (#204).
    pub content_width: f32,
    /// How it was rendered (ADR-0009).
    pub mode: Mode,
    /// The page's `<title>`, when it had one.
    pub title: Option<String>,
    /// Every link, with its rectangles already resolved to absolute URLs.
    pub links: Vec<Link>,
    /// Where an image was going to be and did not arrive (#118).
    ///
    /// The child's own knowledge travelling outward, which is the direction
    /// that is safe: it says what the page asked for, not what the parent did
    /// about it. A refusal and a failure are still the same thing on this
    /// side, so the placeholder these describe says `Load image` rather than
    /// naming a reason it does not have.
    pub missing: Vec<Missing>,
    /// Every picture on the page, with the address it came from (#205).
    ///
    /// Whether it arrived or not, which is what makes this a different list
    /// from `missing` rather than a longer one: that list is the placeholders a
    /// press can retry, and this is every picture a reader can point at, so the
    /// right-hand button has something to offer over one.
    pub pictures: Vec<Picture>,
    /// Whether there is a fallback decision to overrule.
    pub can_toggle_layout: bool,
    /// Where this page's buttons are, in document order (#110).
    ///
    /// The parent routes a press to the child by coordinates, so it does not
    /// strictly need these — but a *test* does, and so would a keyboard that
    /// could reach a button. Rectangles only: which form each belongs to and
    /// what it would send stays on the side that holds the document.
    pub buttons: Vec<Rect>,
    /// Where this page's other pressable controls are: a checkbox, a radio, a
    /// `<select>`.
    ///
    /// Separate from `buttons` because they answer a press differently — one
    /// sends a form and one changes what a form would send — and because the
    /// parent asks a different question of each. Rectangles only, like the
    /// buttons: which control each is, and what pressing it does, stays on the
    /// side that holds the document.
    pub pressables: Vec<Rect>,
    /// A form the reader asked to send, if they did (#110).
    ///
    /// Answered with the render rather than as a message of its own, because
    /// that is what happened: a key was pressed, the page is unchanged, and a
    /// navigation is being asked for. The parent reads it after the pixels and
    /// decides.
    pub submit: Option<Submission>,
    /// A dropdown the reader pressed, waiting to be drawn over the page.
    ///
    /// Answered with the render for the same reason `submit` is: a press
    /// happened, the page itself did not change, and something outside it is
    /// being asked for.
    pub open: Option<Dropdown>,
    /// What on this page has the keyboard (#110, #151).
    ///
    /// The parent needs to know whether a keystroke belongs to the page or to
    /// the window, and which keys the page would even use. It has no business
    /// knowing which control it is or what is in it.
    pub focused: Focused,
    /// How many images were fetched and decoded for this page.
    ///
    /// A diagnostic rather than something the window uses: `2kbrowser render`
    /// reports it, and a page that suddenly loads no images is how a broken
    /// subresource path first shows itself.
    pub images_loaded: u32,
    /// The colour the canvas was cleared to, packed as `0x00RRGGBB`.
    ///
    /// What the window fills the rows it has no pixels for with: below a page
    /// shorter than the window, and ahead of a band still being painted. It is
    /// always opaque — the child composites the page's background over white
    /// before sending it — so no alpha channel crosses the pipe.
    pub background: u32,
}

impl ToParent {
    /// Encodes to a frame.
    pub fn encode(&self) -> Vec<u8> {
        let mut writer = Writer::new();
        match self {
            ToParent::Fetch { urls, kind } => {
                writer.tag(0);
                writer.u32(urls.len() as u32);
                for url in urls {
                    writer.str(url);
                }
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
                writer.u32(page.top);
                writer.u32(page.left);
                writer.f32(page.content_height);
                writer.f32(page.content_width);
                page.mode.write(&mut writer);
                writer.some(page.title.is_some());
                if let Some(title) = &page.title {
                    writer.str(title);
                }
                writer.u32(page.links.len() as u32);
                for link in &page.links {
                    write_rect(&mut writer, &link.rect);
                    writer.str(&link.url);
                    writer.u32(link.group);
                    writer.some(link.jump_to.is_some());
                    if let Some(top) = link.jump_to {
                        writer.f32(top);
                    }
                    writer.some(link.pinned);
                }
                writer.u32(page.missing.len() as u32);
                for missing in &page.missing {
                    write_rect(&mut writer, &missing.rect);
                    writer.str(&missing.url);
                }
                writer.u32(page.pictures.len() as u32);
                for picture in &page.pictures {
                    write_rect(&mut writer, &picture.rect);
                    writer.str(&picture.url);
                }
                writer.u32(page.buttons.len() as u32);
                for rect in &page.buttons {
                    write_rect(&mut writer, rect);
                }
                writer.u32(page.pressables.len() as u32);
                for rect in &page.pressables {
                    write_rect(&mut writer, rect);
                }
                writer.some(page.submit.is_some());
                if let Some(submit) = &page.submit {
                    writer.str(&submit.action);
                    writer.some(submit.post);
                    writer.str(&submit.body);
                }
                writer.some(page.open.is_some());
                if let Some(open) = &page.open {
                    write_rect(&mut writer, &open.rect);
                    writer.u32(open.node);
                    writer.u32(open.options.len() as u32);
                    for option in &open.options {
                        writer.str(option);
                    }
                    writer.u32(open.on);
                }
                writer.some(page.can_toggle_layout);
                writer.tag(match page.focused {
                    Focused::Nothing => 0,
                    Focused::Typing => 1,
                    Focused::Pressable => 2,
                });
                writer.u32(page.images_loaded);
                writer.u32(page.background);
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
            ToParent::Accessible(tree) => {
                writer.tag(5);
                tree.write(&mut writer);
            }
            ToParent::Visited { urls } => {
                writer.tag(6);
                writer.u32(urls.len() as u32);
                for url in urls {
                    writer.str(url);
                }
            }
            ToParent::Selected { rects, text } => {
                writer.tag(4);
                writer.u32(rects.len() as u32);
                for rect in rects {
                    write_rect(&mut writer, rect);
                }
                writer.str(text);
            }
        }
        writer.finish()
    }

    /// Decodes a frame.
    pub fn decode(frame: &[u8]) -> Result<Self, WireError> {
        let mut reader = Reader::new(frame);
        let message = match reader.tag()? {
            0 => {
                // A count, so a claim of four billion URLs cannot reserve for
                // four billion URLs.
                let count = reader.count()?;
                let mut urls = Vec::with_capacity(count.min(64));
                for _ in 0..count {
                    urls.push(reader.str()?);
                }
                ToParent::Fetch {
                    urls,
                    kind: match reader.tag()? {
                        0 => RequestKind::Navigation,
                        1 => RequestKind::Subresource,
                        _ => return Err(WireError::Unknown),
                    },
                }
            }
            1 => {
                let pixels = reader.bytes()?.to_vec();
                let width = reader.u32()?;
                let height = reader.u32()?;
                let top = reader.u32()?;
                let left = reader.u32()?;
                let content_height = reader.f32()?;
                let content_width = reader.f32()?;
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
                        group: reader.u32()?,
                        jump_to: reader.some()?.then(|| reader.f32()).transpose()?,
                        pinned: reader.some()?,
                    });
                }
                let count = reader.count()?;
                let mut missing = Vec::with_capacity(count.min(1024));
                for _ in 0..count {
                    missing.push(Missing {
                        rect: read_rect(&mut reader)?,
                        url: reader.str()?,
                    });
                }
                let count = reader.count()?;
                let mut pictures = Vec::with_capacity(count.min(1024));
                for _ in 0..count {
                    pictures.push(Picture {
                        rect: read_rect(&mut reader)?,
                        url: reader.str()?,
                    });
                }
                let count = reader.count()?;
                let mut buttons = Vec::with_capacity(count.min(1024));
                for _ in 0..count {
                    buttons.push(read_rect(&mut reader)?);
                }
                let count = reader.count()?;
                let mut pressables = Vec::with_capacity(count.min(4096));
                for _ in 0..count {
                    pressables.push(read_rect(&mut reader)?);
                }
                let submit = if reader.some()? {
                    Some(Submission {
                        action: reader.str()?,
                        post: reader.some()?,
                        body: reader.str()?,
                    })
                } else {
                    None
                };
                let open = if reader.some()? {
                    let rect = read_rect(&mut reader)?;
                    let node = reader.u32()?;
                    // A count, so a claim of four billion options cannot
                    // reserve for four billion options.
                    let count = reader.count()?;
                    let mut options = Vec::with_capacity(count.min(1024));
                    for _ in 0..count {
                        options.push(reader.str()?);
                    }
                    Some(Dropdown {
                        rect,
                        node,
                        options,
                        on: reader.u32()?,
                    })
                } else {
                    None
                };
                let can_toggle_layout = reader.some()?;
                let focused = match reader.tag()? {
                    0 => Focused::Nothing,
                    1 => Focused::Typing,
                    2 => Focused::Pressable,
                    _ => return Err(WireError::Unknown),
                };
                let images_loaded = reader.u32()?;
                // Masked rather than rejected: the child is the untrusted side,
                // and a stray high byte here is a colour question, not a
                // structural one — there is nothing to refuse a frame over.
                let background = reader.u32()? & 0x00ff_ffff;
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
                    top,
                    left,
                    content_height,
                    content_width,
                    mode,
                    title,
                    links,
                    missing,
                    pictures,
                    buttons,
                    pressables,
                    submit,
                    open,
                    can_toggle_layout,
                    focused,
                    images_loaded,
                    background,
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
            4 => {
                // A count, not a plain `u32`: bounded by the bytes left, so a
                // claim of four billion lines cannot reserve for four billion
                // lines.
                let count = reader.count()?;
                let mut rects = Vec::with_capacity(count.min(4096));
                for _ in 0..count {
                    rects.push(read_rect(&mut reader)?);
                }
                ToParent::Selected {
                    rects,
                    text: reader.str()?,
                }
            }
            5 => ToParent::Accessible(Box::new(Tree::read(&mut reader)?)),
            6 => {
                // A count, for the reason the two above give: bounded by the
                // bytes left, so a claim of four billion links cannot reserve
                // for four billion links.
                let count = reader.count()?;
                let mut urls = Vec::with_capacity(count.min(4096));
                for _ in 0..count {
                    urls.push(reader.str()?);
                }
                ToParent::Visited { urls }
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
            top: 0,
            left: 0,
            content_height: 123.5,
            content_width: 456.25,
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
                group: 0,
                jump_to: Some(920.0),
                pinned: true,
            }],
            pictures: vec![Picture {
                rect: Rect {
                    x: 9.0,
                    y: 10.0,
                    width: 50.0,
                    height: 50.0,
                },
                url: "https://example.com/arrived.png".to_owned(),
            }],
            missing: vec![Missing {
                rect: Rect {
                    x: 5.0,
                    y: 6.0,
                    width: 7.0,
                    height: 8.0,
                },
                url: "https://cdn.example.net/photo.jpg".to_owned(),
            }],
            buttons: vec![Rect {
                x: 9.0,
                y: 10.0,
                width: 11.0,
                height: 12.0,
            }],
            submit: Some(Submission {
                action: "/search".to_owned(),
                post: false,
                body: "q=tables".to_owned(),
            }),
            can_toggle_layout: true,
            open: None,
            pressables: Vec::new(),
            focused: Focused::Nothing,
            images_loaded: 3,
            background: 0x001c_1b22,
        }
    }

    #[test]
    fn a_render_request_round_trips() {
        let message = ToChild::Render {
            body: b"<p>hello</p>".to_vec(),
            content_type: Some("text/html; charset=utf-8".to_owned()),
            width: 800,
            top: 0,
            height: 2000,
            origin: Some(
                net::parse_url("https://example.com/a.html")
                    .expect("parses")
                    .0,
            ),
            path: "/a.html".to_owned(),
            force_authored: true,
            force_document: false,
            zoom: 1.0,
        };
        assert_eq!(ToChild::decode(&message.encode()), Ok(message));
    }

    #[test]
    fn the_two_layout_overrides_do_not_cross_on_the_wire() {
        // Adjacent fields of the same shape, written and read by position. A
        // decoder that took them in the other order would round-trip both-false
        // and both-true unchanged, so the only case that catches the swap is
        // the one where they differ — which is also the only case either field
        // is ever set in.
        //
        // Worth its own test because the two mean opposite things: one asks for
        // the author's layout over the fallback, the other for the fallback
        // over the author's layout. Crossed, the reader presses a button and
        // gets the page they already had, with the bar saying they asked for
        // the other one.
        for (force_authored, force_document) in [(true, false), (false, true)] {
            let message = ToChild::Render {
                body: b"<p>hello</p>".to_vec(),
                content_type: None,
                width: 800,
                top: 0,
                height: 2000,
                origin: None,
                path: "/a.html".to_owned(),
                force_authored,
                force_document,
                zoom: 1.0,
            };
            let decoded = ToChild::decode(&message.encode());
            assert_eq!(decoded, Ok(message), "{force_authored} {force_document}");
        }
    }

    #[test]
    fn a_render_request_without_a_base_round_trips() {
        let message = ToChild::Render {
            body: Vec::new(),
            content_type: None,
            width: 1,
            top: 0,
            height: 1,
            origin: None,
            path: String::new(),
            force_authored: false,
            force_document: false,
            zoom: 1.0,
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
                top: 0,
                height: 10,
                origin: Some(origin),
                path,
                force_authored: false,
                force_document: false,
                zoom: 1.0,
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
                urls: vec![
                    "https://example.com/x.png".to_owned(),
                    "https://example.com/y.css".to_owned(),
                ],
                kind: RequestKind::Subresource,
            },
            ToParent::Fetch {
                urls: vec!["https://example.com/".to_owned()],
                kind: RequestKind::Navigation,
            },
            // No URLs at all. The child does not send this, and the decoder
            // still has to have an answer for it.
            ToParent::Fetch {
                urls: Vec::new(),
                kind: RequestKind::Subresource,
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
        // Well past the tags either side uses, and deliberately not "one more
        // than the last one": this test used tag 9 until #9's accessibility
        // request became tag 9, at which point it was asserting that a message
        // the wire knows is a message the wire does not.
        assert_eq!(ToChild::decode(&[200]), Err(WireError::Unknown));
        assert_eq!(ToParent::decode(&[200]), Err(WireError::Unknown));
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
                top: 0,
                height: 600,
                origin: Some(net::parse_url("https://example.com/").expect("parses").0),
                path: "/".to_owned(),
                force_authored: false,
                force_document: true,
                zoom: 1.0,
            }
            .encode(),
            ToParent::Rendered(Box::new(rendered(3, 2))).encode(),
            ToParent::Fetch {
                urls: vec![
                    "https://example.com/a".to_owned(),
                    "https://example.com/b".to_owned(),
                ],
                kind: RequestKind::Subresource,
            }
            .encode(),
            ToChild::Resources {
                resources: vec![
                    Supplied {
                        body: b"one".to_vec(),
                        content_type: Some("text/css".to_owned()),
                        ok: true,
                    },
                    Supplied::default(),
                ],
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
        //
        // The fields up to the count are written out one by one because they
        // have to be *got past* — the reader is a stream, so a frame that does
        // not spell them exactly runs out before it reaches the count and comes
        // back `Truncated`, which is a different refusal from the one this is
        // about. That makes this a mirror of the encoder above, and a field
        // added there has to be added here too.
        let mut writer = Writer::new();
        writer.tag(1);
        writer.bytes(&[]);
        writer.u32(0); // width
        writer.u32(0); // height
        writer.u32(0); // top
        writer.u32(0); // left
        writer.f32(0.0); // content height
        writer.f32(0.0); // content width
        writer.tag(0); // mode
        writer.some(false); // no title
        writer.u32(1_000_000_000);
        assert_eq!(
            ToParent::decode(&writer.finish()),
            Err(WireError::BadLength)
        );
    }

    fn box_() -> Rect {
        Rect {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
        }
    }

    #[test]
    fn an_accessibility_request_and_its_answer_survive_the_wire() {
        assert_eq!(
            ToChild::decode(&ToChild::Accessibility.encode()),
            Ok(ToChild::Accessibility),
        );
        let tree = crate::access::Tree {
            nodes: vec![
                crate::access::Node {
                    name: "A page".to_owned(),
                    children: 1,
                    ..crate::access::Node::new(crate::access::Role::Document, box_())
                },
                crate::access::Node {
                    name: "Next".to_owned(),
                    value: Some("/next".to_owned()),
                    ..crate::access::Node::new(crate::access::Role::Link, box_())
                },
            ],
            focus: Some(1),
        };
        let message = ToParent::Accessible(Box::new(tree));
        assert_eq!(ToParent::decode(&message.encode()), Ok(message));
    }

    #[test]
    fn an_accessibility_frame_that_breaks_a_bound_is_refused() {
        // The bounds are enforced where the bytes are read, not somewhere the
        // caller has to remember to look — so this is the same `decode` every
        // other message goes through, answering `Err` rather than handing back
        // a tree with a hole in it.
        let tree = crate::access::Tree {
            nodes: vec![crate::access::Node {
                name: "a".repeat(crate::access::MAX_STRING + 1),
                ..crate::access::Node::new(crate::access::Role::Text, box_())
            }],
            focus: None,
        };
        let frame = ToParent::Accessible(Box::new(tree)).encode();
        assert_eq!(ToParent::decode(&frame), Err(WireError::BadLength));
    }
}
