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

/// Renders using this crate's pipeline. The child's half.
pub struct PageRenderer {
    fonts: FontStore,
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
        }
    }
}

impl Render for PageRenderer {
    fn render(
        &mut self,
        request: &ToChild,
        _fetch: &mut dyn FnMut(&str, net::RequestKind) -> Fetched,
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

        // Subresources still go through the in-process fetcher for now. That is
        // the remaining hole in ADR-0012's story and it is deliberate: routing
        // them through `fetch` means rewriting how `render_sized` loads images,
        // stylesheets, and frames, and doing it in the same change as the
        // process boundary would make a large diff impossible to review. The
        // boundary lands first, provably identical; the network moves next.
        let base = origin.as_ref().map(|origin| (origin, path.as_str()));
        let page = if *force_authored {
            crate::render::render_as_authored(&html, *width, *max_height, &mut self.fonts, base)
        } else {
            crate::render::render_with_base(&html, *width, *max_height, &mut self.fonts, base)
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

        Ok(Rendered {
            pixels: page.pixmap.data().to_vec(),
            width: page.pixmap.width(),
            height: page.pixmap.height(),
            content_height: page.content_height,
            mode,
            title: page.title.clone(),
            links: page
                .links()
                .into_iter()
                .map(|(rect, url)| Link { rect, url })
                .collect(),
            can_toggle_layout,
        })
    }
}

/// Runs this process as a renderer child, reading from stdin and writing to
/// stdout.
///
/// Locked for the whole conversation: anything else writing to stdout would
/// interleave with a frame and corrupt it, and this is the one process where a
/// stray `println!` is a protocol violation rather than noise.
pub fn run_child() -> Result<(), Error> {
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
}
