//! Where the time goes between a reader doing something and the window showing
//! it (#207).
//!
//! The report that opened #207 said "lots of hanging ui issues" and named
//! nothing, which is the most honest form a performance report takes and the
//! least actionable. This is the answer to it: rather than guess which part is
//! slow, measure every operation the event loop performs synchronously and
//! print what each one costs.
//!
//! Synchronously is the whole point. The window is a single thread, and
//! anything it waits for is a frame it does not draw — so an operation's cost
//! here *is* how long the browser is unresponsive for when a reader asks for
//! it. A number over about 100ms is one a person notices; over 250ms is one
//! they call a hang.
//!
//! Run after a release build. A debug build measures the compiler's choices
//! rather than the browser's:
//!
//! ```text
//! cargo build --release
//! cargo run --release -p timings
//! ```
//!
//! Not a pass-or-fail harness, deliberately. `budgets` exists to hold a line
//! that has been agreed; this exists to find out where the line should be, and
//! a threshold invented before the first measurement would be a number nobody
//! had a reason for.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The browser binary, which is also the renderer child.
const BROWSER: &str = if cfg!(windows) {
    "2kbrowser.exe"
} else {
    "2kbrowser"
};

/// How many times each operation is repeated.
///
/// The slowest is reported rather than the mean. A reader does not experience
/// an average: they experience the time the window was not drawing, one
/// occasion at a time, and the occasion they complain about is the worst one.
const REPEATS: usize = 5;

/// Viewport the measurements are taken at.
const WIDTH: u32 = 1000;
const HEIGHT: u32 = 800;

fn main() -> std::process::ExitCode {
    let Some(browser) = browser() else {
        eprintln!("build the browser first: cargo build --release");
        return std::process::ExitCode::FAILURE;
    };

    // Each page with a word it really contains, so the find column measures
    // finding something rather than failing to.
    let mut pages = vec![
        ("an era page", era_page(), "the"),
        ("a long page", generated(&long_page(), "long.html"), "line"),
        ("a big table", generated(&big_table(), "table.html"), "r7c3"),
        ("deep nesting", generated(&deep_page(), "deep.html"), "text"),
    ];
    pages.retain(|(name, source, _)| {
        let kept = source.is_some();
        if !kept {
            eprintln!("skipping {name}: its fixture could not be written");
        }
        kept
    });

    println!(
        "{:<16}{:>10}{:>10}{:>10}{:>10}{:>10}{:>10}",
        "page", "open", "band", "keypress", "select", "find", "a11y"
    );
    println!("{}", "-".repeat(76));

    for (name, source, present) in &pages {
        let Some(source) = source else { continue };
        match measure(&browser, source, present) {
            Some(row) => println!(
                "{:<16}{:>10}{:>10}{:>10}{:>10}{:>10}{:>10}",
                name,
                ms(row.open),
                ms(row.band),
                ms(row.keypress),
                ms(row.select),
                ms(row.find),
                ms(row.accessibility),
            ),
            None => println!("{name:<16}{:>10}", "failed"),
        }
    }

    println!(
        "\nThe worst of {REPEATS} runs, in milliseconds, at {WIDTH}x{HEIGHT}. Every one of these \
         happens\non the thread that draws the window, so each is how long the browser \
         is frozen for\nwhen a reader asks for it."
    );
    std::process::ExitCode::SUCCESS
}

/// What one page cost, operation by operation.
struct Row {
    open: Duration,
    band: Duration,
    keypress: Duration,
    select: Duration,
    find: Duration,
    accessibility: Duration,
}

