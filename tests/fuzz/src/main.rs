//! Runs the fuzzer for as long as you ask it to.
//!
//! `cargo test` runs a short fixed pass on every target (see `tests/soak.rs`);
//! this is the same harness with the iteration count turned up, for the soak
//! that "continuous fuzzing" actually means.
//!
//! ```text
//! cargo run -p fuzz                                     # every target, 5000 each
//! cargo run -p fuzz -- --target render --iterations 1e6 # one target, hard
//! cargo run -p fuzz -- --seed 0x1234 --iterations 100   # reproduce a finding
//! ```
//!
//! Run it in the default profile. The release profile sets `panic = "abort"`,
//! so a panic kills the process instead of being caught and counted — the
//! crasher is still written to disk, but the run ends at the first one.

use std::process::ExitCode;

use fuzz::{Session, Target};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    // Not in the usage text: it is how `tests/watchdog.rs` checks that the
    // watchdog ends the process, which cannot be checked from inside a process
    // the watchdog would end.
    if args.iter().any(|a| a == HANG_SELFTEST) {
        fuzz::hang_selftest();
    }

    let settings = match Settings::parse(&args) {
        Ok(settings) => settings,
        Err(message) => {
            eprintln!("error: {message}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    if cfg!(not(debug_assertions)) {
        println!(
            "note: built with `panic = \"abort\"`; the run stops at the first \
             crash rather than counting them\n"
        );
    }

    let mut failed = false;
    for target in settings.targets {
        let mut session = Session::new(target);
        println!(
            "{:<8} {} seed(s), {} iteration(s), seed {:#018x}",
            target.name(),
            session.corpus_len(),
            settings.iterations,
            settings.seed
        );
        let report = session.run(settings.seed, settings.iterations);

        for path in &report.crashes {
            println!("  CRASH  {}", path.display());
        }
        for (path, elapsed) in &report.slow {
            println!("  SLOW   {} ({:.1?})", path.display(), elapsed);
        }
        println!(
            "  {} {} run, worst {:.1?} (baseline {:.1?}, slow past {:.1?}, hang past {:.1?})\n",
            if report.is_clean() { "ok" } else { "FAILED" },
            report.iterations,
            report.worst,
            report.baseline,
            report.threshold,
            report.hang
        );
        failed |= !report.is_clean();
    }

    if failed {
        eprintln!("findings were written to tests/fuzz/corpus/");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

const USAGE: &str = "\
usage: cargo run -p fuzz -- [--target NAME] [--seed N] [--iterations N]

    --target NAME      html, css, image, url, render (default: all)
    --seed N           starting seed; decimal or 0x-prefixed (default: 1)
    --iterations N     inputs per target (default: 5000)

Findings are written to tests/fuzz/corpus/<target>/ and are picked up as seeds
by later runs, so a fixed bug stays fixed.";

/// Argument that makes the binary hang on purpose, so the watchdog can be
/// caught doing its job.
pub const HANG_SELFTEST: &str = "--hang-selftest";

struct Settings {
    targets: Vec<Target>,
    seed: u64,
    iterations: usize,
}

impl Settings {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut settings = Settings {
            targets: Target::ALL.to_vec(),
            seed: 1,
            iterations: 5000,
        };
        let mut rest = args.iter();
        while let Some(flag) = rest.next() {
            let value = || {
                rest.clone()
                    .next()
                    .ok_or_else(|| format!("{flag} needs a value"))
            };
            match flag.as_str() {
                "--target" => {
                    let name = value()?;
                    settings.targets = vec![
                        Target::from_name(name)
                            .ok_or_else(|| format!("unknown target `{name}`"))?,
                    ];
                    rest.next();
                }
                "--seed" => {
                    settings.seed = parse_number(value()?)?;
                    rest.next();
                }
                "--iterations" => {
                    settings.iterations = parse_number(value()?)? as usize;
                    rest.next();
                }
                other => return Err(format!("unknown argument `{other}`")),
            }
        }
        Ok(settings)
    }
}

/// Accepts `1000`, `0x3e8`, and `1e6`, because a soak count is usually typed as
/// a power of ten and typing seven zeroes correctly is its own small hazard.
fn parse_number(text: &str) -> Result<u64, String> {
    let bad = || format!("`{text}` is not a number");
    if let Some(hex) = text.strip_prefix("0x") {
        return u64::from_str_radix(hex, 16).map_err(|_| bad());
    }
    if let Some((mantissa, exponent)) = text.split_once('e') {
        let mantissa: u64 = mantissa.parse().map_err(|_| bad())?;
        let exponent: u32 = exponent.parse().map_err(|_| bad())?;
        return mantissa
            .checked_mul(10u64.checked_pow(exponent).ok_or_else(bad)?)
            .ok_or_else(bad);
    }
    text.parse().map_err(|_| bad())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(args: &[&str]) -> Result<Settings, String> {
        Settings::parse(&args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn the_defaults_cover_every_target() {
        let settings = settings(&[]).expect("parses");
        assert_eq!(settings.targets.len(), Target::ALL.len());
        assert!(settings.iterations > 0);
    }

    #[test]
    fn a_seed_and_a_count_are_read() {
        let parsed = settings(&["--seed", "0x2a", "--iterations", "1e3"]).expect("parses");
        assert_eq!(parsed.seed, 42);
        assert_eq!(parsed.iterations, 1000);
    }

    #[test]
    fn a_target_can_be_named() {
        let parsed = settings(&["--target", "css"]).expect("parses");
        assert_eq!(parsed.targets, vec![Target::Css]);
        assert!(settings(&["--target", "nope"]).is_err());
    }

    #[test]
    fn a_bad_argument_is_refused_rather_than_ignored() {
        // Silently ignoring `--iteration` would run the default count and
        // report a pass, which is the failure this project cares about.
        assert!(settings(&["--iteration", "10"]).is_err());
        assert!(settings(&["--seed"]).is_err());
        assert!(settings(&["--seed", "twelve"]).is_err());
    }

    #[test]
    fn counts_are_read_in_the_forms_people_type() {
        assert_eq!(parse_number("1000"), Ok(1000));
        assert_eq!(parse_number("0x3e8"), Ok(1000));
        assert_eq!(parse_number("1e6"), Ok(1_000_000));
        assert!(parse_number("1e99").is_err(), "overflow is not a count");
    }
}
