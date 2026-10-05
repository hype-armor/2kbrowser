//! Budget enforcement.
//!
//! Resource weight is one of the four goals in PLAN.md, so the budgets are
//! enforced in CI as a failing step rather than written down as aspirations.
//! Run after a release build:
//!
//! ```text
//! cargo build --release
//! cargo run --release -p budgets
//! ```
//!
//! The limits below are the single source of truth. Changing one is a diff to
//! this file, which is the point: a budget should move only as a deliberate,
//! reviewed decision.
//!
//! Checks that cannot be measured yet report PENDING rather than passing. A
//! budget harness that silently reports green for unimplemented measurements is
//! worse than no harness, because it manufactures confidence.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Maximum size of the stripped release binary.
const MAX_BINARY_SIZE_BYTES: u64 = 20 * 1024 * 1024;

/// Maximum size of the bundled font payload (ADR-0008).
///
/// Real Unicode coverage — CJK and colour emoji in particular — costs tens of
/// megabytes, and cutting coverage to fit a smaller number would render much of
/// the web as tofu.
///
/// M1 bundles only the Liberation core (~4 MiB) and does so with `include_bytes!`,
/// which **embeds the fonts in the binary** rather than shipping them beside it
/// as ADR-0008 describes. That is a deliberate M1 simplification: at 4 MiB it
/// fits the binary budget comfortably and avoids runtime path resolution. It
/// does not scale — adding Noto CJK and colour emoji would blow the binary
/// budget outright — so fonts must move out of the binary before the full
/// payload lands. Tracked with the vendor-versus-fetch decision in issue #7.
const MAX_FONT_PAYLOAD_BYTES: u64 = 64 * 1024 * 1024;

/// How many times each interaction is repeated, with the worst one reported.
const LATENCY_REPEATS: usize = 5;

/// What [`machine_work`] costs on the machine the latency limits were chosen
/// on, in milliseconds.
///
/// The limits below are thresholds of human perception, which makes them claims
/// about the computer in front of the reader and not about this code alone. CI
/// runs this harness on three desktop-class runners and on a Raspberry Pi, and
/// asking a Pi to open a page inside the hundred milliseconds a person notices
/// is asking it to be a different computer. A budget that fails for that reason
/// is one everybody learns to ignore.
///
/// So the limits are scaled by how much slower this machine is than that one.
/// The alternative — skipping the check where it cannot be met — risks a
/// harness that reports PENDING everywhere and therefore enforces nothing,
/// which this file's own header calls worse than having no harness at all.
/// Scaling always enforces something: a machine four times slower gets four
/// times the limit and still catches a regression that doubles the cost.
const REFERENCE_WORK_MS: f64 = 5.0;

/// How much slower than the reference a machine may be and still be held to
/// these limits at all.
///
/// Scaling without a ceiling degrades into a budget that passes anything: a
/// machine measuring thirty times slower would be allowed three seconds to open
/// a page, which is not a limit anybody would notice being broken. Past this,
/// the honest report is that the measurement does not mean anything here, which
/// is what PENDING is for. Eight is well beyond the Raspberry Pi this is
/// actually about and well below the factor an unoptimised build shows (around
/// thirty, which is how this ceiling came to be written).
const MAX_MACHINE_FACTOR: f64 = 8.0;

/// The viewport the interaction budgets are measured at.
const LATENCY_WIDTH: u32 = 1000;
const LATENCY_HEIGHT: u32 = 800;

/// Result of evaluating a single budget.
enum Outcome {
    Pass {
        measured: String,
    },
    Fail {
        measured: String,
        reason: String,
    },
    /// Not measurable yet; names the milestone that unblocks it.
    Pending {
        blocked_on: &'static str,
    },
}

/// A budget, its limit, and how it came out.
struct Check {
    name: &'static str,
    limit: String,
    outcome: Outcome,
}

/// The browser binary, which this harness needs beside it to spawn a renderer.
const BROWSER: &str = if cfg!(target_os = "windows") {
    "2kbrowser.exe"
} else {
    "2kbrowser"
};

