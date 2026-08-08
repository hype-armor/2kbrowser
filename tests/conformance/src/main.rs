//! Running the CSS 2.1 test suite against this engine.
//!
//! PLAN.md §2 rests the whole scope boundary on one fact: CSS 2.1 is a
//! *completed* specification with an official test suite, so unlike an engine
//! chasing the modern web this one has a finish line. That argument had never
//! been checked against the suite it names. This is the check.
//!
//! # What it runs
//!
//! Reference tests only — the ones carrying `<link rel="match">`. A reftest
//! says "these two documents must render identically", which is a question this
//! engine can answer without a human looking: render both, compare pixels. The
//! rest of the suite is self-describing prose ("the test passes if there is a
//! green square and no red"), which needs eyes.
//!
//! The suite is not vendored. It is 84 MB of someone else's repository, it
//! changes on its own schedule, and pinning a copy here would turn a
//! measurement into a fossil. Point this at a checkout:
//!
//! ```text
//! git clone --filter=blob:none --sparse --depth 1 \
//!     https://github.com/web-platform-tests/wpt
//! git -C wpt sparse-checkout set css/CSS2
//! cargo run --profile conformance -p conformance -- wpt/css/CSS2
//! ```
//!
//! # What a pass means, and what it does not
//!
//! A reftest passes when the test and its reference render the same. That is
//! weaker than "renders correctly", and the gap is worth stating plainly: an
//! engine that ignores a property entirely will often render both sides
//! identically and pass. Reftests catch *inconsistency*, not absence. So the
//! number this prints is an upper bound on conformance, not a measure of it —
//! useful for finding what is broken, useless as a boast.
//!
//! Tests needing something no headless renderer can supply are skipped rather
//! than failed, and counted, because a check that cannot run must not look like
//! one that passed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use text::FontStore;

/// Viewport the suite is written against.
///
/// The CSS 2.1 tests assume a 800x600 window. Rendering them narrower makes
/// tests fail on wrapping that has nothing to do with what they check.
const WIDTH: u32 = 800;

/// Tall enough that a test's content is not cut off, since a page cut at
/// different points on each side would compare unequal for the wrong reason.
const MAX_HEIGHT: u32 = 3000;

/// Flags marking a test no headless image comparison can perform.
///
/// Skipped and counted, never failed. These are not engine gaps: `interact`
/// needs a person, `paged` needs a printer, `userstyle` needs a user
/// stylesheet, `dom` and `history` need scripting this browser does not have by
/// design (ADR-0003).
const UNRUNNABLE: &[&str] = &[
    "interact",
    "paged",
    "animated",
    "userstyle",
    "history",
    "dom",
    "scroll",
    // Needs the Ahem test font, which this engine cannot supply: fonts are
    // compiled into the binary and never loaded from a document (ADR-0010).
    //
    // Worth being exact about, because it was the single largest distortion in
    // the first run of this. Ahem's glyphs are solid blocks, so the suite uses
    // it to draw a shape whose size is known exactly — a typical test renders
    // `X` at `100px/1 Ahem` and matches it against a 100x100 coloured `div`.
    // Without the font that is not a colour test we fail, it is a test we
    // cannot perform: the two sides are a letter and a square. Counting those
    // as conformance failures put ~900 of them into the number and said
    // something about our font loading rather than about our CSS.
    "ahem",
];

/// Flags marking a test whose failure is not a spec violation.
///
/// `may` tests optional behaviour, and `svg` needs a format outside this
/// engine's scope entirely (ADR-0004). Both are run, and counted apart, so the
/// headline number is neither flattered by skipping them nor distorted by
/// counting them as breakage.
const NOT_A_VIOLATION: &[&str] = &["may", "svg"];

