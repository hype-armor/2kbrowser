//! Rendering in a separate process.
//!
//! The `shell` half of ADR-0012: `sandbox` supplies the transport and knows
//! nothing about rendering; this supplies the rendering and knows nothing about
//! pipes. The two meet at [`sandbox::child::Render`].
//!
//! Nothing here changes what a page looks like. The child runs exactly the
//! pipeline [`crate::render`] runs in-process, and the test that matters says
//! so: the pixels that come back across the pipe are byte-identical to the ones
//! produced without one.

use sandbox::child::{Fetched, Render};
use sandbox::message::{Link, Mode, Rendered};
use sandbox::{Error, Renderer, ToChild};
use text::FontStore;

/// Loads subresources by asking the parent.
///
/// The child has no sockets and no filesystem of its own (ADR-0012), so this is
/// the only way anything gets in. Every request crosses the pipe and the parent
/// applies ADR-0006's policy — which is the improvement worth having: the rule
/// is now enforced in a process a compromised renderer cannot reach.
struct PipeLoader<'a> {
    fetch: &'a mut dyn FnMut(&str, net::RequestKind) -> Fetched,
}

impl crate::render::Loader for PipeLoader<'_> {
    fn load(
        &mut self,
        url: &str,
        _document: Option<&net::Origin>,
        kind: net::RequestKind,
    ) -> Option<crate::render::Loaded> {
        // The document origin is dropped rather than sent. The parent already
        // knows it — it is what the parent asked for a render of — and taking
        // it from the untrusted side would let a compromised renderer claim to
        // be an origin it is not, which is the whole policy defeated in one
        // field.
        let resource = (self.fetch)(url, kind)?;
        Some(crate::render::Loaded {
            bytes: resource.bytes,
            content_type: resource.content_type,
        })
    }
}

/// Renders using this crate's pipeline. The child's half.
pub struct PageRenderer {
    fonts: FontStore,
    /// The page most recently rendered, kept so it can still be searched.
    ///
    /// This is the whole reason the child outlives a single message: the text
    /// and the box tree a find query searches never cross the boundary, so the
    /// only thing that can answer is the process holding them.
    page: Option<crate::render::Page>,
}

impl Default for PageRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl PageRenderer {
    /// A renderer with the bundled fonts loaded.
    pub fn new() -> Self {
        Self {
            fonts: FontStore::new(),
            page: None,
        }
    }
}

impl Render for PageRenderer {
    fn render(
        &mut self,
        request: &ToChild,
        fetch: &mut dyn FnMut(&str, net::RequestKind) -> Fetched,
    ) -> Result<Rendered, String> {
        let ToChild::Render {
            body,
            content_type,
            width,
            max_height,
            origin,
            path,
            force_authored,
        } = request
        else {
            return Err("expected a render request".to_owned());
        };

        // Decoded here rather than by the parent, so the encoding sniffer stays
        // on the sandboxed side with every other parser.
        let (html, ..) = net::encoding::decode_document(body, content_type.as_deref());

        // Every subresource — images, stylesheets, `@import` chains, frames —
        // goes over the pipe. Nothing in this process opens a socket or a file.
        let mut loader = PipeLoader { fetch };
        let base = origin.as_ref().map(|origin| (origin, path.as_str()));
        let page = if *force_authored {
            crate::render::render_as_authored_with(
                &html,
                *width,
                *max_height,
                &mut self.fonts,
                &mut loader,
                base,
            )
        } else {
            crate::render::render_with_base_and_loader(
                &html,
                *width,
                *max_height,
                &mut self.fonts,
                &mut loader,
                base,
            )
        };

        let mode = match &page.mode {
            layout::RenderMode::Authored => Mode::Authored,
            layout::RenderMode::Document { unsupported_share } => Mode::Document {
                unsupported_share: *unsupported_share,
            },
            layout::RenderMode::RequiresScripting => Mode::RequiresScripting,
        };
        let can_toggle_layout =
            *force_authored || !matches!(page.mode, layout::RenderMode::Authored);

        let rendered = Rendered {
            pixels: page.pixmap.data().to_vec(),
            width: page.pixmap.width(),
            height: page.pixmap.height(),
            content_height: page.content_height,
            mode,
            title: page.title.clone(),
            links: page
                .link_groups()
                .into_iter()
                .enumerate()
                .flat_map(|(group, link)| {
                    let url = link.url;
                    link.rects.into_iter().map(move |rect| Link {
                        rect,
                        url: url.clone(),
                        group: group as u32,
                    })
                })
                .collect(),
            can_toggle_layout,
        };
        // Kept for the questions that come after: find, and re-rendering at a
        // new width without re-fetching anything.
        self.page = Some(page);
        Ok(rendered)
    }