fn main() -> ExitCode {
    // Before anything measures the browser, check that the browser on disk is
    // the browser the sources describe. See `staleness`.
    if let Some(warning) = staleness() {
        eprintln!("{warning}\n");
    }

    let checks = vec![
        binary_size(),
        Check {
            name: "cold start to first paint",
            limit: "<= 150 ms".to_owned(),
            outcome: Outcome::Pending {
                blocked_on: "the window; headless render works, startup path does not exist",
            },
        },
        resident_memory(),
        third_party_requests(),
        font_payload(),
    ];
    let mut checks = checks;
    checks.extend(interaction_latency());

    report(&checks)
}

/// Peak resident memory for the browser rendering a real page.
///
/// One of the four claims in PLAN.md §1 is "tens of MB, not hundreds", and it
/// has been reporting PENDING since it was written. It is measurable now
/// because the renderer is a separate process with a process id the parent can
/// ask about: the number that matters is the *pair*, since a browser that moved
/// its memory into a child did not save anyone anything.
///
/// Peak rather than current. Rendering allocates a canvas, a box tree, and
/// decoded images and then lets most of it go, so sampling afterwards would
/// measure the tidying up rather than the work.
///
/// Linux only, and it says so elsewhere rather than passing quietly. `VmHWM` in
/// `/proc/<pid>/status` is the high-water mark the kernel already tracks;
/// macOS and Windows both have an equivalent and both need FFI to reach, which
/// ADR-0002 forbids here. A check that runs on one of three platforms is worth
/// more than one that runs nowhere, and this is the platform CI renders on
/// most.
fn resident_memory() -> Check {
    let name = "peak memory rendering a page";
    // Generous against the claim it is checking: PLAN.md says tens of MB, and
    // this is the number at which "tens" has stopped being true. A budget set
    // at the current measurement would fail on the first honest change.
    let limit_mb = 100u64;
    let limit = format!("<= {limit_mb} MB total");

    if !cfg!(target_os = "linux") {
        return Check {
            name,
            limit,
            outcome: Outcome::Pending {
                blocked_on: "reading peak RSS off Linux, which needs FFI (ADR-0002)",
            },
        };
    }

    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../ref/fixtures/era-page.html")
        .canonicalize();
    let Ok(fixture) = fixture else {
        return Check {
            name,
            limit,
            outcome: Outcome::Fail {
                measured: "-".to_owned(),
                reason: "the era reference fixture is missing".to_owned(),
            },
        };
    };
    let Ok(body) = std::fs::read(&fixture) else {
        return Check {
            name,
            limit,
            outcome: Outcome::Fail {
                measured: "-".to_owned(),
                reason: "the era reference fixture could not be read".to_owned(),
            },
        };
    };
    let url = net::file_url(&fixture);
    let Ok((origin, path)) = net::parse_url(&url) else {
        return Check {
            name,
            limit,
            outcome: Outcome::Fail {
                measured: "-".to_owned(),
                reason: "could not parse the fixture URL".to_owned(),
            },
        };
    };

    let Some(browser) = browser_beside_us() else {
        return Check {
            name,
            limit,
            outcome: Outcome::Pending {
                blocked_on: "a release build of the browser beside this harness",
            },
        };
    };
    let renderer = sandbox::Renderer::with_program(browser);
    // Held open on purpose: the child is read while it is alive, because a
    // process that has exited has no `/proc` entry to ask.
    let page = shell::viewport::Viewport::open(
        &renderer,
        shell::viewport::Document {
            body,
            content_type: None,
            origin,
            path,
        },
        800,
        2400,
        // Neither layout override: what is being measured is an ordinary page.
        false,
        false,
        1.0,
    );
    let page = match page {
        Ok(page) => page,
        Err(error) => {
            return Check {
                name,
                limit,
                outcome: Outcome::Fail {
                    measured: "-".to_owned(),
                    reason: format!("the page did not render: {error}"),
                },
            };
        }
    };

    let parent = peak_rss_kib("self");
    let child = peak_rss_kib(&page.child_id().to_string());
    drop(page);

    let (Some(parent), Some(child)) = (parent, child) else {
        return Check {
            name,
            limit,
            outcome: Outcome::Fail {
                measured: "-".to_owned(),
                reason: "could not read VmHWM from /proc".to_owned(),
            },
        };
    };
    let total_mb = (parent + child) as f64 / 1024.0;
    let measured = format!(
        "{total_mb:.1} MB — {:.1} parent + {:.1} renderer",
        parent as f64 / 1024.0,
        child as f64 / 1024.0
    );
    Check {
        name,
        limit,
        outcome: if total_mb <= limit_mb as f64 {
            Outcome::Pass { measured }
        } else {
            Outcome::Fail {
                measured,
                reason: "PLAN.md §1 claims tens of megabytes, not hundreds".to_owned(),
            }
        },
    }
}

