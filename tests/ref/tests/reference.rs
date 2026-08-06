//! Renders every fixture and compares it to its baseline, byte for byte.

use std::path::{Path, PathBuf};

use text::FontStore;

/// Viewport width for every fixture. Fixed so baselines are comparable.
const WIDTH: u32 = 600;
/// Canvas height cap, generous enough that no fixture is clipped.
const MAX_HEIGHT: u32 = 3000;

fn dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(name)
}

fn fixtures() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir("fixtures"))
        .expect("fixtures directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "html"))
        .collect();
    // Sorted so failures are reported in a stable order.
    paths.sort();
    paths
}

#[test]
fn fixtures_match_their_baselines() {
    let bless = std::env::var_os("BLESS").is_some();
    let baselines = dir("baselines");
    std::fs::create_dir_all(&baselines).expect("baselines directory");

    let mut fonts = FontStore::new();
    let mut failures = Vec::new();
    let paths = fixtures();
    assert!(!paths.is_empty(), "no fixtures found");

    for path in &paths {
        let name = path
            .file_stem()
            .expect("stem")
            .to_string_lossy()
            .to_string();
        // Read as bytes and decoded the way a fetched page is, so a fixture
        // may be in a legacy encoding — which is the only way to test that
        // path end to end.
        let bytes = std::fs::read(path).expect("fixture readable");
        let (html, _, _) = net::encoding::decode_document(&bytes, None);
        // Render with the fixture's own location as the base, so relative
        // subresource URLs resolve and image fixtures actually exercise images.
        let url = format!("file://{}", path.display());
        let (origin, base_path) = net::parse_url(&url).expect("fixture url");
        let page = shell::render::render_with_base(
            &html,
            WIDTH,
            MAX_HEIGHT,
            &mut fonts,
            Some((&origin, &base_path)),
        );
        let actual = page.pixmap.encode_png().expect("encode png");
        let baseline = baselines.join(format!("{name}.png"));

        if bless || !baseline.exists() {
            std::fs::write(&baseline, &actual).expect("write baseline");
            continue;
        }

        let expected = std::fs::read(&baseline).expect("read baseline");
        if expected != actual {
            let failed = baselines.join(format!("{name}.actual.png"));
            std::fs::write(&failed, &actual).expect("write actual");
            failures.push(format!(
                "{name}: differs from baseline ({} bytes vs {} bytes); wrote {}",
                actual.len(),
                expected.len(),
                failed.display()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} fixtures differ:\n  {}\n\nIf the change is intended, re-run with BLESS=1 and \
         review the new images before committing.",
        failures.len(),
        paths.len(),
        failures.join("\n  ")
    );
}

#[test]
fn rendering_is_stable_across_runs() {
    // Same input, same bytes — the property the single baseline set rests on.
    // A failure here means the baselines are meaningless, so it is worth
    // asserting directly rather than inferring from the comparison above.
    let html = std::fs::read_to_string(dir("fixtures").join("text.html")).expect("fixture");
    let mut fonts = FontStore::new();
    let first = shell::render::render(&html, WIDTH, MAX_HEIGHT, &mut fonts);

    // A second, independently constructed store: font loading order must not
    // affect output either.
    let mut other_fonts = FontStore::new();
    let second = shell::render::render(&html, WIDTH, MAX_HEIGHT, &mut other_fonts);

    assert_eq!(first.pixmap.data(), second.pixmap.data());
}
