//! The watchdog, checked from outside the process it ends.
//!
//! A fuzzer that hangs on a hostile input reports nothing: the harness stops
//! printing and a person has to notice. The watchdog turns that into a named
//! input and a distinct exit status — and being code that only runs when
//! something has already gone wrong, it is exactly the kind that rots unchecked.
//!
//! It cannot be checked in-process, because what it does is end the process. So
//! the binary hangs on purpose and this reads the corpse.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long to give the watchdog before deciding it is not coming.
///
/// The self-test's limit is one second and the watchdog wakes four times a
/// second, so this is more than ten times the margin it needs — and it exists at
/// all because the obvious version of this test does not fail when the watchdog
/// breaks, it *hangs*. A test that hangs when the anti-hang feature stops
/// working would be an unusually pointed way to get this wrong.
const PATIENCE: Duration = Duration::from_secs(20);

#[test]
fn an_input_that_never_returns_is_named_rather_than_waited_on() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fuzz"))
        .arg("--hang-selftest")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the fuzzer runs");

    let deadline = Instant::now() + PATIENCE;
    loop {
        match child.try_wait().expect("the child can be waited on") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the watchdog never fired: the fuzzer hung on a hang");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    let output = child.wait_with_output().expect("the child is collected");

    // A distinct status, not just failure. A run that found crashers finished
    // and wrote them; a run that hung did neither, and the remaining iterations
    // were never tried. Those want different responses from whoever reads the
    // exit code, so they are different codes.
    assert_eq!(
        output.status.code(),
        Some(fuzz::HANG_EXIT),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The message has to carry the input's path and a way to reproduce it.
    // "Something hung" is the report a person could already have written by
    // watching the terminal; the file and the seed are the part that is worth
    // anything.
    let complaint = String::from_utf8_lossy(&output.stderr);
    assert!(complaint.contains("HANG"), "{complaint}");
    assert!(complaint.contains("in-flight.bin"), "{complaint}");
    assert!(complaint.contains("--seed"), "{complaint}");
}

#[test]
fn an_ordinary_run_is_not_cut_short_by_the_watchdog() {
    // The other half, and the one that would bite in practice: a watchdog that
    // fires on a healthy run is worse than none, because it turns every soak
    // into a false finding. `url` is the fastest target, so this is a few
    // thousand inputs in well under the interval the watchdog even wakes on.
    let output = Command::new(env!("CARGO_BIN_EXE_fuzz"))
        .args(["--target", "url", "--iterations", "3000"])
        .output()
        .expect("the fuzzer runs");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("hang past"), "{report}");
}