/// Peak resident set size in KiB, from the kernel's own high-water mark.
fn peak_rss_kib(pid: &str) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|number| number.parse().ok())
}

/// Renders a page whose every subresource is third-party, and checks that none
/// of them loaded.
///
/// The claim in PLAN.md is that one policy rule removes essentially all
/// advertising and tracking without filter lists (ADR-0006). That claim is
/// about the whole pipeline, not about `Policy::check` — which is separately
/// unit-tested — so it is measured here, end to end, with the real renderer.
///
/// What is counted is requests *issued*, not requests that succeeded. Success
/// counts prove nothing here: a page full of unreachable ad hosts loads no
/// images whether the policy works or not — which this check reported as a
/// pass until the counter replaced it.
///
/// A second page with a same-origin image is rendered as a control, because a
/// loader that had silently stopped working would also issue no requests. Zero
/// third-party and one same-origin is the only passing combination.
///
/// No network is touched: the third-party requests never leave the policy.
fn third_party_requests() -> Check {
    let name = "third-party network requests";
    let limit = "0".to_owned();
    let fail = |measured: String, reason: String| Check {
        name,
        limit: "0".to_owned(),
        outcome: Outcome::Fail { measured, reason },
    };

    // A real image from the reference fixtures, so the control genuinely
    // decodes rather than merely being requested.
    let logo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../ref/fixtures/assets/logo.png")
        .canonicalize();
    let Ok(logo) = logo else {
        return fail(
            "-".to_owned(),
            "the reference fixture assets are missing".to_owned(),
        );
    };
    let document = logo.with_file_name("budget-page.html");
    let url = net::file_url(&document);
    let Ok((origin, path)) = net::parse_url(&url) else {
        return fail(
            "-".to_owned(),
            "could not parse the document URL".to_owned(),
        );
    };

    let page = r#"<!doctype html><html><head>
        <link rel="stylesheet" href="https://tracker.example.net/style.css">
        </head><body>
        <img src="https://ads.example.net/banner.gif" width="10" height="10">
        <img src="https://beacon.example.org/pixel.png" width="1" height="1">
        <p>Text that must survive.</p>
        </body></html>"#;
    let control = r#"<!doctype html><html><body>
        <img src="logo.png"><p>Text that must survive.</p>
        </body></html>"#;

    let mut fonts = text::FontStore::new();
    net::reset_third_party_request_count();
    net::reset_third_party_refusal_count();
    shell::render::render_with_base(page, 400, 400, &mut fonts, Some((&origin, &path)));
    let third_party = net::third_party_request_count();
    // The other half of the pair (issue #118). Zero issued is only evidence
    // when something was asked for: a loader that had stopped resolving `src`
    // attributes at all would report zero issued and pass, and the same-origin
    // control below would not catch it if it broke only for absolute URLs.
    // Three refusals say the policy was reached three times and held.
    let refused = net::third_party_refusal_count();
    let same_origin =
        shell::render::render_with_base(control, 400, 400, &mut fonts, Some((&origin, &path)))
            .images_loaded;

    match (third_party, refused, same_origin) {
        (0, 3, 1) => Check {
            name,
            limit,
            outcome: Outcome::Pass {
                measured: "0 of 3 third-party issued, 3 refused, 1 of 1 same-origin loaded"
                    .to_owned(),
            },
        },
        (0, 3, _) => fail(
            format!("{same_origin} of 1 same-origin"),
            "the same-origin control did not load, so the zero above proves nothing".to_owned(),
        ),
        (0, _, _) => fail(
            format!("{refused} of 3 third-party refused"),
            "the page's third-party subresources never reached the policy, so nothing was proved"
                .to_owned(),
        ),
        _ => fail(
            format!("{third_party} of 3 third-party issued"),
            "a third-party subresource request left the origin (ADR-0006)".to_owned(),
        ),
    }
}