fn main() -> std::process::ExitCode {
    let root = match std::env::args().nth(1) {
        Some(path) => PathBuf::from(path),
        None => {
            eprintln!(
                "usage: conformance <path to a wpt css/CSS2 checkout>\n\n\
                 The suite is not vendored — see this file's header for how to \
                 fetch one."
            );
            return std::process::ExitCode::FAILURE;
        }
    };
    if !root.is_dir() {
        eprintln!("SKIP: {} is not a directory", root.display());
        return std::process::ExitCode::SUCCESS;
    }

    let mut tests = Vec::new();
    collect(&root, &mut tests);
    tests.sort();
    if tests.is_empty() {
        eprintln!("SKIP: no test files under {}", root.display());
        return std::process::ExitCode::SUCCESS;
    }
    eprintln!("{} candidate files under {}", tests.len(), root.display());

    let mut fonts = FontStore::new();
    let mut chapters: BTreeMap<String, Tally> = BTreeMap::new();
    let mut totals = Tally::default();
    let mut failures: Vec<String> = Vec::new();

    for path in &tests {
        let Some(source) = read(path) else {
            continue;
        };
        let document = dom::parse(&source);
        let Some(reference) = match_link(&document) else {
            // Not a reftest: self-describing prose, which needs a person.
            continue;
        };
        let flags = flags(&document);

        let chapter = chapter_of(&root, path);
        let tally = chapters.entry(chapter.clone()).or_default();
        totals.considered += 1;
        tally.considered += 1;

        if flags.iter().any(|flag| UNRUNNABLE.contains(&flag.as_str())) {
            totals.unrunnable += 1;
            tally.unrunnable += 1;
            continue;
        }

        let Some(reference) = resolve(path, &reference) else {
            totals.broken += 1;
            tally.broken += 1;
            failures.push(format!(
                "{}: reference {reference} not found",
                show(&root, path)
            ));
            continue;
        };

        let excused = flags
            .iter()
            .any(|flag| NOT_A_VIOLATION.contains(&flag.as_str()));

        match compare(path, &reference, &mut fonts) {
            Outcome::Same => {
                totals.passed += 1;
                tally.passed += 1;
            }
            Outcome::Different(why) => {
                if excused {
                    totals.excused += 1;
                    tally.excused += 1;
                } else {
                    totals.failed += 1;
                    tally.failed += 1;
                    failures.push(format!("{}: {why}", show(&root, path)));
                }
            }
            Outcome::Crashed => {
                totals.crashed += 1;
                tally.crashed += 1;
                failures.push(format!("{}: PANICKED", show(&root, path)));
            }
        }
    }

    report(&chapters, &totals, &failures);
    std::process::ExitCode::SUCCESS
}

/// What happened to one test.
enum Outcome {
    Same,
    Different(String),
    Crashed,
}

/// Counts for one chapter, or for the run.
#[derive(Debug, Default)]
struct Tally {
    considered: usize,
    passed: usize,
    failed: usize,
    /// Failed, but carrying a flag that makes the failure not a violation.
    excused: usize,
    /// Panicked. Counted apart from a failure because a crash on a well-formed
    /// document is a different kind of bug from rendering it wrongly.
    crashed: usize,
    unrunnable: usize,
    /// The reference named by the test is missing from the checkout.
    broken: usize,
}

impl Tally {
    /// Tests that actually ran and could have gone either way.
    fn ran(&self) -> usize {
        self.passed + self.failed + self.excused + self.crashed
    }
}

/// Renders both sides and compares them pixel for pixel.
fn compare(test: &Path, reference: &Path, fonts: &mut FontStore) -> Outcome {
    // `catch_unwind` because a panic on one test must not end the run — and
    // because a panic *is* a finding, recorded rather than swallowed. It needs
    // the unwinding profile; see the `conformance` profile in the workspace
    // manifest, which exists for this.
    let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let left = render(test, fonts)?;
        let right = render(reference, fonts)?;
        Some((left, right))
    }));

    match rendered {
        Ok(Some((left, right))) => {
            if left == right {
                Outcome::Same
            } else {
                Outcome::Different("renders differently from its reference".to_owned())
            }
        }
        Ok(None) => Outcome::Different("one side could not be read".to_owned()),
        Err(_) => Outcome::Crashed,
    }
}

/// One document, rendered to raw pixels.
fn render(path: &Path, fonts: &mut FontStore) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    let (html, ..) = net::encoding::decode_document(&bytes, None);
    // With the file's own location as the base, so the `support/` images and
    // stylesheets these tests lean on actually resolve.
    let url = net::file_url(path);
    let (origin, base) = net::parse_url(&url).ok()?;
    let page =
        shell::render::render_with_base(&html, WIDTH, MAX_HEIGHT, fonts, Some((&origin, &base)));
    Some(page.pixmap.data().to_vec())
}

/// The `href` of this document's `<link rel="match">`, if it has one.
fn match_link(document: &dom::Document) -> Option<String> {
    fn walk(document: &dom::Document, node: dom::NodeId, found: &mut Option<String>) {
        if found.is_some() {
            return;
        }
        if let Some(element) = document.element(node)
            && element.local_name() == "link"
            && element
                .attr("rel")
                .is_some_and(|rel| rel.eq_ignore_ascii_case("match"))
            && let Some(href) = element.attr("href")
        {
            *found = Some(href.to_owned());
            return;
        }
        for &child in document.children(node) {
            walk(document, child, found);
        }
    }
    let mut found = None;
    walk(document, document.root(), &mut found);
    found
}

