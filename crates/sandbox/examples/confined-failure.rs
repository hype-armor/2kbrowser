//! Fails, under the renderer's own confinement, in each of the ways a page can
//! make it fail.
//!
//! Rendering a document uses nine syscalls. Failing to render one uses a
//! different set, and no fixture reaches it: a panic wants `gettid` and
//! `tgkill`, an abort wants them too, and a stack overflow — which a document
//! nested deeply enough can cause — runs a signal handler before any of that.
//! Those calls are in the allowlist because this example was traced, not
//! because they seemed likely.
//!
//! It is a measurement instrument rather than a demonstration:
//! `scripts/renderer-syscalls.sh` runs it under `strace`. Run by hand it looks
//! like a program that crashes, which is exactly what it is for.
//!
//! Refusing the abort path is worth stating plainly, since it is the one that
//! reads as safe and is not. `abort` raises a signal at itself with `tgkill`;
//! a filter that denies it does not prevent the abort, it prevents the process
//! from *finishing* it, and a renderer wedged mid-abort is a hang rather than a
//! failure the parent can report.

fn main() {
    let how = std::env::args().nth(1).unwrap_or_default();
    // Applied here rather than in a child, because this program *is* the thing
    // being measured — a confined process failing on purpose.
    let confinement = sandbox::confine::apply();
    eprintln!("confinement={confinement:?} how={how}");

    match how.as_str() {
        "panic" => panic!("a panic under confinement"),
        "abort" => std::process::abort(),
        "overflow" => {
            // `black_box` on all three of the recursion, the accumulator, and
            // the frame padding: without it this optimises into a loop in
            // release and measures a program that returns rather than one that
            // runs out of stack.
            fn deeper(remaining: u64) -> u64 {
                let padding = std::hint::black_box([remaining; 512]);
                if std::hint::black_box(remaining) == 0 {
                    0
                } else {
                    std::hint::black_box(deeper(remaining - 1) + padding[0])
                }
            }
            println!("{}", deeper(u64::MAX));
        }
        // The baseline: confined, and exits normally. What a renderer's last
        // syscalls look like when nothing went wrong.
        "exit" => {}
        _ => eprintln!("usage: confined-failure panic | abort | overflow | exit"),
    }
}