/// Measures the release binary and compares it against the size budget.
fn binary_size() -> Check {
    let name = "release binary size";
    let limit = format!("<= {}", human_bytes(MAX_BINARY_SIZE_BYTES));

    let path = match binary_path() {
        Some(path) => path,
        None => {
            return Check {
                name,
                limit,
                outcome: Outcome::Fail {
                    measured: "not found".to_owned(),
                    reason: "run `cargo build --release` first".to_owned(),
                },
            };
        }
    };

    let size = match std::fs::metadata(&path) {
        Ok(metadata) => metadata.len(),
        Err(err) => {
            return Check {
                name,
                limit,
                outcome: Outcome::Fail {
                    measured: "unreadable".to_owned(),
                    reason: format!("{}: {err}", path.display()),
                },
            };
        }
    };

    let measured = human_bytes(size);
    let outcome = if size <= MAX_BINARY_SIZE_BYTES {
        Outcome::Pass { measured }
    } else {
        Outcome::Fail {
            measured,
            reason: format!(
                "over budget by {}",
                human_bytes(size - MAX_BINARY_SIZE_BYTES)
            ),
        }
    };

    Check {
        name,
        limit,
        outcome,
    }
}

/// Measures the vendored font tree against the font budget.
fn font_payload() -> Check {
    let name = "bundled font payload";
    let limit = format!("<= {}", human_bytes(MAX_FONT_PAYLOAD_BYTES));

    let dir = repo_root().map(|root| root.join("fonts"));
    let Some(total) = dir.as_deref().and_then(directory_size) else {
        return Check {
            name,
            limit,
            outcome: Outcome::Fail {
                measured: "not found".to_owned(),
                reason: "fonts/ is missing".to_owned(),
            },
        };
    };

    let measured = human_bytes(total);
    let outcome = if total <= MAX_FONT_PAYLOAD_BYTES {
        Outcome::Pass { measured }
    } else {
        Outcome::Fail {
            measured,
            reason: format!(
                "over budget by {}",
                human_bytes(total - MAX_FONT_PAYLOAD_BYTES)
            ),
        }
    };
    Check {
        name,
        limit,
        outcome,
    }
}

/// Total size of every regular file under `dir`, recursively.
fn directory_size(dir: &std::path::Path) -> Option<u64> {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in std::fs::read_dir(&path).ok()? {
            let entry = entry.ok()?;
            let file_type = entry.file_type().ok()?;
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                total += entry.metadata().ok()?.len();
            }
        }
    }
    Some(total)
}

/// The repository root, derived from this crate's location.
fn repo_root() -> Option<PathBuf> {
    // CARGO_MANIFEST_DIR is <repo>/tests/budgets.
    Some(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()?
            .parent()?
            .to_path_buf(),
    )
}

/// Whether the browser on disk is older than the sources, as a warning to
/// print before any measurement that spawns it.
///
/// `cargo run --release -p budgets` builds *this* crate and its dependencies.
/// It does not build `target/release/2kbrowser`, which is what the memory
/// budget spawns as a renderer. So the parent can be the new code while the
/// child on disk is whatever was built last — on another branch, quite
/// possibly — and the run then reports
///
/// ```text
/// peak memory rendering a page    FAIL  (the page did not render: renderer
///                                        sent a malformed message: length
///                                        field does not fit the frame)
/// ```
///
/// which reads as "this change broke the wire format" and means "the binary on
/// disk is from a different change". The failure is loud and points at the
/// wrong thing, which is the worst shape a diagnostic can have.
///
/// A warning rather than a failure: the check is a heuristic over file times,
/// and a heuristic that can *stop* a run has to be right every time. This one
/// only has to be useful when it fires. A missing binary is not stale and is
/// reported by the budget that needs it, in the words that budget already has.
fn staleness() -> Option<String> {
    let binary = binary_path()?;
    let built = binary.metadata().ok()?.modified().ok()?;
    let (newest, source) = newest_source(&repo_root()?)?;
    (newest > built).then(|| {
        format!(
            "warning: {} is older than {}.\n\
             warning: `cargo run -p budgets` does not build the browser it spawns — run \
             `cargo build --release` first, or what follows measures the last build rather \
             than this one.",
            binary.display(),
            source.display()
        )
    })
}