/// The `<meta name="flags">` content, split into words.
fn flags(document: &dom::Document) -> Vec<String> {
    fn walk(document: &dom::Document, node: dom::NodeId, out: &mut Vec<String>) {
        if let Some(element) = document.element(node)
            && element.local_name() == "meta"
            && element
                .attr("name")
                .is_some_and(|name| name.eq_ignore_ascii_case("flags"))
            && let Some(content) = element.attr("content")
        {
            out.extend(content.split_whitespace().map(str::to_owned));
        }
        for &child in document.children(node) {
            walk(document, child, out);
        }
    }
    let mut out = Vec::new();
    walk(document, document.root(), &mut out);
    out
}

/// A reference's path, resolved against the test that names it.
///
/// Two forms appear. A relative `href` resolves against the test's own
/// directory, which is the obvious case. One beginning with `/` is
/// *server*-absolute — the suite is written to be served over HTTP, where `/`
/// is the checkout's root, not the filesystem's. Reading those as filesystem
/// paths is what made 268 references look missing in the first run; they were
/// all there, and all being looked for in the wrong place.
///
/// The root is found by walking up until the path resolves, rather than being
/// configured, so this works whether it is pointed at `css/CSS2` or at a whole
/// wpt checkout.
fn resolve(test: &Path, href: &str) -> Option<PathBuf> {
    let href = href.split(['?', '#']).next().unwrap_or(href);
    if let Some(absolute) = href.strip_prefix('/') {
        let mut directory = test.parent();
        while let Some(base) = directory {
            let candidate = base.join(absolute);
            if candidate.is_file() {
                return Some(candidate);
            }
            directory = base.parent();
        }
        return None;
    }
    let candidate = test.parent()?.join(href);
    candidate.is_file().then_some(candidate)
}

/// Every file that might be a test.
///
/// References are not filtered out here — a file with no `rel="match"` is
/// skipped later, and that covers references without having to guess from a
/// naming convention the suite does not enforce.
fn collect(directory: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // `support/` holds images and stylesheets, never tests.
            if path.file_name().is_some_and(|name| name == "support") {
                continue;
            }
            collect(&path, out);
        } else if path
            .extension()
            .is_some_and(|ext| matches!(&*ext.to_string_lossy(), "xht" | "html" | "htm" | "xhtml"))
        {
            out.push(path);
        }
    }
}

fn read(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let (text, ..) = net::encoding::decode_document(&bytes, None);
    Some(text)
}

/// Which chapter a test belongs to, taken from its directory.
fn chapter_of(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .and_then(|rest| rest.components().next())
        .map(|first| first.as_os_str().to_string_lossy().into_owned())
        .filter(|name| !name.ends_with(".xht") && !name.ends_with(".html"))
        .unwrap_or_else(|| "(root)".to_owned())
}

fn show(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn report(chapters: &BTreeMap<String, Tally>, totals: &Tally, failures: &[String]) {
    println!();
    println!(
        "{:<26} {:>6} {:>6} {:>6} {:>7} {:>7} {:>6}",
        "CHAPTER", "RAN", "PASS", "FAIL", "CRASH", "EXCUSED", "SKIP"
    );
    for (chapter, tally) in chapters {
        if tally.ran() == 0 && tally.unrunnable == 0 {
            continue;
        }
        println!(
            "{:<26} {:>6} {:>6} {:>6} {:>7} {:>7} {:>6}",
            chapter,
            tally.ran(),
            tally.passed,
            tally.failed,
            tally.crashed,
            tally.excused,
            tally.unrunnable,
        );
    }
    println!();
    println!(
        "{:<26} {:>6} {:>6} {:>6} {:>7} {:>7} {:>6}",
        "TOTAL",
        totals.ran(),
        totals.passed,
        totals.failed,
        totals.crashed,
        totals.excused,
        totals.unrunnable,
    );

    if totals.ran() > 0 {
        let share = 100.0 * totals.passed as f32 / totals.ran() as f32;
        println!();
        println!("{share:.1}% of the reference tests that ran render as their reference does.");
        println!(
            "That is an upper bound. A reftest passes when both sides look the same, and an\n\
             engine that ignores a property draws both sides the same way — so this counts\n\
             what is inconsistent, not what is absent."
        );
    }
    if totals.broken > 0 {
        println!(
            "\n{} tests name a reference that is not in the checkout.",
            totals.broken
        );
    }

    // The list, not just the count: a number nobody can act on is not a
    // measurement, it is a score.
    let listing = std::path::Path::new("conformance-failures.txt");
    let body = failures.join("\n");
    match std::fs::write(listing, format!("{body}\n")) {
        Ok(()) => println!(
            "\n{} failures listed in {}",
            failures.len(),
            listing.display()
        ),
        Err(error) => eprintln!("could not write {}: {error}", listing.display()),
    }
}
