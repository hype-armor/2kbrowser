//! What this engine actually honours, checked rather than remembered.
//!
//! README.md and PLAN.md both carry a list of what is missing or wrong, and the
//! value this project puts on that list is that it is *honest*. It was not.
//! Twenty-odd entries were missing from it, and they had been missing for as
//! long as the properties had — because nothing checked, and a gap list nobody
//! checks decays into "the things we happened to notice".
//!
//! # Why neither existing harness could find them
//!
//! Not for want of trying, and this is the part worth understanding before
//! adding a third harness to a repository that already has two good ones.
//!
//! The **CSS 2.1 conformance suite** is blind to this class by construction. A
//! reftest passes when its test and its reference render alike, so a property
//! this engine ignores makes *both* sides render the same way and the test
//! passes. It is an upper bound, as the README says, and this is the mechanism.
//!
//! The **reference tests** are blind to whatever nobody wrote a fixture for.
//! They are excellent at catching a rendering that *changed* and cannot say a
//! word about one that was never there. `<caption>` drew nothing for the whole
//! life of the table code and no baseline noticed, because no fixture had one.
//!
//! Both are good at regressions. Neither can find an absence.
//!
//! # What this does instead
//!
//! For a property: render two pages differing by exactly one declaration and
//! ask whether the engine drew anything different. If not, the property is
//! ignored — no browser needed and no judgement involved, because a declaration
//! that changes no pixel changed nothing.
//!
//! For an element: render it alone and ask whether any ink reached the canvas.
//!
//! Each case then carries the answer we expect, and a mismatch fails. That is
//! the whole point: implementing a property flips its row and *makes* somebody
//! update the record, so the list in README.md cannot quietly go stale again.
//! A newly working property is good news and still a failure here, because the
//! documentation has just become untrue.
//!
//! The expectations were set by rendering each case beside headless Chromium
//! once, by hand. That comparison is not repeated here on purpose: it would
//! make a browser a build dependency, and the question "does this declaration
//! change anything" does not need one.

use text::FontStore;

/// Viewport width for every case. Fixed so a case's two renders are comparable.
const WIDTH: u32 = 400;

/// Canvas height cap, generous enough that no case is clipped.
const MAX_HEIGHT: u32 = 600;

/// Whether a declaration reaches the pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// The engine draws something different for it.
    Honoured,
    /// Parsed, perhaps cascaded, and then of no consequence.
    Ignored,
}

impl Verdict {
    fn name(self) -> &'static str {
        match self {
            Verdict::Honoured => "honoured",
            Verdict::Ignored => "ignored",
        }
    }
}

/// One property, and whether this engine does anything about it.
struct Property {
    /// What to call it in the report.
    name: &'static str,
    /// CSS present in *both* renders — the box, the width, the background that
    /// makes the effect visible at all.
    ///
    /// Getting this wrong is the trap this file exists to avoid. Put the
    /// scaffolding only in the variant and any difference proves that
    /// *something* in the variant mattered, not that the property did. The
    /// first run of this sweep did exactly that and called nine ignored
    /// properties working.
    scaffold: &'static str,
    /// The one declaration under test, present only in the second render.
    declaration: &'static str,
    /// Markup for both renders.
    body: &'static str,
    /// What we expect, and what README.md and PLAN.md say.
    expected: Verdict,
}

/// Whether an element adds anything to the page it is put on.
///
/// Measured against a control page rather than as absolute ink, and the
/// difference matters twice over. A page whose whole body is a `<script>` has
/// no content at all, so it is re-rendered as a document (ADR-0009) and comes
/// back a dark reader page — which absolute ink cannot tell apart from script
/// text leaking onto the canvas. Anchoring every case to a paragraph of real
/// content keeps the fallback out of it, and asks the sharper question anyway:
/// not "is anything on this canvas" but "did this element put it there".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ink {
    /// Something was drawn.
    Some,
    /// Nothing reached the canvas at all.
    None,
}

impl Ink {
    /// For the table, where the column is already headed.
    fn name(self) -> &'static str {
        match self {
            Ink::Some => "draws",
            Ink::None => "nothing",
        }
    }

    /// For a sentence, where "recorded as drawing draws" is not one.
    fn phrase(self) -> &'static str {
        match self {
            Ink::Some => "something",
            Ink::None => "nothing",
        }
    }
}

/// One element, rendered alone, and whether anything appears.
struct Element {
    name: &'static str,
    body: &'static str,
    expected: Ink,
}