    fn find(&mut self, query: &str) -> Vec<layout::Rect> {
        // An empty query matches everything, which is not what a reader who has
        // just cleared the box wants to see.
        if query.trim().is_empty() {
            return Vec::new();
        }
        match &self.page {
            Some(page) => page.find(query),
            None => Vec::new(),
        }
    }
}

/// Runs this process as a renderer child, reading from stdin and writing to
/// stdout.
///
/// Locked for the whole conversation: anything else writing to stdout would
/// interleave with a frame and corrupt it, and this is the one process where a
/// stray `println!` is a protocol violation rather than noise.
pub fn run_child() -> Result<(), Error> {
    // Before anything is read. The very first frame carries the document, so it
    // is already attacker-influenced — confining afterwards would be confining
    // after the interesting bytes had arrived.
    //
    // The font store is built after this on purpose too: it reads only embedded
    // data (ADR-0010), and building it under the filter is the check that it
    // really does not touch the filesystem.
    let confinement = sandbox::confine::apply();
    // Only when the platform *has* a sandbox and it failed anyway — a kernel
    // too old, or a container that forbids installing a filter. That is a fact
    // about this machine and worth a line every time.
    //
    // A platform with no implementation is a fact about the *build*, and saying
    // it here meant every spawned child said it: twenty lines in one test run,
    // which is how a warning becomes something people scroll past. The parent
    // says that one once, at startup.
    if confinement == sandbox::Confinement::Failed {
        // stderr, never stdout: stdout is the protocol.
        eprintln!("2kbrowser renderer: {}", confinement.describe());
    }

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    sandbox::child::serve(&mut input, &mut output, &mut PageRenderer::new())
}