/// A word every fixture contains, so find has something to find.
///
/// Passed in rather than fixed. The first version searched for one word against
/// every page and two of them did not contain it — so two columns reported a
/// fast zero, which is what a *failure* looks like and not what a speed looks
/// like. The warning below caught it, which is the point of the warning.
fn measure(browser: &Path, source: &Path, present: &str) -> Option<Row> {
    let body = std::fs::read(source).ok()?;
    let url = net::file_url(source);
    let (origin, path) = net::parse_url(&url).ok()?;
    let renderer = sandbox::Renderer::with_program(browser.to_path_buf());
    let document = || shell::viewport::Document {
        body: body.clone(),
        content_type: None,
        origin: origin.clone(),
        path: path.clone(),
    };

    // Opening is measured on its own children, because each one is a fresh
    // process and that cost is part of what a navigation charges the reader.
    let open = worst(|| {
        let started = Instant::now();
        let page = shell::viewport::Viewport::open(
            &renderer,
            document(),
            WIDTH,
            HEIGHT,
            false,
            false,
            1.0,
        );
        let taken = started.elapsed();
        drop(page);
        taken
    });

    // Everything else against one live page, which is the situation it happens
    // in: the reader is looking at something and does a thing to it.
    let mut page =
        shell::viewport::Viewport::open(&renderer, document(), WIDTH, HEIGHT, false, false, 1.0)
            .ok()?;

    // A band: the cheap operation, and the one the others should be compared
    // against. Scrolling is meant to cost only pixels.
    let band = worst(|| {
        let started = Instant::now();
        let _ = page.request_band(0, 400, HEIGHT);
        while page.band_outstanding() && !page.accept_band() {
            std::hint::spin_loop();
        }
        started.elapsed()
    });

    // A keystroke. The child re-renders the whole page for one — parse,
    // cascade, layout, paint — because the document is not kept between
    // renders, so this is the operation most likely to be felt as a hang while
    // filling in a form.
    let keypress = worst(|| {
        let started = Instant::now();
        page.type_key(sandbox::message::Key::Insert("a".to_owned()));
        started.elapsed()
    });

    // A selection drag sends one of these *per pointer move*, so this number is
    // charged many times a second while a reader drags across a paragraph.
    //
    // What came back is checked, not just how long it took. An operation that
    // failed answers instantly, and a harness that reported a fast zero for one
    // would be saying the opposite of the truth.
    let mut selected = 0usize;
    let select = worst(|| {
        let started = Instant::now();
        let (rects, _) = page.select((10.0, 10.0), (900.0, 600.0));
        selected = rects.len();
        started.elapsed()
    });
    if selected == 0 {
        eprintln!(
            "warning: nothing was selected, so the select column is a failure and not a speed"
        );
    }

    // Find sends one per keystroke in the find field.
    let mut found = 0usize;
    let find = worst(|| {
        let started = Instant::now();
        found = page.find(present).len();
        started.elapsed()
    });
    if found == 0 {
        eprintln!(
            "warning: {}: `{present}` was not found, so its find column is a failure and not a \
             speed",
            source.display()
        );
    }

    // The accessibility tree, which crosses as data (ADR-0019) and is rebuilt
    // whenever something is listening.
    let mut nodes = 0usize;
    let accessibility = worst(|| {
        let started = Instant::now();
        nodes = page.accessibility().nodes.len();
        started.elapsed()
    });
    if nodes == 0 {
        eprintln!(
            "warning: {}: the accessibility tree came back empty, so its column is a failure",
            source.display()
        );
    }

    Some(Row {
        open,
        band,
        keypress,
        select,
        find,
        accessibility,
    })
}

/// The slowest of [`REPEATS`] runs.
fn worst(mut once: impl FnMut() -> Duration) -> Duration {
    (0..REPEATS).map(|_| once()).max().unwrap_or_default()
}

/// A duration as whole milliseconds, for the table.
fn ms(taken: Duration) -> String {
    format!("{}", taken.as_millis())
}

/// Where the browser is, beside this harness.
fn browser() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(BROWSER)))
        .filter(|path| path.exists())
}

/// The era fixture the reference tests and `budgets` both use.
fn era_page() -> Option<PathBuf> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../ref/fixtures/era-page.html")
        .canonicalize()
        .ok()
}

/// Writes a generated fixture somewhere temporary and hands back its path.
fn generated(html: &str, name: &str) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join("2kbrowser-timings");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(name);
    std::fs::write(&path, html).ok()?;
    Some(path)
}

/// A long article: the shape of a page somebody actually reads.
fn long_page() -> String {
    let paragraphs: String = (0..2000)
        .map(|n| {
            format!(
                "<p>Paragraph {n}, with enough words in it to wrap across more \
                 than one line at any sensible width, which is what makes a \
                 line breaker do work.</p>"
            )
        })
        .collect();
    format!("<!doctype html><title>Long</title><body>{paragraphs}</body>")
}

/// A big table: the era's layout mechanism, and the one this engine does most
/// work for.
fn big_table() -> String {
    let rows: String = (0..400)
        .map(|row| {
            let cells: String = (0..12)
                .map(|column| format!("<td>r{row}c{column}</td>"))
                .collect();
            format!("<tr>{cells}</tr>")
        })
        .collect();
    format!(
        "<!doctype html><title>Table</title>\
         <body><table border=1 cellpadding=3>{rows}</table></body>"
    )
}

/// Deeply nested markup, which is what #179 is about.
///
/// Well short of the hundred thousand that issue uses: this harness is meant to
/// be run while somebody waits for it, and the point here is where the cost
/// starts to show rather than where it becomes absurd.
fn deep_page() -> String {
    let depth = 2000;
    let open = "<div>".repeat(depth);
    let close = "</div>".repeat(depth);
    format!("<!doctype html><title>Deep</title><body>{open}text{close}</body>")
}