/// Properties, and what the engine currently does with each.
///
/// A row marked `Ignored` is an entry in the README's list of what is missing.
/// A row marked `Honoured` is a claim that it works, which this checks.
const PROPERTIES: &[Property] = &[
    // --- honoured: these are the controls. If one of these ever reports
    // `ignored`, the harness has broken rather than the engine.
    Property {
        name: "color",
        scaffold: "",
        declaration: "#t { color: #cc0000 }",
        body: "<p id=t>coloured</p>",
        expected: Verdict::Honoured,
    },
    Property {
        name: "border-collapse",
        scaffold: "#t td { border: 4px solid black }",
        declaration: "#t { border-collapse: collapse }",
        body: "<table id=t><tr><td>a</td><td>b</td></tr></table>",
        expected: Verdict::Honoured,
    },
    Property {
        name: "caption-side",
        scaffold: "",
        declaration: "#t { caption-side: bottom }",
        body: "<table id=t><caption>cap</caption><tr><td>a</td></tr></table>",
        expected: Verdict::Honoured,
    },
    Property {
        name: "visibility",
        scaffold: "#t { background: #ccc }",
        declaration: "#t { visibility: hidden }",
        body: "<p id=t>hide me</p>",
        expected: Verdict::Honoured,
    },
    Property {
        name: "white-space: pre-line",
        scaffold: "",
        declaration: "#t { white-space: pre-line }",
        body: "<p id=t>one\ntwo\nthree</p>",
        expected: Verdict::Honoured,
    },
    Property {
        name: "max-width",
        scaffold: "#t { background: #ccc }",
        declaration: "#t { max-width: 60px }",
        body: "<div id=t>a line of text wide enough to be clamped</div>",
        expected: Verdict::Honoured,
    },
    Property {
        name: "float",
        scaffold: "#t { width: 60px; background: #ccc }",
        declaration: "#t { float: right }",
        body: "<div id=t>x</div><p>after</p>",
        expected: Verdict::Honoured,
    },
    // --- ignored: every one of these is named in README.md's gap list.
    // A row moving out of this block is a change to that list as well, which
    // is the whole reason the verdicts are recorded rather than printed.
    Property {
        name: "text-indent",
        scaffold: "#t { width: 240px }",
        declaration: "#t { text-indent: 60px }",
        body: "<p id=t>the first line of this paragraph should be indented</p>",
        expected: Verdict::Honoured,
    },
    Property {
        name: "letter-spacing",
        scaffold: "",
        declaration: "#t { letter-spacing: 6px }",
        body: "<p id=t>spaced letters</p>",
        expected: Verdict::Honoured,
    },
    Property {
        name: "word-spacing",
        scaffold: "",
        declaration: "#t { word-spacing: 20px }",
        body: "<p id=t>spaced words here</p>",
        expected: Verdict::Ignored,
    },
    Property {
        name: "text-transform",
        scaffold: "",
        declaration: "#t { text-transform: uppercase }",
        body: "<p id=t>make me shout</p>",
        expected: Verdict::Honoured,
    },
    Property {
        name: "font-variant",
        scaffold: "",
        declaration: "#t { font-variant: small-caps }",
        body: "<p id=t>small caps text</p>",
        expected: Verdict::Ignored,
    },
    Property {
        name: "outline",
        scaffold: "#t { width: 120px }",
        declaration: "#t { outline: 4px solid red }",
        body: "<p id=t>outlined</p>",
        expected: Verdict::Ignored,
    },
    Property {
        name: "font (shorthand)",
        scaffold: "#t { background: #ccc }",
        // The line height, because that is the half that went unnoticed: the
        // shorthand parsed as nothing at all, and a row set with
        // `font: 20px/1` came out short enough to show the background under
        // its own cells.
        declaration: "#t { font: 20px/3 serif }",
        body: "<div id=t>x</div>",
        expected: Verdict::Honoured,
    },
    Property {
        name: "font-variant",
        scaffold: "#t { background: #ccc; font-size: 20px }",
        declaration: "#t { font-variant: small-caps }",
        body: "<div id=t>small caps</div>",
        expected: Verdict::Ignored,
    },
    Property {
        name: "content on ::before",
        scaffold: "",
        declaration: "#t::before { content: \"GENERATED\" }",
        body: "<p id=t>own text</p>",
        expected: Verdict::Honoured,
    },
    Property {
        name: "content: counter()",
        scaffold: "",
        // Out of scope: there is no counter state. The whole declaration is
        // dropped rather than half-applied, so this must change nothing.
        declaration: "#t::before { content: counter(c) }",
        body: "<p id=t>own text</p>",
        expected: Verdict::Ignored,
    },
    Property {
        name: "min-height",
        scaffold: "#t { background: #ccc }",
        declaration: "#t { min-height: 120px }",
        body: "<div id=t>x</div>",
        expected: Verdict::Honoured,
    },
    Property {
        name: "max-height",
        scaffold: "#t { background: #ccc; overflow: hidden }",
        declaration: "#t { max-height: 10px }",
        body: "<div id=t>one<br>two<br>three<br>four</div>",
        expected: Verdict::Honoured,
    },
    Property {
        name: "text-align: justify",
        scaffold: "#t { width: 200px }",
        declaration: "#t { text-align: justify }",
        body: "<p id=t>a paragraph long enough that justification has some slack \
               to distribute across several lines of text</p>",
        expected: Verdict::Ignored,
    },
    Property {
        name: "list-style-position",
        scaffold: "#t { width: 200px }",
        declaration: "#t { list-style-position: inside }",
        body: "<ul id=t><li>an item long enough to wrap onto a second line</li></ul>",
        expected: Verdict::Ignored,
    },
    Property {
        name: "z-index",
        scaffold: "#a,#b { position: absolute; top: 0; left: 0; width: 80px; \
                   height: 80px } #a { background: red } #b { background: blue }",
        declaration: "#a { z-index: 2 } #b { z-index: 1 }",
        body: "<div id=a></div><div id=b></div>",
        expected: Verdict::Honoured,
    },
    Property {
        name: "clip",
        scaffold: "#t { position: absolute; background: red; width: 100px; height: 100px }",
        declaration: "#t { clip: rect(0,20px,20px,0) }",
        body: "<div id=t></div>",
        expected: Verdict::Ignored,
    },
    Property {
        name: "position: fixed",
        scaffold: "#t { position: absolute; top: 100px; left: 100px }",
        declaration: "#t { position: fixed }",
        body: "<p id=t>fixed</p><p>flow</p>",
        expected: Verdict::Ignored,
    },
    Property {
        name: "table-layout: fixed",
        scaffold: "#t { width: 300px } #t td { border: 1px solid }",
        declaration: "#t { table-layout: fixed }",
        body: "<table id=t><tr><td>a very long first cell indeed</td><td>b</td></tr></table>",
        expected: Verdict::Ignored,
    },
    Property {
        name: "empty-cells",
        scaffold: "#t { border-collapse: separate } #t td { border: 2px solid black }",
        declaration: "#t { empty-cells: hide }",
        body: "<table id=t><tr><td>a</td><td></td></tr></table>",
        expected: Verdict::Ignored,
    },
    Property {
        name: "border-spacing, second value",
        scaffold: "#t td { border: 1px solid } #t { border-spacing: 2px }",
        declaration: "#t { border-spacing: 2px 30px }",
        body: "<table id=t><tr><td>a</td></tr><tr><td>b</td></tr></table>",
        expected: Verdict::Ignored,
    },
    Property {
        name: "direction: rtl",
        scaffold: "",
        declaration: "#t { direction: rtl }",
        body: "<p id=t>abc def ghi</p>",
        expected: Verdict::Ignored,
    },
    Property {
        name: "generated content",
        scaffold: "",
        declaration: "#t:before { content: 'PREFIX ' }",
        body: "<p id=t>body text</p>",
        expected: Verdict::Honoured,
    },
    Property {
        name: "display: inline-block",
        scaffold: "",
        declaration: "#t { display: inline-block; width: 90px; height: 40px; background: #c00 }",
        body: "text <span id=t></span> text",
        expected: Verdict::Ignored,
    },
];