/// Renders a document in a child process.
///
/// The parent's half, for callers that have already fetched the document.
pub fn render_isolated(
    body: Vec<u8>,
    content_type: Option<String>,
    width: u32,
    max_height: u32,
    origin: Option<net::Origin>,
    path: String,
    force_authored: bool,
) -> Result<Rendered, Error> {
    Renderer::new()?.render(
        body,
        content_type,
        width,
        max_height,
        origin,
        path,
        force_authored,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(body: &[u8], width: u32) -> ToChild {
        ToChild::Render {
            body: body.to_vec(),
            content_type: None,
            width,
            max_height: 2000,
            origin: None,
            path: String::new(),
            force_authored: false,
        }
    }

    fn no_fetch(_: &str, _: net::RequestKind) -> Fetched {
        None
    }

    /// A real PNG, so a decode that succeeds means the bytes arrived intact.
    fn tile() -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/ref/fixtures/assets/tile.png"),
        )
        .expect("the reference fixture tile")
    }

    #[test]
    fn the_child_renders_the_same_pixels_as_the_in_process_path() {
        // The property the whole boundary rests on: moving rendering across a
        // process changes nothing about what a page looks like. Byte-identical,
        // not merely similar — ADR-0005's determinism is what makes that a fair
        // thing to demand.
        let html = "<body bgcolor=\"#eef\"><h1>Heading</h1><p>Some <b>text</b> here.</p>\
                    <table border=1><tr><td>a</td><td>b</td></tr></table></body>";

        let mut fonts = FontStore::new();
        let direct = crate::render::render(html, 300, 2000, &mut fonts);

        let mut renderer = PageRenderer::new();
        let crossed = renderer
            .render(&request(html.as_bytes(), 300), &mut no_fetch)
            .expect("renders");

        assert_eq!(crossed.width, direct.pixmap.width());
        assert_eq!(crossed.height, direct.pixmap.height());
        assert_eq!(
            crossed.pixels,
            direct.pixmap.data(),
            "the child produced different pixels"
        );
        assert_eq!(crossed.content_height, direct.content_height);
        assert_eq!(crossed.title, direct.title);
    }

    #[test]
    fn a_title_and_a_mode_survive_the_crossing() {
        let html = "<title>Named</title><body><p>x</p></body>";
        let mut renderer = PageRenderer::new();
        let page = renderer
            .render(&request(html.as_bytes(), 200), &mut no_fetch)
            .expect("renders");
        assert_eq!(page.title.as_deref(), Some("Named"));
        assert_eq!(page.mode, Mode::Authored);
        assert!(!page.can_toggle_layout, "nothing to overrule here");
    }

    #[test]
    fn the_document_fallback_crosses_with_its_share_intact() {
        // ADR-0009 forbids switching rendering mode silently, and the chrome
        // can only say what it was told — so the number has to survive.
        let html = "<body><div style=\"display: flex\">\
                    <div style=\"display: grid\">a</div></div></body>";
        let mut renderer = PageRenderer::new();
        let page = renderer
            .render(&request(html.as_bytes(), 200), &mut no_fetch)
            .expect("renders");
        match page.mode {
            Mode::Document { unsupported_share } => {
                assert!(unsupported_share > 0.0, "{unsupported_share}");
                assert!(page.can_toggle_layout, "there is a decision to overrule");
            }
            other => panic!("expected the document fallback, got {other:?}"),
        }
    }

    #[test]
    fn the_bytes_are_decoded_on_the_child_side() {
        // A page declaring nothing is windows-1252, and the sniffer belongs
        // with every other parser — on the far side of the boundary.
        let mut body = b"<body><p>".to_vec();
        // An em dash in windows-1252.
        body.push(0x97);
        body.extend_from_slice(b"</p></body>");

        let mut renderer = PageRenderer::new();
        let page = renderer
            .render(&request(&body, 200), &mut no_fetch)
            .expect("renders");
        assert!(page.width > 0);
    }

    #[test]
    fn a_page_with_no_base_still_renders_and_carries_no_links() {
        // Link geometry is resolved against a base; without one there is
        // nothing to navigate to, so the parent gets none rather than getting
        // unresolved ones it would have to interpret.
        let html = "<body><a href=\"b.html\">there</a></body>";
        let mut renderer = PageRenderer::new();
        let page = renderer
            .render(&request(html.as_bytes(), 200), &mut no_fetch)
            .expect("renders");
        assert!(page.links.is_empty());
    }

    #[test]
    fn links_cross_already_resolved_to_absolute_urls() {
        let dir = std::env::temp_dir().join("2kbrowser-isolated-tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("a.html");
        let html = "<body><p><a href=\"b.html\">there</a></p></body>";
        std::fs::write(&path, html).expect("write");
        let (origin, at) = net::parse_url(&net::file_url(&path)).expect("parses");

        let mut renderer = PageRenderer::new();
        let page = renderer
            .render(
                &ToChild::Render {
                    body: html.as_bytes().to_vec(),
                    content_type: None,
                    width: 300,
                    max_height: 2000,
                    origin: Some(origin),
                    path: at,
                    force_authored: false,
                },
                &mut no_fetch,
            )
            .expect("renders");

        assert_eq!(page.links.len(), 1, "{:?}", page.links);
        assert!(
            page.links[0].url.ends_with("/b.html"),
            "{}",
            page.links[0].url
        );
        assert!(
            page.links[0].url.starts_with("file:///"),
            "resolved absolutely, so the parent never resolves anything: {}",
            page.links[0].url
        );
    }

    #[test]
    fn every_subresource_is_asked_for_rather_than_fetched() {
        // The property this whole change exists for. Nothing in the child may
        // open a socket or a file: a stylesheet, an `@import` inside it, and an
        // image all have to arrive as answers to requests.
        let dir = std::env::temp_dir().join("2kbrowser-pipe-loader");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (origin, at) = net::parse_url(&net::file_url(&dir.join("page.html"))).expect("parses");

        let html = "<html><head><link rel=\"stylesheet\" href=\"site.css\"></head>\
                    <body><img src=\"tile.png\"><p>text</p></body></html>";

        let mut asked: Vec<String> = Vec::new();
        let mut fetch = |url: &str, _kind: net::RequestKind| -> Fetched {
            asked.push(url.to_owned());
            if url.ends_with("site.css") {
                return Some(sandbox::child::Resource {
                    bytes: b"@import url(more.css); p { color: #ff0000 }".to_vec(),
                    content_type: Some("text/css".to_owned()),
                });
            }
            if url.ends_with("more.css") {
                return Some(sandbox::child::Resource {
                    bytes: b"body { background: #00ff00 }".to_vec(),
                    content_type: None,
                });
            }
            if url.ends_with("tile.png") {
                return Some(sandbox::child::Resource {
                    bytes: tile(),
                    content_type: Some("image/png".to_owned()),
                });
            }
            None
        };

        let page = PageRenderer::new()
            .render(
                &ToChild::Render {
                    body: html.as_bytes().to_vec(),
                    content_type: None,
                    width: 300,
                    max_height: 600,
                    origin: Some(origin),
                    path: at,
                    force_authored: false,
                },
                &mut fetch,
            )
            .expect("renders");

        assert!(
            asked.iter().any(|url| url.ends_with("site.css")),
            "the stylesheet was not asked for: {asked:?}"
        );
        assert!(
            asked.iter().any(|url| url.ends_with("more.css")),
            "the @import inside it was not asked for: {asked:?}"
        );
        assert!(
            asked.iter().any(|url| url.ends_with("tile.png")),
            "the image was not asked for: {asked:?}"
        );

        // And the answers were actually used. The imported sheet paints the
        // body green, which nothing else in this page does.
        let green = page
            .pixels
            .chunks_exact(4)
            .filter(|p| p[0] < 80 && p[1] > 150 && p[2] < 80)
            .count();
        assert!(green > 0, "the imported stylesheet did not reach the paint");
    }

    #[test]
    fn a_subresource_the_parent_refuses_is_simply_absent() {
        // A refusal and a failure look identical to the child, and both render
        // as "no image" rather than as an error. Nothing about the parent's
        // policy leaks across.
        let dir = std::env::temp_dir().join("2kbrowser-pipe-loader");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (origin, at) = net::parse_url(&net::file_url(&dir.join("page.html"))).expect("parses");

        let html = "<body><img src=\"https://tracker.example.net/pixel.gif\"><p>text</p></body>";
        let mut refused = 0usize;
        let mut fetch = |_url: &str, _kind: net::RequestKind| -> Fetched {
            refused += 1;
            None
        };

        let page = PageRenderer::new()
            .render(
                &ToChild::Render {
                    body: html.as_bytes().to_vec(),
                    content_type: None,
                    width: 200,
                    max_height: 200,
                    origin: Some(origin),
                    path: at,
                    force_authored: false,
                },
                &mut fetch,
            )
            .expect("renders anyway");

        assert_eq!(refused, 1, "it was asked for exactly once");
        assert!(page.width > 0, "the page still rendered");
    }
}
