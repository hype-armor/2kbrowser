//! Putting a picture on the reader's disk (#205).
//!
//! The browser could already fetch an image, decode it and draw it, and had
//! nowhere to put one — so a reader who wanted to keep a photograph had to open
//! it in something else. This is the missing half: the same fetch, the same
//! policy, and a file at the end of it.
//!
//! Deliberately small. There is no download manager, no queue, no resumption
//! and no progress bar, because there is no page here big enough to need one:
//! the era's pictures are tens of kilobytes, and the honest shape for saving
//! one is a function that either wrote a file or says why it did not.
//!
//! What it is careful about is the *name*. A filename comes out of a URL, and a
//! URL is written by a stranger — so every part of the name is rebuilt here
//! rather than trusted, and the result is placed in one directory that the
//! caller chose.

use std::path::{Path, PathBuf};

/// Where saved pictures go, when the caller does not say.
///
/// The platform's downloads directory where the reader has one, and the home
/// directory otherwise — resolved by hand rather than by pulling in a crate for
/// a handful of `std::env` lines (ADR-0007), exactly as `bookmarks` resolves
/// the config directory.
///
/// `XDG_DOWNLOAD_DIR` is what a desktop session sets when the reader has told
/// it where downloads belong, and honouring it is the difference between saving
/// where they expect and saving where we guessed.
pub fn default_directory() -> PathBuf {
    if let Some(named) = std::env::var_os("XDG_DOWNLOAD_DIR").map(PathBuf::from)
        && !named.as_os_str().is_empty()
    {
        return named;
    }
    let home = if cfg!(windows) {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    } else {
        std::env::var_os("HOME").map(PathBuf::from)
    };
    match home {
        Some(home) => home.join("Downloads"),
        None => PathBuf::from("."),
    }
}

/// The filename to save a URL's contents under.
///
/// The last path segment, rebuilt character by character rather than trusted. A
/// URL is written by whoever served the page, and the three things a name must
/// not be able to do are leave the directory it was given, name a device, or be
/// empty — so separators, `..`, and everything outside a small allowed set are
/// replaced rather than rejected. A name that survives that is a name, and one
/// that does not becomes `image`.
///
/// The query string goes, because it is not part of what a reader calls the
/// file, and the era's picture URLs carry a great deal of it.
pub fn name_for(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    let last = path
        .rsplit('/')
        .find(|segment| !segment.is_empty() && *segment != "." && *segment != "..")
        .unwrap_or_default();
    let cleaned: String = last
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => character,
            // Everything else, including every separator and every control
            // character, becomes one. A dropped character could turn `a/b` into
            // `ab`; a replaced one cannot turn anything into a path.
            _ => '_',
        })
        // A name that is nothing but dots is `.` or `..` under another spelling.
        .skip_while(|character| *character == '.')
        .collect();
    // Bounded, because a filesystem's own limit is not somewhere to find out
    // about a hostile name. 96 is longer than any real picture's name and
    // shorter than every limit worth worrying about.
    let cleaned: String = cleaned.chars().take(96).collect();
    if cleaned.is_empty() {
        "image".to_owned()
    } else {
        cleaned
    }
}

/// A path in `directory` that nothing is at yet.
///
/// `photo.jpg`, then `photo-1.jpg`, then `photo-2.jpg`. Saving the same picture
/// twice is a thing people do on purpose — the second copy is usually the point
/// — and silently writing over the first would be the browser deciding it knew
/// better.
///
/// Gives up after a hundred, which is not a limit anybody will meet and is
/// better than looping forever against a directory that cannot be written to.
pub fn free_path(directory: &Path, name: &str) -> Option<PathBuf> {
    let first = directory.join(name);
    if !first.exists() {
        return Some(first);
    }
    let (stem, extension) = match name.rsplit_once('.') {
        // A leading dot is not an extension; `.profile` is a name.
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };
    (1..100).find_map(|n| {
        let candidate = directory.join(format!("{stem}-{n}{extension}"));
        (!candidate.exists()).then_some(candidate)
    })
}