/// The most recently modified source file under `root`, and when.
///
/// Walks `crates/` and the workspace manifests — what a build of the browser
/// depends on and nothing else, so that editing this harness or a test fixture
/// does not report the browser as stale.
fn newest_source(root: &Path) -> Option<(std::time::SystemTime, PathBuf)> {
    /// Deep enough for this repository; a bound rather than a judgement.
    const MAX_DEPTH: usize = 12;

    fn walk(
        path: &Path,
        depth: usize,
        best: &mut Option<(std::time::SystemTime, PathBuf)>,
    ) -> Option<()> {
        if depth >= MAX_DEPTH {
            return Some(());
        }
        for entry in std::fs::read_dir(path).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, depth + 1, best);
                continue;
            }
            let is_source = path
                .extension()
                .is_some_and(|extension| extension == "rs" || extension == "toml");
            if !is_source {
                continue;
            }
            if let Ok(modified) = entry.metadata().and_then(|data| data.modified())
                && best.as_ref().is_none_or(|(best, _)| modified > *best)
            {
                *best = Some((modified, path));
            }
        }
        Some(())
    }

    let mut best = None;
    walk(&root.join("crates"), 0, &mut best);
    for manifest in ["Cargo.toml", "Cargo.lock"] {
        let path = root.join(manifest);
        if let Ok(modified) = path.metadata().and_then(|data| data.modified())
            && best.as_ref().is_none_or(|(best, _)| modified > *best)
        {
            best = Some((modified, path));
        }
    }
    best
}

/// Locates the release binary: first CLI argument, else the conventional path.
fn binary_path() -> Option<PathBuf> {
    if let Some(arg) = std::env::args_os().nth(1) {
        let path = PathBuf::from(arg);
        return path.is_file().then_some(path);
    }

    let exe = if cfg!(windows) {
        "2kbrowser.exe"
    } else {
        "2kbrowser"
    };
    let path = repo_root()?.join("target").join("release").join(exe);
    path.is_file().then_some(path)
}

