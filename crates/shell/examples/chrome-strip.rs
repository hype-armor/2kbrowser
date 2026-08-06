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

    let cases: Vec<chrome::State> = vec![
        chrome::State {
            url: "https://example.com/a-perfectly-ordinary-page.html",
            mode: &authored,
            error: None,
            can_go_back: false,
            can_go_forward: false,
            forcing_authored: false,
            can_toggle_layout: false,
        },
        chrome::State {
            url: "http://example.org/an-old-page.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            can_toggle_layout: false,
        },
        chrome::State {
            url: "file:///home/user/pages/index.html",
            mode: &authored,
            error: None,
            can_go_back: true,
            can_go_forward: true,
            forcing_authored: false,
            can_toggle_layout: false,
        },
        chrome::State {
            url: "https://example.com/something-modern",
            mode: &document,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            can_toggle_layout: true,
        },
        chrome::State {
            url: "https://example.com/app",
            mode: &scripting,
            error: None,
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: true,
            can_toggle_layout: true,
        },
        chrome::State {
            url: "https://example.com/gone.html",
            mode: &authored,
            error: Some("server returned 404"),
            can_go_back: true,
            can_go_forward: false,
            forcing_authored: false,
            can_toggle_layout: false,
        },
    ];

    let gap = 6u32;
    let height = cases.len() as u32 * (chrome::HEIGHT + gap);
    let mut sheet = paint::Pixmap::new(width, height).expect("sheet");
    sheet.fill(paint::RasterColor::from_rgba8(0x60, 0x60, 0x60, 0xff));

    let mut fonts = text::FontStore::new();
    for (index, state) in cases.iter().enumerate() {
        let bar = chrome::render(state, width, &mut fonts);
        sheet.draw_pixmap(
            0,
            (index as u32 * (chrome::HEIGHT + gap)) as i32,
            bar.as_ref(),
            &paint::PixmapPaint::default(),
            paint::Transform::identity(),
            None,
        );
    }
    sheet.save_png(&output).expect("write");
    println!("wrote {output} with {} states", cases.len());
}
