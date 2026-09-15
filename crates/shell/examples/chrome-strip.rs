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
    // A page that classified as authored and was handed the fallback anyway.
    // Classification still ran, and none of this page wanted newer layout —
    // reporting zero is the honest answer rather than an awkward one
    // (ADR-0009).
    let asked_for = RenderMode::Document {
        unsupported_share: 0.0,
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

    // What ADR-0006 refused a page, which until #118 the bar never mentioned.
    let one_host = ["fonts.example.net".to_owned()];
    let already = ["fonts.example.net".to_owned()];
    let several_hosts = [
        "fonts.example.net".to_owned(),
        "ads.example.org".to_owned(),
        "beacon.example.com".to_owned(),
    ];

    let cases: Vec<chrome::State> = vec![
        chrome::State {
            theme: chrome::Theme::LIGHT,
            url: "https://example.com/a-perfectly-ordinary-page.html",
            mode: &authored,
            error: None,
            can_go_back: false,
            can_go_forward: false,
            forcing_authored: false,
            forcing_document: false,
            can_toggle_layout: false,
            editing: None,
            finding: None,
            saved: false,
            local_root: false,
            withheld: 0,
            withheld_hosts: &[],
            allowed_hosts: &[],
        },
        chrome::State {
            theme: chrome::Theme::LIGHT,
            url: "http://example.org/an-old-page.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            forcing_document: false,
            can_toggle_layout: false,
            editing: None,
            finding: None,
            saved: true,
            local_root: false,
            withheld: 0,
            withheld_hosts: &[],
            allowed_hosts: &[],
        },
        // A connection an intercepting proxy signed. Marked, because trusting
        // this computer's roots silently would make it look ordinary.
        chrome::State {
            theme: chrome::Theme::LIGHT,
            url: "https://example.com/behind-a-proxy.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            forcing_document: false,
            can_toggle_layout: false,
            editing: None,
            finding: None,
            saved: false,
            local_root: true,
            withheld: 0,
            withheld_hosts: &[],
            allowed_hosts: &[],
        },
        chrome::State {
            theme: chrome::Theme::LIGHT,
            url: "file:///home/user/pages/index.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: true,
            forcing_authored: false,
            forcing_document: false,
            can_toggle_layout: false,
            editing: None,
            finding: None,
            saved: false,
            local_root: false,
            withheld: 0,
            withheld_hosts: &[],
            allowed_hosts: &[],
        },
        chrome::State {
            theme: chrome::Theme::LIGHT,
            url: "https://example.com/something-modern",
            mode: &document,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            forcing_document: false,
            can_toggle_layout: true,
            editing: None,
            finding: None,
            saved: true,
            local_root: false,
            withheld: 0,
            withheld_hosts: &[],
            allowed_hosts: &[],
        },
        // The other direction: an ordinary page the reader asked to simplify.
        // Not the absence of the state above — that one is a fallback being
        // overruled, and this one is a fallback being asked for by a page that
        // had none to overrule.
        chrome::State {
            theme: chrome::Theme::LIGHT,
            url: "https://example.com/a-busy-but-working-page.html",
            mode: &asked_for,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            forcing_document: true,
            can_toggle_layout: true,
            editing: None,
            finding: None,
            saved: false,
            local_root: false,
            withheld: 0,
            withheld_hosts: &[],
            allowed_hosts: &[],
        },
        chrome::State {
            theme: chrome::Theme::LIGHT,
            url: "https://example.com/app",
            mode: &scripting,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: true,
            forcing_document: false,
            can_toggle_layout: true,
            editing: None,
            finding: None,
            saved: false,
            local_root: false,
            withheld: 0,
            withheld_hosts: &[],
            allowed_hosts: &[],
        },
        chrome::State {
            theme: chrome::Theme::LIGHT,
            url: "https://example.com/gone.html",
            mode: &authored,
            error: Some("server returned 404"),
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            forcing_document: false,
            can_toggle_layout: false,
            editing: None,
            finding: None,
            saved: false,
            local_root: false,
            withheld: 0,
            withheld_hosts: &[],
            allowed_hosts: &[],
        },
        chrome::State {
            theme: chrome::Theme::LIGHT,
            url: "https://example.com/a-page.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            forcing_document: false,
            can_toggle_layout: false,
            editing: Some(&selected),
            finding: None,
            saved: false,
            local_root: false,
            withheld: 0,
            withheld_hosts: &[],
            allowed_hosts: &[],
        },
        chrome::State {
            theme: chrome::Theme::LIGHT,
            url: "https://example.com/",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            forcing_document: false,
            can_toggle_layout: false,
            editing: Some(&typing),
            finding: None,
            saved: false,
            local_root: false,
            withheld: 0,
            withheld_hosts: &[],
            allowed_hosts: &[],
        },
        chrome::State {
            theme: chrome::Theme::LIGHT,
            url: "https://example.com/index.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            forcing_document: false,
            can_toggle_layout: false,
            editing: Some(&partial),
            finding: None,
            saved: false,
            local_root: false,
            withheld: 0,
            withheld_hosts: &[],
            allowed_hosts: &[],
        },
        chrome::State {
            theme: chrome::Theme::LIGHT,
            url: "https://example.com/index.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            forcing_document: false,
            can_toggle_layout: false,
            editing: None,
            finding: Some((&searching, 2, 7)),
            saved: false,
            local_root: false,
            withheld: 0,
            withheld_hosts: &[],
            allowed_hosts: &[],
        },
        chrome::State {
            theme: chrome::Theme::LIGHT,
            url: "https://example.com/index.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            forcing_document: false,
            can_toggle_layout: false,
            editing: None,
            finding: Some((&fruitless, 0, 0)),
            saved: false,
            local_root: false,
            withheld: 0,
            withheld_hosts: &[],
            allowed_hosts: &[],
        },
        // What the policy refused, on its own and stacked with something else
        // the bar already had to say. The second row is the case worth looking
        // at: two facts of different kinds sharing one line, neither of which
        // may be dropped to make room for the other.
        chrome::State {
            theme: chrome::Theme::LIGHT,
            url: "https://example.com/a-page-with-a-font-cdn.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            forcing_document: false,
            can_toggle_layout: false,
            editing: None,
            finding: None,
            saved: false,
            local_root: false,
            withheld: 1,
            withheld_hosts: &one_host,
            allowed_hosts: &already,
        },
        chrome::State {
            theme: chrome::Theme::LIGHT,
            url: "http://example.org/an-old-page-with-trackers.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            forcing_document: false,
            can_toggle_layout: false,
            editing: None,
            finding: None,
            saved: false,
            local_root: false,
            withheld: 11,
            withheld_hosts: &several_hosts,
            allowed_hosts: &[],
        },
        // The dark scheme, which until now this sheet did not draw at all — so
        // "every state the bar can be in" was every state of one of the two
        // themes. That was survivable while the schemes differed only in the
        // colours behind text, and stopped being when the controls gained a
        // surface of their own: a `Theme` colour given a real value for one
        // scheme and a guess for the other passes every test and is wrong on
        // screen, which is the failure this sheet exists to catch.
        //
        // Two rows, because the thing worth looking at is the difference
        // between them — a control with a surface and one without.
        chrome::State {
            theme: chrome::Theme::DARK,
            url: "https://example.com/a-perfectly-ordinary-page.html",
            mode: &authored,
            error: None,
            can_go_back: false,
            can_go_forward: false,
            forcing_authored: false,
            forcing_document: false,
            can_toggle_layout: false,
            editing: None,
            finding: None,
            saved: false,
            local_root: false,
            withheld: 0,
            withheld_hosts: &[],
            allowed_hosts: &[],
        },
        chrome::State {
            theme: chrome::Theme::DARK,
            url: "http://example.org/an-old-page.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: true,
            forcing_authored: false,
            forcing_document: false,
            can_toggle_layout: false,
            editing: None,
            finding: None,
            saved: true,
            local_root: false,
            withheld: 0,
            withheld_hosts: &[],
            allowed_hosts: &[],
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

    // The site panel the padlock opens, in both schemes. It is chrome, it is
    // drawn by the same rasteriser, and it is the one piece of this browser's
    // interface that changes what the network policy does — which makes
    // looking at it rather more than a nicety (#118).
    let panels: Vec<(chrome::Theme, Option<usize>)> =
        vec![(chrome::Theme::LIGHT, None), (chrome::Theme::DARK, Some(2))];
    let panel_rows = shell::site_panel::rows_for(
        "example.com",
        &[
            "ads.example.org".to_owned(),
            "beacon.example.com".to_owned(),
        ],
        &["fonts.example.net".to_owned()],
    );
    let panel_height: f32 =
        shell::site_panel::Panel::open((0.0, 0.0), panel_rows.clone(), (900, 900))
            .expect("opens")
            .rect()
            .height;

    let gap = 6u32;
    let height = cases.len() as u32 * (chrome::HEIGHT + gap)
        + strips.len() as u32 * (chrome::TAB_HEIGHT + gap)
        + panels.len() as u32 * (panel_height as u32 + gap);
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
        let strip = chrome::render_tabs(labels, *active, width, &mut fonts, chrome::Theme::LIGHT);
        place(&mut sheet, &strip, &mut y);
    }

    for (theme, hovered) in &panels {
        let mut panel = shell::site_panel::Panel::open((0.0, 0.0), panel_rows.clone(), (900, 900))
            .expect("opens");
        panel.hovered = *hovered;
        place(&mut sheet, &panel.render(&mut fonts, *theme), &mut y);
    }

    sheet.save_png(&output).expect("write");
    println!(
        "wrote {output} with {} bar states, {} strips and {} panels",
        cases.len(),
        strips.len(),
        panels.len()
    );
}