/// Writes `bytes` into `directory` under a name derived from `url`.
///
/// Returns where it went, so the caller can say so. The directory is created if
/// it is not there, because a reader whose desktop has no `Downloads` folder
/// should get their picture rather than an explanation.
pub fn save(directory: &Path, url: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("could not make {}: {error}", directory.display()))?;
    let path = free_path(directory, &name_for(url))
        .ok_or_else(|| format!("no free name for {url} in {}", directory.display()))?;
    std::fs::write(&path, bytes).map_err(|error| format!("could not write {path:?}: {error}"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_comes_from_the_last_segment_without_the_query() {
        assert_eq!(name_for("https://example.com/photos/cat.jpg"), "cat.jpg");
        assert_eq!(
            name_for("https://example.com/cat.jpg?utm_source=nowhere&w=64"),
            "cat.jpg"
        );
        assert_eq!(name_for("https://example.com/cat.png#top"), "cat.png");
    }

    #[test]
    fn a_trailing_slash_falls_back_to_the_segment_before_it() {
        assert_eq!(name_for("https://example.com/photos/"), "photos");
    }

    #[test]
    fn a_url_with_no_name_in_it_still_gets_one() {
        // An address that is only a host falls back to the host, by the same
        // rule the trailing slash above follows. Not much of a filename, and
        // better than none: it is what the reader typed, and it is what every
        // other browser calls the file.
        assert_eq!(name_for("https://example.com/"), "example.com");
        assert_eq!(name_for(""), "image");
        // A segment of nothing but dots is `.` or `..` under another spelling,
        // and a name beginning with one is hidden on every platform that has an
        // opinion about it.
        assert_eq!(name_for("https://example.com/..."), "image");
        assert_eq!(name_for("https://example.com/.hidden"), "hidden");
    }

    #[test]
    fn a_name_cannot_leave_the_directory_it_was_given() {
        // The one thing this function exists to guarantee. Every one of these
        // is a real spelling of "somewhere else", and none of them may survive
        // as anything a `join` would follow.
        for hostile in [
            "https://example.com/a/..%2f..%2fetc%2fpasswd",
            "https://example.com/....//....//etc/shadow",
            "https://example.com/a/b/../../../../root/.ssh/id_rsa",
            "https://example.com/C:%5CWindows%5Csystem32%5Cdrivers",
        ] {
            let name = name_for(hostile);
            assert!(!name.contains('/'), "{hostile} kept a separator: {name}");
            assert!(!name.contains('\\'), "{hostile} kept a separator: {name}");
            assert!(!name.starts_with('.'), "{hostile} kept a dot: {name}");
            assert_eq!(
                Path::new(&name).components().count(),
                1,
                "{hostile} became more than one path component: {name}"
            );
        }
    }

    #[test]
    fn a_name_is_bounded() {
        let long = format!("https://example.com/{}.jpg", "a".repeat(5000));
        assert!(name_for(&long).len() <= 96);
    }

    #[test]
    fn saving_the_same_picture_twice_keeps_both() {
        let dir = std::env::temp_dir().join(format!("2kbrowser-downloads-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let first = save(&dir, "https://example.com/cat.jpg", b"one").expect("writes");
        assert_eq!(first.file_name().unwrap(), "cat.jpg");
        let second = save(&dir, "https://example.com/cat.jpg", b"two").expect("writes");
        assert_eq!(second.file_name().unwrap(), "cat-1.jpg");

        assert_eq!(std::fs::read(&first).expect("read"), b"one");
        assert_eq!(std::fs::read(&second).expect("read"), b"two");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_with_no_extension_still_gets_a_second_copy() {
        let dir = std::env::temp_dir().join(format!("2kbrowser-noext-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");

        std::fs::write(dir.join("photo"), b"one").expect("write");
        assert_eq!(
            free_path(&dir, "photo").expect("a free name"),
            dir.join("photo-1")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