/// Prints the budget table and returns the process exit code.
/// How long the window is frozen for when a reader does something (#207).
///
/// The three operations a reader performs constantly, each measured on the era
/// fixture and each charged to the thread that draws the window. Every one of
/// these is synchronous: the time it takes *is* the time the browser does not
/// respond for, which is why "lots of hanging ui issues" was the original
/// report and why these are the numbers that answer it.
///
/// The limits come from what a person notices rather than from what the
/// browser currently manages, the same way the memory budget is set against
/// PLAN.md's claim and not against the last measurement. A budget pinned to
/// today's number fails on the first honest change and teaches everyone to
/// raise it; a budget set at the threshold of a complaint keeps meaning the
/// same thing as the code moves.
///
/// Measured as the worst of [`LATENCY_REPEATS`] runs, because a reader does not
/// experience an average: they experience the occasion the window stalled, and
/// that is the one they report.
///
/// One page, opened once, then asked for a band and a keystroke, because that
/// is the order a reader does them in and because a renderer that had never
/// drawn the page would answer the other two differently.
fn interaction_latency() -> Vec<Check> {
    // A band must land within a frame. Two frames at 60Hz rather than one:
    // scrolling that misses every other frame reads as a stutter, and this is
    // CI rather than a quiet desktop.
    const BAND_LIMIT_MS: u128 = 32;
    // A keystroke re-renders the page. Half of the hundred milliseconds at
    // which an action stops feeling immediate, because typing is a run of them
    // and the next letter should not queue behind the last.
    const KEYSTROKE_LIMIT_MS: u128 = 50;
    // Opening a page is the one a reader expects to take a moment, so this is
    // the whole of that hundred milliseconds.
    const OPEN_LIMIT_MS: u128 = 100;

    // Measured before anything else, so the limits are known even for the
    // reports that never get as far as rendering a page.
    let factor = machine_factor();
    let names = [
        ("opening a page", scaled_limit(OPEN_LIMIT_MS, factor)),
        ("scrolling a band", scaled_limit(BAND_LIMIT_MS, factor)),
        ("a keystroke", scaled_limit(KEYSTROKE_LIMIT_MS, factor)),
    ];
    let pending = |blocked_on: &'static str| {
        names
            .iter()
            .map(|(name, (_, shown))| Check {
                name,
                limit: shown.clone(),
                outcome: Outcome::Pending { blocked_on },
            })
            .collect::<Vec<_>>()
    };
    let failed = |reason: String| {
        names
            .iter()
            .map(|(name, (_, shown))| Check {
                name,
                limit: shown.clone(),
                outcome: Outcome::Fail {
                    measured: "-".to_owned(),
                    reason: reason.clone(),
                },
            })
            .collect::<Vec<_>>()
    };

    if factor > MAX_MACHINE_FACTOR {
        return pending(
            "a machine too far slower than the one these thresholds describe for them to \
             mean anything here",
        );
    }
    let Some(browser) = browser_beside_us() else {
        return pending("a release build of the browser beside this harness");
    };
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../ref/fixtures/era-page.html")
        .canonicalize();
    let Ok(fixture) = fixture else {
        return failed("the era reference fixture is missing".to_owned());
    };
    let Ok(body) = std::fs::read(&fixture) else {
        return failed("the era reference fixture could not be read".to_owned());
    };
    let url = net::file_url(&fixture);
    let Ok((origin, path)) = net::parse_url(&url) else {
        return failed("could not parse the fixture URL".to_owned());
    };

    let renderer = sandbox::Renderer::with_program(browser);
    let document = || shell::viewport::Document {
        body: body.clone(),
        content_type: None,
        origin: origin.clone(),
        path: path.clone(),
    };

    // Each on its own child, because opening is what is being measured and a
    // reused process would be measuring the second navigation.
    let mut opening = std::time::Duration::ZERO;
    for _ in 0..LATENCY_REPEATS {
        let started = std::time::Instant::now();
        let page = shell::viewport::Viewport::open(
            &renderer,
            document(),
            LATENCY_WIDTH,
            LATENCY_HEIGHT,
            false,
            false,
            1.0,
        );
        let taken = started.elapsed();
        if let Err(error) = page {
            return failed(format!("the page did not render: {error}"));
        }
        opening = opening.max(taken);
    }

    let page = shell::viewport::Viewport::open(
        &renderer,
        document(),
        LATENCY_WIDTH,
        LATENCY_HEIGHT,
        false,
        false,
        1.0,
    );
    let mut page = match page {
        Ok(page) => page,
        Err(error) => return failed(format!("the page did not render: {error}")),
    };

    let mut band = std::time::Duration::ZERO;
    for _ in 0..LATENCY_REPEATS {
        let started = std::time::Instant::now();
        let _ = page.request_band(0, LATENCY_HEIGHT / 2, LATENCY_HEIGHT);
        while page.band_outstanding() && !page.accept_band() {
            std::hint::spin_loop();
        }
        band = band.max(started.elapsed());
    }

    let mut keystroke = std::time::Duration::ZERO;
    for _ in 0..LATENCY_REPEATS {
        let started = std::time::Instant::now();
        page.type_key(sandbox::message::Key::Insert("a".to_owned()));
        keystroke = keystroke.max(started.elapsed());
    }

    vec![
        latency_check(&names[0], opening),
        latency_check(&names[1], band),
        latency_check(&names[2], keystroke),
    ]
}

