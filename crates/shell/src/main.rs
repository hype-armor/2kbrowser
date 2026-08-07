//! The 2kbrowser command line.
//!
//! `render` fetches a URL or file and writes a PNG; `open` shows it in a
//! window. The headless path is what reference tests and CI depend on, and it is
//! what makes the window a thin shell over a tested core.

use std::process::ExitCode;

use shell::{render, window};

use text::FontStore;

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
    let (resource, url) = load_from(&input)?;
    window::open(
        // The raw bytes, not the decoded text: the window renders in a child
        // process, and the encoding sniffer lives there with every other parser
        // (ADR-0012).
        resource.bytes,
        None,
        url,
        resource.origin,
        resource.path,
        options.width,
        options.height.min(2000),
    )?;
    Ok(String::new())
}

/// Options shared by both commands.
struct Options {
    input: Option<String>,
    output: String,
    width: u32,
    height: u32,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Options {
            input: None,
            output: "page.png".to_owned(),
            width: 800,
            height: 4000,
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
    let resource = load(&input)?;

    let mut fonts = FontStore::new();
    let page = render::render_with_base(
        &resource.body,
        options.width,
        options.height,
        &mut fonts,
        Some((&resource.origin, &resource.path)),
    );

    let links = page.links();
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
    let resource = load(&input)?;

    let mut fonts = FontStore::new();
    let page = render::render_with_base(
        &resource.body,
        width,
        height,
        &mut fonts,
        Some((&resource.origin, &resource.path)),
    );
    page.pixmap
        .save_png(&output)
        .map_err(|e| format!("{output}: {e}"))?;

    let mut message = format!(
        "wrote {output} ({}x{})",
        page.pixmap.width(),
        page.pixmap.height()
    );
    if page.images_loaded > 0 {
        message.push_str(&format!(", {} image(s)", page.images_loaded));
    }
    if page.content_height.ceil() as u32 > page.pixmap.height() {
        message.push_str(&format!(
            "\nnote: page is {}px tall; output was clipped to --height",
            page.content_height.ceil() as u32
        ));
    }
    // ADR-0009 forbids switching rendering mode silently. With no chrome to
    // show a banner in, the CLI says it here.
    if let Some(explanation) = page.mode.explanation() {
        message.push('\n');
        message.push_str(&explanation);
    }
    // ADR-0006: plain HTTP is allowed but must never be presented as secure.
    if resource.origin.scheme == net::Scheme::Http {
        message.push_str(
            "\nnote: loaded over plain HTTP — not authenticated, and modifiable in transit",
        );
    }
    Ok(message)
}

/// Loads the target, accepting a URL or a bare filesystem path.
///
/// A bare path is a convenience, resolved to an absolute `file:` URL so that
/// everything downstream sees one representation and the policy has an origin
/// to judge subresources against.
fn load(input: &str) -> Result<net::Resource, String> {
    load_from(input).map(|(resource, _)| resource)
}

/// Fetches, and reports the absolute URL it settled on.
///
/// The window needs that URL: it is the first history entry, and every
/// relative link on the page is resolved against it.
fn load_from(input: &str) -> Result<(net::Resource, String), String> {
    let fetcher = net::Fetcher::default();
    let url = absolute_url(input)?;
    let resource = fetcher
        .fetch(&url, None, net::RequestKind::Navigation)
        .map_err(|error| format!("{url}: {error}"))?;
    Ok((resource, url))
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
