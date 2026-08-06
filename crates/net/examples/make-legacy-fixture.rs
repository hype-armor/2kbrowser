//! Writes the reference tests' legacy-encoding fixture.
//!
//! The fixture is deliberately *not* UTF-8 — that is the whole point of it —
//! so it cannot be edited as an ordinary text file and would be unreviewable
//! checked in on its own. Generating it from source keeps the content in a
//! diff anyone can read.
//!
//! Run with `cargo run -p net --example make-legacy-fixture -- <path.html>`.

use encoding_rs::WINDOWS_1252;

/// Written as UTF-8 source here and encoded to windows-1252 on the way out.
const SOURCE: &str = r#"<!DOCTYPE html>
<html>
<head>
  <title>A page in windows-1252</title>
  <meta http-equiv="Content-Type" content="text/html; charset=iso-8859-1">
  <style>
    body { font-family: Georgia, serif; margin: 16px }
    .box { border: 1px solid #999999; padding: 10px; background: #f6f4ee }
  </style>
</head>
<body>
  <h2>Bytes that are not UTF-8</h2>

  <p class="box">Café, naïve, résumé — “curly quotes”, an em dash, and
  ‘single quotes’ too. Señor François Ångström measured 50° ± 2°, which cost
  him £40 or about €55.</p>

  <p>This page declares <code>iso-8859-1</code> and is in fact windows-1252,
  which is true of almost every page that declares it: the characters above
  the ASCII range that pages actually used — the curly quotes, the dashes, the
  euro sign — exist only in windows-1252. The encoding standard maps one label
  onto the other for exactly this reason.</p>

  <p>Read as UTF-8, every one of those characters is a replacement character
  and the page is unreadable. That is what most of the surviving old web looks
  like to a browser that assumes UTF-8.</p>
</body>
</html>
"#;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "legacy-encoding.html".to_owned());
    let (bytes, _, unmappable) = WINDOWS_1252.encode(SOURCE);
    assert!(
        !unmappable,
        "the fixture must be expressible in windows-1252, or it tests nothing"
    );
    std::fs::write(&path, &bytes).expect("write fixture");
    println!("wrote {path} ({} bytes)", bytes.len());
}