/// A fixed amount of work, for finding out how fast this machine is.
///
/// Four mebibytes written and read sixteen times over, which is the shape of
/// what rendering actually does — a canvas is a few megabytes and painting
/// walks it. A pure arithmetic loop would measure a part of the machine that
/// painting a page does not lean on.
///
/// Deterministic, so the only thing that varies between runs is the machine.
fn machine_work() -> std::time::Duration {
    let started = std::time::Instant::now();
    let mut buffer = vec![0u32; 1 << 20];
    for round in 0..16u32 {
        for (at, slot) in buffer.iter_mut().enumerate() {
            *slot = slot.wrapping_add((at as u32) ^ round);
        }
    }
    let total = buffer
        .iter()
        .fold(0u32, |sum, slot| sum.wrapping_add(*slot));
    // Kept, so that the optimiser cannot delete the loop that was being timed.
    std::hint::black_box(total);
    started.elapsed()
}

/// How much slower this machine is than the one the limits were chosen on.
///
/// The best of several runs rather than the worst or the mean: the thing being
/// estimated is how fast the machine *can* go, and every source of noise here —
/// another job on the runner, a migration between cores — makes a run slower
/// and none makes it faster. The worst run would measure the neighbours.
///
/// Never below one. A machine faster than the reference does not get a limit
/// tighter than the threshold a person notices, because there is no such thing
/// as responding better than imperceptibly.
fn machine_factor() -> f64 {
    // Nine rather than a handful. The same container measured 1.0 on one run and
    // 1.9 on the next because something else was running on it, and the factor
    // only has to be roughly right — but a limit that halves between runs is one
    // nobody can reason about. More samples cost about forty milliseconds and
    // make the minimum steadier.
    let best = (0..9).map(|_| machine_work()).min().unwrap_or_default();
    (best.as_secs_f64() * 1e3 / REFERENCE_WORK_MS).max(1.0)
}

/// A perception threshold, scaled for the machine measuring it.
fn scaled_limit(perception_ms: u128, factor: f64) -> (u128, String) {
    let scaled = (perception_ms as f64 * factor).round() as u128;
    let shown = if scaled == perception_ms {
        format!("<= {perception_ms} ms")
    } else {
        format!("<= {scaled} ms ({perception_ms} x {factor:.1})")
    };
    (scaled, shown)
}

/// One interaction measured against its limit.
///
/// Its own function so the comparison can be tested. The measurement needs a
/// window, a page and a renderer; deciding whether a number is over a limit
/// needs none of those, and it is the half that would fail silently — a budget
/// that cannot report FAIL is decoration.
fn latency_check(
    (name, (limit, shown)): &(&'static str, (u128, String)),
    taken: std::time::Duration,
) -> Check {
    let measured = format!("{} ms", taken.as_millis());
    Check {
        name,
        limit: shown.clone(),
        outcome: if taken.as_millis() <= *limit {
            Outcome::Pass { measured }
        } else {
            Outcome::Fail {
                measured,
                reason: "the window does not answer for this long, which is what #207 reported"
                    .to_owned(),
            }
        },
    }
}

/// The browser binary beside this harness.
///
/// The browser, not this harness. `Renderer::new` re-invokes whatever is
/// running, which here is `budgets` — and `budgets --render-child` is not a
/// renderer, so the first thing the parent read was garbage. The wire layer
/// caught it ("length field does not fit the frame"), which is the bounds
/// checking working, and the measurement was still wrong.
fn browser_beside_us() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(BROWSER)))
        .filter(|path| path.exists())
}

fn report(checks: &[Check]) -> ExitCode {
    let mut failed = 0usize;
    let mut pending = 0usize;

    println!("{:<34} {:<18} RESULT", "BUDGET", "LIMIT");
    for check in checks {
        let result = match &check.outcome {
            Outcome::Pass { measured } => format!("PASS    {measured}"),
            Outcome::Fail { measured, reason } => {
                failed += 1;
                format!("FAIL    {measured} ({reason})")
            }
            Outcome::Pending { blocked_on } => {
                pending += 1;
                format!("PENDING blocked on {blocked_on}")
            }
        };
        println!("{:<34} {:<18} {result}", check.name, check.limit);
    }

    println!();
    if failed > 0 {
        println!("{failed} budget(s) exceeded.");
        return ExitCode::FAILURE;
    }
    println!("All measurable budgets within limits ({pending} not yet measurable).");
    ExitCode::SUCCESS
}

