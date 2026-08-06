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

OPTIONS:
    --out <path>     Where to write the PNG (render only; default: page.png)
    --width <px>     Viewport width (default: 800)
    --height <px>    Window height, or maximum canvas height for render

Accepts http:, https:, and file: URLs, or a plain path. Third-party requests
are refused by default (ADR-0006) and JavaScript is never run (ADR-0003).

In a window: arrows and PageUp/PageDown scroll, Home/End jump, Esc or q quits.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("render") => report(run_render(&args[1..])),
        Some("open") => report(run_open(&args[1..])),
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
    let resource = load(&input)?;
    window::open(
        resource.body,
        input,
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
    let fetcher = net::Fetcher::default();
    let url = if input.contains("://") {
        input.to_owned()
    } else {
        let path = std::path::Path::new(input);
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(path)
        };
        format!("file://{}", absolute.display())
    };

    fetcher
        .fetch(&url, None, net::RequestKind::Navigation)
        .map_err(|error| format!("{url}: {error}"))
}

fn take(args: &[String], index: &mut usize, flag: &str) -> Result<String, String> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value"))
}
