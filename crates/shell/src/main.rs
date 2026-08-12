//! The 2kbrowser command line.
//!
//! `render` fetches a URL or file and writes a PNG; `open` shows it in a
//! window. The headless path is what reference tests and CI depend on, and it is
//! what makes the window a thin shell over a tested core.

use std::process::ExitCode;

use shell::viewport::Viewport;
use shell::window;

const USAGE: &str = "\
2kbrowser — a web browser without the slop

USAGE:
    2kbrowser open   <url-or-file> [--width <px>] [--height <px>]
    2kbrowser render <url-or-file> [--out <file.png>] [--width <px>] [--height <px>]
    2kbrowser links  <url-or-file> [--width <px>]
    2kbrowser bookmarks

OPTIONS:
    --out <path>     Where to write the PNG (render only; default: page.png)
    --width <px>     Viewport width (default: 800)
    --height <px>    Window height, or maximum canvas height for render

`links` lists every link on the page with the rectangle you would click to
follow it — the same geometry the window uses, printed instead of drawn.

`bookmarks` prints the saved list, and says where the file is. It is a plain
tab-separated file: edit it in anything.

Accepts http:, https:, and file: URLs, or a plain path. Third-party requests
are refused by default (ADR-0006) and JavaScript is never run (ADR-0003).

