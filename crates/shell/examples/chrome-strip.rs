//! Draws the chrome bar in each of its states, stacked. A debugging aid.
//!
//! The bar is the one part of the browser whose job is to be *read*, and
//! reading it is the only way to tell whether it says what it should.
//!
//! Run with `cargo run -p shell --example chrome-strip -- [out.png]`.

use layout::RenderMode;
use shell::chrome;

fn main() {
    let output = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "chrome-strip.png".to_owned());
    let width = 700u32;

    let authored = RenderMode::Authored;
    let document = RenderMode::Document {
        unsupported_share: 0.87,
    };
    let scripting = RenderMode::RequiresScripting;

    // Three editing states, so the caret and selection are visible.
    let selected = shell::field::Field::with_all_selected("https://example.com/a-page.html");
    let mut typing = shell::field::Field::with_all_selected("https://example.com/");
    typing.insert("example.org/new");
    let mut partial = shell::field::Field::with_all_selected("https://example.com/index.html");
    partial.end(false);
    partial.word_left(true);

    let mut searching = shell::field::Field::with_all_selected("");
    searching.insert("tables");
    let mut fruitless = shell::field::Field::with_all_selected("");
    fruitless.insert("nothing here");

    let cases: Vec<chrome::State> = vec![
        chrome::State {
            url: "https://example.com/a-perfectly-ordinary-page.html",
            mode: &authored,
            error: None,
            can_go_back: false,
            can_go_forward: false,
            forcing_authored: false,
            can_toggle_layout: false,
            editing: None,
            finding: None,
            saved: false,
            local_root: false,
        },
        chrome::State {
            url: "http://example.org/an-old-page.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            can_toggle_layout: false,
            editing: None,
            finding: None,
            saved: true,
            local_root: false,
        },
        // A connection an intercepting proxy signed. Marked, because trusting
        // this computer's roots silently would make it look ordinary.
        chrome::State {
            url: "https://example.com/behind-a-proxy.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            can_toggle_layout: false,
            editing: None,
            finding: None,
            saved: false,
            local_root: true,
        },
        chrome::State {
            url: "file:///home/user/pages/index.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: true,
            forcing_authored: false,
            can_toggle_layout: false,
            editing: None,
            finding: None,
            saved: false,
            local_root: false,
        },
        chrome::State {
            url: "https://example.com/something-modern",
            mode: &document,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            can_toggle_layout: true,
            editing: None,
            finding: None,
            saved: true,
            local_root: false,
        },
        chrome::State {
            url: "https://example.com/app",
            mode: &scripting,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: true,
            can_toggle_layout: true,
            editing: None,
            finding: None,
            saved: false,
            local_root: false,
        },
        chrome::State {
            url: "https://example.com/gone.html",
            mode: &authored,
            error: Some("server returned 404"),
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            can_toggle_layout: false,
            editing: None,
            finding: None,
            saved: false,
            local_root: false,
        },
        chrome::State {
            url: "https://example.com/a-page.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            can_toggle_layout: false,
            editing: Some(&selected),
            finding: None,
            saved: false,
            local_root: false,
        },
        chrome::State {
            url: "https://example.com/",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            can_toggle_layout: false,
            editing: Some(&typing),
            finding: None,
            saved: false,
            local_root: false,
        },
        chrome::State {
            url: "https://example.com/index.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            can_toggle_layout: false,
            editing: Some(&partial),
            finding: None,
            saved: false,
            local_root: false,
        },
        chrome::State {
            url: "https://example.com/index.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            can_toggle_layout: false,
            editing: None,
            finding: Some((&searching, 2, 7)),
            saved: false,
            local_root: false,
        },
        chrome::State {
            url: "https://example.com/index.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            can_toggle_layout: false,
            editing: None,
            finding: Some((&fruitless, 0, 0)),
            saved: false,
            local_root: false,
        },
    ];

    // Tab strips, drawn below the bars.
    let strips: Vec<(Vec<&str>, usize)> = vec![
        (vec!["The Node & Nib", "Archive"], 0),
        (
            vec!["The Node & Nib", "Archive", "A page with a very long title"],
            2,
        ),
        (
            vec![
                "one", "two", "three", "four", "five", "six", "seven", "eight",
            ],
            4,
        ),
    ];

    let gap = 6u32;
    let height = cases.len() as u32 * (chrome::HEIGHT + gap)
        + strips.len() as u32 * (chrome::TAB_HEIGHT + gap);
    let mut sheet = paint::Pixmap::new(width, height).expect("sheet");
    sheet.fill(paint::RasterColor::from_rgba8(0x60, 0x60, 0x60, 0xff));

    let mut fonts = text::FontStore::new();
    let mut y = 0i32;
    let place = |sheet: &mut paint::Pixmap, image: &paint::Pixmap, y: &mut i32| {
        sheet.draw_pixmap(
            0,
            *y,
            image.as_ref(),
            &paint::PixmapPaint::default(),
            paint::Transform::identity(),
            None,
        );
        *y += image.height() as i32 + gap as i32;
    };

    for state in &cases {
        let bar = chrome::render(state, width, &mut fonts);
        place(&mut sheet, &bar, &mut y);
    }
    for (labels, active) in &strips {
        let strip = chrome::render_tabs(labels, *active, width, &mut fonts);
        place(&mut sheet, &strip, &mut y);
    }

    sheet.save_png(&output).expect("write");
    println!(
        "wrote {output} with {} bar states and {} strips",
        cases.len(),
        strips.len()
    );
}
