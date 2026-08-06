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
/// Fonts ship beside the binary rather than inside it, so they get their own
/// budget instead of inflating the one above. Real Unicode coverage — CJK and
/// colour emoji in particular — costs tens of megabytes, and cutting coverage
/// to fit a smaller number would render much of the web as tofu.
const MAX_FONT_PAYLOAD_BYTES: u64 = 64 * 1024 * 1024;

/// Maximum size of everything we ship: binary plus fonts plus data.
///
/// Tracked separately because the two budgets above can each pass while the
/// thing a user actually downloads grows without anyone noticing.
const MAX_DISTRIBUTION_BYTES: u64 = 84 * 1024 * 1024;

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

fn main() -> ExitCode {
    let checks = vec![
        binary_size(),
        Check {
            name: "cold start to first paint",
            limit: "<= 150 ms".to_owned(),
            outcome: Outcome::Pending {
                blocked_on: "M1 (nothing paints yet)",
            },
        },
        Check {
            name: "RSS rendering a reference page",
            limit: "<= 100 MB".to_owned(),
            outcome: Outcome::Pending {
                blocked_on: "M1 (nothing renders yet)",
            },
        },
        Check {
            name: "third-party network requests",
            limit: "0".to_owned(),
            outcome: Outcome::Pending {
                blocked_on: "M1 (no network stack yet)",
            },
        },
        Check {
            name: "bundled font payload",
            limit: format!("<= {}", human_bytes(MAX_FONT_PAYLOAD_BYTES)),
            outcome: Outcome::Pending {
                blocked_on: "M1 (fonts land with the text stack, ADR-0008)",
            },
        },
        Check {
            name: "total distribution",
            limit: format!("<= {}", human_bytes(MAX_DISTRIBUTION_BYTES)),
            outcome: Outcome::Pending {
                blocked_on: "M1 (fonts land with the text stack, ADR-0008)",
            },
        },
    ];

    report(&checks)
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
    // CARGO_MANIFEST_DIR is <repo>/tests/budgets.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .to_path_buf();
    let path = root.join("target").join("release").join(exe);
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
