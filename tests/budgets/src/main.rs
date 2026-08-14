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

use std::path::PathBuf;
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

    // The browser, not this harness. `Renderer::new` re-invokes whatever is
    // running, which here is `budgets` — and `budgets --render-child` is not a
    // renderer, so the first thing the parent read was garbage. The wire layer
    // caught it ("length field does not fit the frame"), which is the bounds
    // checking working, and the measurement was still wrong.
    let browser = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(BROWSER)))
        .filter(|path| path.exists());
    let Some(browser) = browser else {
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
    shell::render::render_with_base(page, 400, 400, &mut fonts, Some((&origin, &path)));
    let third_party = net::third_party_request_count();
    let same_origin =
        shell::render::render_with_base(control, 400, 400, &mut fonts, Some((&origin, &path)))
            .images_loaded;

    match (third_party, same_origin) {
        (0, 1) => Check {
            name,
            limit,
            outcome: Outcome::Pass {
                measured: "0 of 3 third-party issued, 1 of 1 same-origin loaded".to_owned(),
            },
        },
        (0, _) => fail(
            format!("{same_origin} of 1 same-origin"),
            "the same-origin control did not load, so the zero above proves nothing".to_owned(),
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
