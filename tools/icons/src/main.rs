//! Derives every icon the platforms want from one master image.
//!
//! ```text
//! cargo run -p icons
//! ```
//!
//! There is one source of truth — `assets/icon.png` — and everything else in
//! `assets/generated/` and `packaging/` is produced from it. Replacing the icon
//! is therefore a one-file change followed by a re-run, rather than an export
//! from a drawing program repeated at eight sizes and three container formats,
//! which is the kind of job that is done correctly once and approximately
//! forever after.
//!
//! The two container formats are written by hand here. `.ico` and `.icns` are
//! both a header and a list of PNGs, which is a few lines each; the crates that
//! do it would be two more dependencies for something this project can write
//! and test itself. Neither needs a platform tool, so the whole set is built on
//! any machine — which matters, because the macOS icon has to be producible by
//! somebody who does not own a Mac.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::imageops::FilterType;
use image::{ImageFormat, RgbaImage};

/// Sizes the freedesktop icon theme wants, in pixels.
///
/// 22 and 24 are not powers of two and are not mistakes: they are the panel and
/// toolbar sizes in the hicolor spec, and leaving them out means a desktop
/// scales 16 up or 32 down and shows a blurred icon in the one place a user
/// looks most often.
const HICOLOR_SIZES: [u32; 8] = [16, 22, 24, 32, 48, 64, 128, 256];

/// Sizes stored in the Windows `.ico`.
///
/// 256 is the largest Explorer reads, and it is stored as PNG rather than a
/// bitmap — which is what the format has allowed since Vista and what keeps the
/// file from being a megabyte of uncompressed BGRA.
const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

/// Sizes stored in the macOS `.icns`, with the four-character type each one is
/// filed under.
///
/// `icp4`/`icp5`/`icp6` and `ic07`..`ic10` all take PNG. The older `is32`/`il32`
/// types take a run-length-encoded bitmap and a separate mask, which nothing
/// since Snow Leopard needs.
const ICNS_ENTRIES: [(&[u8; 4], u32); 7] = [
    (b"icp4", 16),
    (b"icp5", 32),
    (b"icp6", 64),
    (b"ic07", 128),
    (b"ic08", 256),
    (b"ic09", 512),
    (b"ic10", 1024),
];

/// The window icon winit is handed at startup.
///
/// One size, because `Icon::from_rgba` takes one image and the compositor
/// scales from there. 256 is generous for a title bar and costs 12 KB in the
/// binary against a 20 MB budget.
const WINDOW_ICON_SIZE: u32 = 256;

fn main() -> std::process::ExitCode {
    match run() {
        Ok(count) => {
            println!("wrote {count} files from assets/icon.png");
            std::process::ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("icons: {message}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<usize, String> {
    let root = repository_root()?;
    let master_path = root.join("assets/icon.png");
    let master = image::open(&master_path)
        .map_err(|error| format!("{}: {error}", master_path.display()))?
        .into_rgba8();

    // A master smaller than the largest size asked for would be scaled up, and
    // an icon scaled up is a blurred icon that nobody notices until it is on a
    // dock. Refuse rather than produce one.
    let largest = ICNS_ENTRIES
        .iter()
        .map(|(_, size)| *size)
        .max()
        .unwrap_or(0);
    if master.width() < largest || master.height() < largest {
        return Err(format!(
            "master is {}x{} but {largest}x{largest} is needed; scaling up would blur every size below it",
            master.width(),
            master.height()
        ));
    }
    if master.width() != master.height() {
        return Err(format!(
            "master is {}x{} and an icon is square; a non-square master would be squashed rather than cropped",
            master.width(),
            master.height()
        ));
    }

    let mut written = 0;

    // The window icon, embedded by `crates/shell`.
    write_png(
        &root.join("assets/generated/window-256.png"),
        &resize(&master, WINDOW_ICON_SIZE),
    )?;
    written += 1;

    // The README's own copy, so the picture in the documentation is the icon
    // the program actually uses rather than one exported by hand once.
    write_png(
        &root.join("docs/images/icon.png"),
        &resize(&master, WINDOW_ICON_SIZE),
    )?;
    written += 1;

    // Linux: the hicolor theme, installed under share/icons.
    for size in HICOLOR_SIZES {
        let path = root.join(format!(
            "packaging/linux/hicolor/{size}x{size}/apps/2kbrowser.png"
        ));
        write_png(&path, &resize(&master, size))?;
        written += 1;
    }

    // Windows.
    let ico = encode_ico(&master)?;
    write_bytes(&root.join("packaging/windows/2kbrowser.ico"), &ico)?;
    written += 1;

    // macOS.
    let icns = encode_icns(&master)?;
    write_bytes(&root.join("packaging/macos/2kbrowser.icns"), &icns)?;
    written += 1;

    Ok(written)
}

/// Finds the repository root from this crate's manifest, so the tool can be run
/// from anywhere rather than only from the root.
fn repository_root() -> Result<PathBuf, String> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("no repository root above {}", manifest.display()))
}

/// Scales the master down to `size`.
///
/// Lanczos3 rather than the default: every size here is a reduction, and a
/// box filter on a reduction of 4x or more throws away detail that a good
/// resampler keeps — which at 16 pixels is the difference between a legible
/// shape and a smudge.
fn resize(master: &RgbaImage, size: u32) -> RgbaImage {
    image::imageops::resize(master, size, size, FilterType::Lanczos3)
}

fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .map_err(|error| format!("encoding png: {error}"))?;
    Ok(bytes)
}

/// Writes a Windows icon: a directory of entries, then the PNG for each.
fn encode_ico(master: &RgbaImage) -> Result<Vec<u8>, String> {
    let entries: Vec<(u32, Vec<u8>)> = ICO_SIZES
        .iter()
        .map(|&size| encode_png(&resize(master, size)).map(|png| (size, png)))
        .collect::<Result<_, _>>()?;

    let count = u16::try_from(entries.len()).map_err(|_| "too many icon sizes".to_owned())?;
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // 1 = icon, 2 = cursor
    out.extend_from_slice(&count.to_le_bytes());

    // Payloads start after the directory, so the offsets can be computed before
    // any of them is written.
    let mut offset = 6 + 16 * u32::from(count);
    for (size, png) in &entries {
        let length = u32::try_from(png.len()).map_err(|_| "icon too large".to_owned())?;
        // 0 means 256: the field is one byte and 256 does not fit in it.
        let dimension = u8::try_from(*size).unwrap_or(0);
        out.push(dimension);
        out.push(dimension);
        out.push(0); // palette size, 0 for a true-colour image
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += length;
    }
    for (_, png) in &entries {
        out.extend_from_slice(png);
    }
    Ok(out)
}

/// Writes a macOS icon: a magic, a total length, then a typed chunk per size.
fn encode_icns(master: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    for (kind, size) in ICNS_ENTRIES {
        let png = encode_png(&resize(master, size))?;
        // The length counts the 8-byte header as well as the payload, which is
        // the detail that makes a hand-written icns open or not open.
        let length = u32::try_from(png.len() + 8).map_err(|_| "icon too large".to_owned())?;
        body.extend_from_slice(kind);
        body.extend_from_slice(&length.to_be_bytes());
        body.extend_from_slice(&png);
    }

    let total = u32::try_from(body.len() + 8).map_err(|_| "icon set too large".to_owned())?;
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

fn write_png(path: &Path, image: &RgbaImage) -> Result<(), String> {
    write_bytes(path, &encode_png(image)?)
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    fs::write(path, bytes).map_err(|error| format!("{}: {error}", path.display()))
}