/// Formats a byte count for human reading, without pulling in a dependency.
fn human_bytes(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const KIB: u64 = 1024;
    if bytes >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.2} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_slower_machine_gets_a_looser_limit_and_a_faster_one_does_not() {
        // The scaling is what keeps these budgets enforceable on a Raspberry Pi
        // without making them meaningless on a desktop, so both ends matter.
        let (limit, shown) = scaled_limit(100, 4.0);
        assert_eq!(
            limit, 400,
            "a machine four times slower gets four times the limit"
        );
        assert!(
            shown.contains("100 x 4.0"),
            "the report has to say why the limit moved, or it reads as a changed budget: {shown}"
        );

        // A fast machine is held to the threshold a person notices and no
        // tighter: there is no responding better than imperceptibly, and a
        // limit that chased the hardware down would fail on the quiet runs.
        let (limit, shown) = scaled_limit(100, 1.0);
        assert_eq!(limit, 100);
        assert_eq!(
            shown, "<= 100 ms",
            "an unscaled limit should read as the plain number"
        );
        // One call, not two compared against each other. This is a
        // measurement, and two of them differ by whatever the machine was
        // doing in between — the first version of this line compared two and
        // failed, which it deserved to.
        let factor = machine_factor();
        assert!(factor >= 1.0, "the factor is never below one: {factor}");
    }

    #[test]
    fn an_interaction_over_its_limit_fails_the_budget() {
        // The half of the latency budgets that has no window in it, and the
        // half that would otherwise be taken on trust. A budget whose
        // comparison is wrong reports PASS for a browser that freezes, which is
        // worse than having no budget at all.
        let band = ("scrolling a band", scaled_limit(32, 1.0));
        let over = latency_check(&band, Duration::from_millis(33));
        assert!(
            matches!(over.outcome, Outcome::Fail { .. }),
            "33ms against a 32ms limit must fail"
        );
        // The limit itself is allowed: `<= 32 ms` is what the report prints, so
        // 32 passing is what it must mean.
        let at = latency_check(&band, Duration::from_millis(32));
        assert!(
            matches!(at.outcome, Outcome::Pass { .. }),
            "32ms against a 32ms limit must pass"
        );
        // Milliseconds, not nanoseconds: a measurement is reported in whole
        // milliseconds, so a limit compared against the wrong unit would pass
        // everything.
        let far_over = latency_check(
            &("opening a page", scaled_limit(100, 1.0)),
            Duration::from_secs(4),
        );
        assert!(
            matches!(far_over.outcome, Outcome::Fail { .. }),
            "4s must fail"
        );
    }

    #[test]
    fn the_newest_source_is_a_source_of_this_browser() {
        // The staleness warning compares the browser's build time against
        // this. Pointed at the wrong tree it would either never fire or fire
        // always, and a warning that always fires is one nobody reads.
        let root = repo_root().expect("repo root");
        let (_, path) = newest_source(&root).expect("some source");
        assert!(
            path.starts_with(root.join("crates")) || path.parent() == Some(root.as_path()),
            "{} is not a source of the browser",
            path.display()
        );
        // `lock` as well as `rs` and `toml`: `newest_source` watches
        // `Cargo.lock` on purpose, because a dependency that moved makes the
        // binary on disk just as stale as an edited source file does. Leaving
        // it out here passed for as long as the lock file was never the newest
        // thing in the tree, and failed the first time a release bumped the
        // version — which touches the lock and nothing else.
        let extension = path.extension().expect("an extension");
        assert!(
            extension == "rs" || extension == "toml" || extension == "lock",
            "{extension:?}"
        );
    }

    #[test]
    fn editing_this_harness_does_not_make_the_browser_stale() {
        // `tests/` is deliberately outside the walk: a change to the budget
        // harness or a reference fixture does not mean the browser on disk is
        // out of date, and reporting that it does would train the reader to
        // ignore the warning.
        let root = repo_root().expect("repo root");
        let (_, path) = newest_source(&root).expect("some source");
        assert!(
            !path.starts_with(root.join("tests")),
            "the walk reached {}",
            path.display()
        );
    }
}