In a window: click a link to follow it, or Tab to it and press Enter — Shift+Tab
goes back, Escape drops the focus. Alt+Left and Alt+Right, or Backspace,
go back and forward. Ctrl+L focuses the URL bar and Ctrl+F searches the page;
Enter goes, Escape gives up. Ctrl+T opens a tab, Ctrl+W closes one, Ctrl+Tab
switches. Ctrl+D saves the page and Ctrl+B shows the saved list. Arrows and
PageUp/PageDown scroll, Home/End jump, Esc or q quits.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        // The renderer child (ADR-0012). Not in the usage text: it is how the
        // browser talks to itself, not something to run by hand.
        //
        // Nothing may be printed on this path — stdout is the protocol, and a
        // stray line would be read as a frame header.
        // Applies the sandbox and reports what it could and could not do. The
        // only honest way to test a sandbox is from inside one.
        Some(sandbox::confine::SELFTEST_ARGUMENT) => {
            println!("{}", sandbox::confine::selftest());
            ExitCode::SUCCESS
        }
        // The far half of the self-test, for platforms where the confinement is
        // applied from outside and so cannot be applied by the process running
        // the probes. Not in the usage text: the self-test runs this on itself.
        Some(sandbox::confine::SELFTEST_PROBE_ARGUMENT) => {
            println!("{}", sandbox::confine::selftest_probe(&args[1..]));
            ExitCode::SUCCESS
        }
        Some(sandbox::CHILD_ARGUMENT) => match shell::isolated::run_child() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("renderer: {error}");
                ExitCode::FAILURE
            }
        },
        Some("render") => report(run_render(&args[1..])),
        Some("links") => report(run_links(&args[1..])),
        Some("open") => report(run_open(&args[1..])),
        Some("bookmarks") => report(run_bookmarks()),
        Some("--help" | "-h" | "help") | None => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("error: unknown command `{other}`\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

/// Prints a command's outcome and turns it into an exit code.
fn report(outcome: Result<String, String>) -> ExitCode {
    match outcome {
        Ok(message) => {
            if !message.is_empty() {
                println!("{message}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Opens a window. See the caveat in `window.rs`: this path is unverified.
fn run_open(args: &[String]) -> Result<String, String> {
    let options = Options::parse(args)?;
    let input = options.input.ok_or("no input given")?;
    let (document, url) = load_from(&input)?;
    window::open(
        document.body,
        document.content_type,
        url,
        document.origin,
        document.path,
        options.width,
        if options.height_given {
            options.height
        } else {
            // `render` writes a PNG, so its 4000px default is a canvas that
            // shrinks to the content. A window is not a canvas: asking for one
            // 2000px tall — as this did — puts most of it below the bottom of
            // an ordinary screen, and the browser then believes the whole page
            // is visible and refuses to scroll to the part that is not.
            WINDOW_HEIGHT
        },
    )?;
    Ok(String::new())
}

/// Height of a window that nobody asked to be a particular size.
const WINDOW_HEIGHT: u32 = 800;

/// Options shared by both commands.
struct Options {
    input: Option<String>,
    output: String,
    width: u32,
    height: u32,
    /// Whether `--height` was given, so `open` can tell a deliberate window
    /// height from `render`'s canvas default.
    height_given: bool,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Options {
            input: None,
            output: "page.png".to_owned(),
            width: 800,
            height: 4000,
            height_given: false,
        };
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--out" => options.output = take(args, &mut index, "--out")?,
                "--width" => {
                    options.width = take(args, &mut index, "--width")?
                        .parse()
                        .map_err(|_| "--width must be a number".to_owned())?;
                }
                "--height" => {
                    options.height_given = true;
                    options.height = take(args, &mut index, "--height")?
                        .parse()
                        .map_err(|_| "--height must be a number".to_owned())?;
                }
                other if options.input.is_none() => options.input = Some(other.to_owned()),
                other => return Err(format!("unexpected argument `{other}`")),
            }
            index += 1;
        }
        Ok(options)
    }
}

/// Lists the page's links and where each one is.
///
/// The window will draw these; printing them is how the geometry gets tested
/// without a display, and how you check that a link on a real page is where it
/// looks like it is.
fn run_links(args: &[String]) -> Result<String, String> {
    let options = Options::parse(args)?;
    let input = options.input.ok_or("no input given")?;
    let page = render_in_child(&input, options.width, options.height)?;

    let links: Vec<(layout::Rect, String)> = page
        .links()
        .into_iter()
        .flat_map(|link| {
            link.rects
                .into_iter()
                .map(move |rect| (rect, link.url.clone()))
        })
        .collect();
    if links.is_empty() {
        return Ok("no links on this page".to_owned());
    }
    let mut message = format!("{} link rectangle(s):", links.len());
    for (rect, url) in links {
        message.push_str(&format!(
            "\n  {:>5},{:<5} {:>4}x{:<4}  {url}",
            rect.x.round(),
            rect.y.round(),
            rect.width.round(),
            rect.height.round(),
        ));
    }
    Ok(message)
}

/// Prints the saved list.
///
/// The file is the only state this browser keeps between runs, so it is worth
/// being able to see it without opening a window — and worth saying where it
/// is, because it is a text file anyone can edit.
fn run_bookmarks() -> Result<String, String> {
    let path = shell::bookmarks::default_path();
    let marks = shell::bookmarks::Bookmarks::load(&path);
    if marks.is_empty() {
        return Ok(format!("nothing saved yet ({})", path.display()));
    }
    let mut message = format!("{} saved page(s) in {}:", marks.len(), path.display());
    for entry in marks.iter() {
        message.push_str("\n  ");
        message.push_str(&entry.url);
        if !entry.title.is_empty() {
            message.push_str(&format!("\n      {}", entry.title));
        }
    }
    Ok(message)
}

fn run_render(args: &[String]) -> Result<String, String> {
    let options = Options::parse(args)?;
    let output = options.output;
    let (width, height) = (options.width, options.height);

    let input = options.input.ok_or("no input given")?;
    let page = render_in_child(&input, width, height)?;

    let pixmap = page
        .to_pixmap()
        .ok_or_else(|| format!("{output}: the renderer returned an unusable canvas"))?;
    pixmap
        .save_png(&output)
        .map_err(|e| format!("{output}: {e}"))?;

    let mut message = format!("wrote {output} ({}x{})", page.width(), page.height());
    if page.images_loaded() > 0 {
        message.push_str(&format!(", {} image(s)", page.images_loaded()));
    }
    // Still a real clip here, and still worth saying: a PNG is one image, so
    // `--height` is a ceiling rather than a band. The window has no such limit
    // any more — it paints whatever rows the reader scrolls to.
    if page.content_height().ceil() > page.height() as f32 {
        message.push_str(&format!(
            "\nnote: page is {}px tall; output was clipped to {}px",
            page.content_height().ceil() as u32,
            page.height()
        ));
    }
    // ADR-0009 forbids switching rendering mode silently. With no chrome to
    // show a banner in, the CLI says it here.
    if let Some(explanation) = page.mode().explanation() {
        message.push('\n');
        message.push_str(&explanation);
    }
    // ADR-0006: plain HTTP is allowed but must never be presented as secure.
    if page.origin().scheme == net::Scheme::Http {
        message.push_str(
            "\nnote: loaded over plain HTTP — not authenticated, and modifiable in transit",
        );
    }
    Ok(message)
}

/// Fetches a page and renders it in a renderer child.
///
/// The command line used to parse and lay out in this process, which made
/// ADR-0012 a property of the window rather than of the browser: `2kbrowser
/// render https://example.com` would fetch a stranger's HTML and hand it
/// straight to the parsers, in the process holding the network and the disk.
/// The child is the same binary and the same confinement the window gets.
///
/// The canvas is capped at what one frame can carry, so a `--height` past that
/// is a clipped page with a note rather than a renderer that cannot answer.
fn render_in_child(input: &str, width: u32, height: u32) -> Result<Viewport, String> {
    let (document, _) = load_from(input)?;
    let renderer = sandbox::Renderer::new().map_err(|error| error.to_string())?;
    if !renderer.confinement().is_confined() {
        eprintln!("2kbrowser: {}", renderer.confinement().describe());
        if let Some(reason) = renderer.confinement_failure() {
            eprintln!("2kbrowser: {reason}");
        }
    }
    Viewport::open(
        &renderer,
        document,
        width,
        height.min(sandbox::max_canvas_height(width)),
        false,
    )
    .map_err(|error| error.to_string())
}

/// Loads the target, accepting a URL or a bare filesystem path, and reports the
/// absolute URL it settled on.
///
/// A bare path is a convenience, resolved to an absolute `file:` URL so that
/// everything downstream sees one representation and the policy has an origin
/// to judge subresources against. The window needs the URL as well: it is the
/// first history entry, and every relative link on the page resolves against
/// it.
///
/// Raw, and *with* the `Content-Type`. The bytes stay undecoded because the
/// encoding sniffer lives on the far side of the renderer boundary with every
/// other parser (ADR-0012) — but the header has to travel with them, because on
/// a page that declares its encoding nowhere else the header is the only thing
/// that knows. Dropping it decoded Hacker News, which says `charset=utf-8` in
/// the header and nothing in the markup, as windows-1252: every em dash and
/// curly quote came out as mojibake.
fn load_from(input: &str) -> Result<(shell::viewport::Document, String), String> {
    let fetcher = net::Fetcher::default();
    let url = absolute_url(input)?;
    let fetched = fetcher
        .fetch_raw(&url, None, net::RequestKind::Navigation)
        .map_err(|error| format!("{url}: {error}"))?;
    if fetched.trust == net::Trust::LocalRoot {
        // ADR-0015: usable behind an intercepting proxy, and never quiet about
        // it. The window marks this in the bar; the command line has only
        // stderr to say it in.
        eprintln!(
            "2kbrowser: {url}: verified only by a certificate this computer trusts — \
             something on this network can read it"
        );
    }
    Ok((
        shell::viewport::Document {
            body: fetched.body,
            content_type: fetched.content_type,
            origin: fetched.origin,
            path: fetched.path,
        },
        url,
    ))
}

/// Turns a command-line argument into a URL.
///
/// A bare path is taken as a file, which is what someone typing a filename
/// means; anything naming a scheme is left alone.
fn absolute_url(input: &str) -> Result<String, String> {
    // Before the scheme test: `C:` satisfies every rule for a scheme, so a
    // Windows path given on the command line would otherwise be handed
    // straight to the URL parser, which wants a `://` it does not have.
    if !net::policy::is_drive_path(input) && net::policy::has_scheme(input) {
        return Ok(input.to_owned());
    }
    let path = std::path::Path::new(input);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    Ok(net::file_url(&absolute))
}

fn take(args: &[String], index: &mut usize, flag: &str) -> Result<String, String> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value"))
}
