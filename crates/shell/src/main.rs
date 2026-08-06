//! The 2kbrowser command line.
//!
//! M1 ships the headless path only: `render` turns a local HTML file into a
//! PNG. Networking and a window follow; this is the part reference tests and CI
//! depend on, and it is what makes the rest a thin shell over a tested core.

use std::process::ExitCode;

use shell::render;

use text::FontStore;

const USAGE: &str = "\
2kbrowser — a web browser without the slop

USAGE:
    2kbrowser render <file.html> [--out <file.png>] [--width <px>] [--height <px>]

OPTIONS:
    --out <path>     Where to write the PNG (default: page.png)
    --width <px>     Viewport width (default: 800)
    --height <px>    Maximum canvas height (default: 4000)

M1: renders local files. Networking and a window are not built yet — see
PLAN.md. This browser does not run JavaScript, by design (ADR-0003).";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("render") => match run_render(&args[1..]) {
            Ok(message) => {
                println!("{message}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
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

fn run_render(args: &[String]) -> Result<String, String> {
    let mut input = None;
    let mut output = "page.png".to_owned();
    let mut width = 800u32;
    let mut height = 4000u32;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--out" => {
                output = take(args, &mut index, "--out")?;
            }
            "--width" => {
                width = take(args, &mut index, "--width")?
                    .parse()
                    .map_err(|_| "--width must be a number".to_owned())?;
            }
            "--height" => {
                height = take(args, &mut index, "--height")?
                    .parse()
                    .map_err(|_| "--height must be a number".to_owned())?;
            }
            other if input.is_none() => input = Some(other.to_owned()),
            other => return Err(format!("unexpected argument `{other}`")),
        }
        index += 1;
    }

    let input = input.ok_or("no input file given")?;
    let html = std::fs::read_to_string(&input).map_err(|e| format!("{input}: {e}"))?;

    let mut fonts = FontStore::new();
    let page = render::render(&html, width, height, &mut fonts);
    page.pixmap
        .save_png(&output)
        .map_err(|e| format!("{output}: {e}"))?;

    let mut message = format!(
        "wrote {output} ({}x{})",
        page.pixmap.width(),
        page.pixmap.height()
    );
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
    Ok(message)
}

fn take(args: &[String], index: &mut usize, flag: &str) -> Result<String, String> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value"))
}