/// Elements, and whether anything of them reaches the canvas.
const ELEMENTS: &[Element] = &[
    // Controls, again: ordinary content that must draw.
    Element {
        name: "p",
        body: "<p>text</p>",
        expected: Ink::Some,
    },
    Element {
        name: "table cell",
        body: "<table><tr><td>text</td></tr></table>",
        expected: Ink::Some,
    },
    Element {
        name: "caption",
        body: "<table><caption>text</caption><tr><td>x</td></tr></table>",
        expected: Ink::Some,
    },
    // Content that must *not* draw, which is the other way to be wrong.
    Element {
        name: "script contents",
        body: "<script>var x = 'leaked';</script>",
        expected: Ink::None,
    },
    Element {
        name: "style contents",
        body: "<style>.x { color: red }</style>",
        expected: Ink::None,
    },
    Element {
        name: "comment",
        body: "<!-- leaked -->",
        expected: Ink::None,
    },
    Element {
        name: "title in body",
        body: "<title>leaked</title>",
        expected: Ink::None,
    },
    Element {
        name: "unselected option",
        body: "<select><option>shown</option><option>hidden</option></select>",
        expected: Ink::Some,
    },
    // Form controls. Each draws its chrome now — a border, a background, and
    // its label — though none of them can be typed into or submitted, which is
    // the line `layout::forms` draws and the README states.
    Element {
        name: "input[type=text]",
        body: "<input type=\"text\" value=\"x\">",
        expected: Ink::Some,
    },
    Element {
        name: "input[type=submit]",
        body: "<input type=\"submit\" value=\"Go\">",
        expected: Ink::Some,
    },
    Element {
        name: "input[type=checkbox]",
        body: "<input type=\"checkbox\" checked>",
        expected: Ink::Some,
    },
    Element {
        name: "input[type=radio]",
        body: "<input type=\"radio\" checked>",
        expected: Ink::Some,
    },
    Element {
        name: "textarea box",
        body: "<textarea></textarea>",
        expected: Ink::Some,
    },
    Element {
        name: "empty button",
        body: "<button></button>",
        expected: Ink::Some,
    },
    Element {
        name: "empty fieldset",
        body: "<fieldset></fieldset>",
        expected: Ink::Some,
    },
];

