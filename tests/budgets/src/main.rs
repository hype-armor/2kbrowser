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
        Check {
            name: "RSS rendering a reference page",
            limit: "<= 100 MB".to_owned(),
            outcome: Outcome::Pending {
                blocked_on: "memory instrumentation; rendering itself now works",
            },
        },
        Check {
            name: "third-party network requests",
            limit: "0".to_owned(),
            outcome: Outcome::Pending {
                // The policy exists and is unit-tested in `net`. What is missing
                // is subresource loading: nothing yet follows <link> or <img>,
                // so there is no end-to-end request count to assert against.
                blocked_on: "subresource loading; the policy itself is tested in net",
            },
        },
        font_payload(),
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