/// A page with the given stylesheet and body.
fn page(css: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><style>\n\
         body {{ margin: 8px; font-family: serif; font-size: 16px; \
         background: #ffffff }}\n{css}\n</style></head><body>{body}</body></html>"
    )
}

/// The canvas for one page.
fn render(fonts: &mut FontStore, html: &str) -> Vec<u8> {
    shell::render::render(html, WIDTH, MAX_HEIGHT, fonts)
        .pixmap
        .data()
        .to_vec()
}

/// Whether anything but the blank canvas was drawn.
fn inked(pixels: &[u8]) -> bool {
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .any(|px| px != &[255, 255, 255, 255])
}

fn main() -> std::process::ExitCode {
    let mut fonts = FontStore::new();
    let mut wrong = Vec::new();

    println!("{:<32} {:<10} ACTUAL", "PROPERTY", "RECORDED");
    for case in PROPERTIES {
        let base = render(&mut fonts, &page(case.scaffold, case.body));
        let variant = render(
            &mut fonts,
            &page(
                &format!("{}\n{}", case.scaffold, case.declaration),
                case.body,
            ),
        );
        let actual = if base == variant {
            Verdict::Ignored
        } else {
            Verdict::Honoured
        };
        println!(
            "{:<32} {:<10} {}",
            case.name,
            case.expected.name(),
            actual.name()
        );
        if actual != case.expected {
            wrong.push(format!(
                "{}: recorded as {}, but is {}",
                case.name,
                case.expected.name(),
                actual.name()
            ));
        }
    }

    // Every element sits after a paragraph of ordinary content, so the page is
    // never contentless and the document fallback never enters into it.
    //
    // The length is not arbitrary and a shorter one does not work. `classify`
    // sends any page carrying a `<script>` and fewer than `MIN_CONTENT_CHARS`
    // of visible text to the scripting fallback, which is right — that is an
    // empty single-page-app shell and there is nothing to render. But it means
    // a six-word anchor makes the `<script>` cases measure the fallback's dark
    // canvas instead of what the script contributed, which is exactly the
    // mistake this harness exists to stop other people making.
    const ANCHOR: &str = "<p>An anchor of ordinary prose, long enough that a page \
        carrying it is never mistaken for one with no content at all. The \
        classifier sends a short page with a script on it to the document \
        fallback, and this paragraph is what keeps every case below out of \
        that path and measuring what it means to measure.</p>";
    let control = render(&mut fonts, &page("", ANCHOR));
    assert!(
        inked(&control),
        "the control page drew nothing, so no element case below can mean anything"
    );

    println!("\n{:<32} {:<10} ACTUAL", "ELEMENT", "RECORDED");
    for case in ELEMENTS {
        let with = render(&mut fonts, &page("", &format!("{ANCHOR}{}", case.body)));
        let actual = if with == control {
            Ink::None
        } else {
            Ink::Some
        };
        println!(
            "{:<32} {:<10} {}",
            case.name,
            case.expected.name(),
            actual.name()
        );
        if actual != case.expected {
            wrong.push(format!(
                "{}: recorded as drawing {}, but draws {}",
                case.name,
                case.expected.phrase(),
                actual.phrase()
            ));
        }
    }

    let total = PROPERTIES.len() + ELEMENTS.len();
    if wrong.is_empty() {
        println!("\n{total} cases, all as recorded.");
        return std::process::ExitCode::SUCCESS;
    }

    println!("\n{} of {total} cases differ from the record:", wrong.len());
    for line in &wrong {
        println!("  {line}");
    }
    // A property that started working is good news and still a failure here.
    // The point of this harness is that the gap list cannot go stale quietly,
    // and it has just gone stale: README.md and PLAN.md say something that is
    // no longer true, and saying so is what this exists for.
    println!(
        "\nUpdate the row here and the gap lists in README.md and PLAN.md \
         together: something that began working is a change to all three, and \
         leaving the documentation behind is what this check exists to stop."
    );
    std::process::ExitCode::FAILURE
}
