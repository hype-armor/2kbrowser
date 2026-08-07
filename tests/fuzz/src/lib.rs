//! Feeding hostile bytes to the parts of the browser that read them.
//!
//! M4's first requirement is continuous fuzzing of the HTML, CSS, and
//! image-decode paths, because those are the three places attacker-controlled
//! bytes are interpreted. Everything downstream of them — cascade, layout,
//! paint — is reached through mutated markup as well, since a parser that
//! survives garbage can still hand the next stage something it cannot cope
//! with.
//!
//! # Why this is written here
//!
//! `cargo-fuzz` and libFuzzer would be the obvious tools and are not usable:
//! they need a nightly toolchain, and `rust-toolchain.toml` pins stable. Nor
//! is the usual reason for coverage-guided fuzzing in play — its great value is
//! finding memory-unsafety, and ADR-0002 forbids `unsafe`, so the failure modes
//! actually reachable here are panics, hangs, and unbounded allocation. Those
//! are findable by feeding in mutated bytes and watching, which needs no
//! instrumentation and no new dependency (ADR-0007).
//!
//! What that costs is honestly stated: this is a dumb mutator, not a
//! coverage-guided one. It will not find a bug behind a magic constant it has
//! to guess. It is good at what actually breaks parsers — truncation,
//! repetition, deep nesting, and bytes appearing where a length said they would
//! not.
//!
//! # Reproducibility
//!
//! Every input is a pure function of the seed and the iteration number, so a
//! failure found on a CI machine reproduces exactly:
//!
//! ```text
//! cargo run -p fuzz -- --target html --seed 12345 --iterations 100000
//! ```
//!
//! # Surviving the crash it is looking for
//!
//! The input under test is written to disk *before* it runs, not after it
//! fails. A panic in the test profile unwinds and can be caught; the release
//! profile sets `panic = "abort"`, and an abort leaves nothing to catch. In
//! both cases the bytes are already on disk, so the crasher survives the
//! process that found it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A deterministic pseudo-random generator.
///
/// xorshift64*, which is nine lines and entirely adequate for choosing where to
/// put a mutation. Written here rather than taken from `rand` because a
/// dependency for this would be hard to justify (ADR-0007) — and because a
/// fuzzer's generator must never change under it. An upstream improvement to
/// randomness quality would silently invalidate every recorded seed.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Starts from `seed`. Zero is remapped, since xorshift is stuck there.
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9e37_79b9_7f4a_7c15
            } else {
                seed
            },
        }
    }

    /// The next value.
    pub fn next_u64(&mut self) -> u64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        self.state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value below `bound`. Zero for a bound of zero, which saves every
    /// caller a length check on an empty input.
    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % bound as u64) as usize
    }

    /// One of `items`, or `None` when there are none.
    pub fn choose<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        items.get(self.below(items.len()))
    }
}

/// Largest input the mutator will produce.
///
/// Growth mutations compound, and without a ceiling a long run drifts into
/// measuring how fast the machine can memcpy rather than whether the parser is
/// correct. 256 KiB is far past any document this engine is meant for and still
/// small enough to keep iterations cheap.
const MAX_INPUT: usize = 256 * 1024;

/// Bytes worth trying at a boundary.
///
/// The structural characters of HTML, CSS, and URLs, plus the numeric edges. A
/// dumb mutator finds far more with these than with uniform random bytes: the
/// interesting states of a parser are reached by punctuation, not by noise.
const INTERESTING: &[u8] = b"<>&\"'/\\{}();:%#=?[]!-*\0\x01\x7f\x80\xff\n\r\t ";

/// Produces a mutated copy of `input`.
///
/// Between one and four mutations, so a single run mixes small perturbations
/// with compound damage.
pub fn mutate(rng: &mut Rng, input: &[u8], corpus: &[Vec<u8>]) -> Vec<u8> {
    let mut out = input.to_vec();
    let rounds = 1 + rng.below(4);
    for _ in 0..rounds {
        mutate_once(rng, &mut out, corpus);
    }
    out.truncate(MAX_INPUT);
    out
}

fn mutate_once(rng: &mut Rng, out: &mut Vec<u8>, corpus: &[Vec<u8>]) {
    match rng.below(8) {
        // Flip one bit. The classic, and what finds off-by-one length checks.
        0 if !out.is_empty() => {
            let at = rng.below(out.len());
            out[at] ^= 1 << rng.below(8);
        }
        // Drop in a structural byte, which is how a parser gets steered into a
        // state the input never legitimately reaches.
        1 if !out.is_empty() => {
            let at = rng.below(out.len());
            out[at] = *rng.choose(INTERESTING).unwrap_or(&b'<');
        }
        // Truncate. Every "read the next N bytes" is a bug waiting for an input
        // that ends first.
        2 if !out.is_empty() => {
            let keep = rng.below(out.len());
            out.truncate(keep);
        }
        // Delete a span.
        3 if !out.is_empty() => {
            let at = rng.below(out.len());
            let len = 1 + rng.below((out.len() - at).min(64));
            out.drain(at..at + len);
        }
        // Duplicate a span. Growth finds quadratic behaviour, and repeating a
        // fragment of markup is how nesting gets deep enough to matter.
        4 if !out.is_empty() && out.len() < MAX_INPUT => {
            let at = rng.below(out.len());
            let len = 1 + rng.below((out.len() - at).min(1024));
            let chunk = out[at..at + len].to_vec();
            let to = rng.below(out.len());
            out.splice(to..to, chunk);
        }
        // Splice in a piece of another corpus entry, which is the one way a
        // mutator without coverage feedback can combine two structures.
        5 if !corpus.is_empty() => {
            let Some(other) = rng.choose(corpus).filter(|other| !other.is_empty()) else {
                return;
            };
            let at = rng.below(other.len());
            let len = 1 + rng.below((other.len() - at).min(1024));
            let chunk = other[at..at + len].to_vec();
            let to = rng.below(out.len().max(1)).min(out.len());
            out.splice(to..to, chunk);
        }
        // Repeat a byte many times. Cheap way to reach a depth or length limit.
        6 => {
            let byte = *rng.choose(INTERESTING).unwrap_or(&b'<');
            let count = 1 + rng.below(4096);
            let to = rng.below(out.len().max(1)).min(out.len());
            out.splice(to..to, std::iter::repeat_n(byte, count));
        }
        // Insert a random byte anywhere, including at the very end.
        _ => {
            let byte = (rng.next_u64() & 0xff) as u8;
            let to = rng.below(out.len().max(1)).min(out.len());
            out.insert(to, byte);
        }
    }
}

/// Which part of the browser an input is fed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The HTML parser and the tree it builds.
    Html,
    /// The CSS parser.
    Css,
    /// Image decoding.
    Image,
    /// URL parsing and relative resolution.
    Url,
    /// The whole pipeline: parse, cascade, lay out, paint.
    Render,
}

impl Target {
    /// Every target, for a run that names none.
    pub const ALL: [Target; 5] = [
        Target::Html,
        Target::Css,
        Target::Image,
        Target::Url,
        Target::Render,
    ];

    /// Its name on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Target::Html => "html",
            Target::Css => "css",
            Target::Image => "image",
            Target::Url => "url",
            Target::Render => "render",
        }
    }

    /// Parses a name, for the command line.
    pub fn from_name(name: &str) -> Option<Self> {
        Target::ALL.into_iter().find(|t| t.name() == name)
    }

    /// Which seed files this target starts from.
    fn seed_extensions(self) -> &'static [&'static str] {
        match self {
            Target::Html | Target::Render => &["html"],
            Target::Css => &["css"],
            Target::Image => &["png", "gif", "jpg", "jpeg"],
            // URLs are short and structured; the seeds are written out below
            // rather than read from disk.
            Target::Url => &[],
        }
    }
}

/// Runs one input through a target.
///
/// Deliberately does not assert anything about the *result*. A parser given
/// garbage is entitled to return anything at all; what it may not do is panic,
/// hang, or exhaust memory. Checking the output would be checking a
/// specification that does not exist for these inputs.
pub fn run_once(target: Target, input: &[u8], fonts: &mut text::FontStore) {
    match target {
        Target::Html => {
            // Through the decoder, because that is how bytes become a document
            // in the browser — and it puts the encoding sniffer under test too.
            let (html, ..) = net::encoding::decode_document(input, None);
            let document = dom::parse(&html);
            // Walked, not merely built: a tree with a cycle or a dangling index
            // parses fine and hangs the first thing that traverses it.
            let mut nodes = 0usize;
            for node in document.descendants(document.root()) {
                nodes += 1;
                let _ = document.enclosing_link(node);
                // A malformed tree that reported more nodes than it has would
                // otherwise spin here forever rather than failing.
                assert!(nodes <= MAX_INPUT * 4, "descendants did not terminate");
            }
        }
        Target::Css => {
            let (source, ..) = net::encoding::decode_document(input, None);
            let sheet = css::Stylesheet::parse(&source);
            // The style attribute is a separate entry point with its own
            // parser, and it takes the same untrusted bytes.
            let _ = css::parse_style_attribute(&source);
            let _ = sheet.rules.len();
        }
        Target::Image => {
            let _ = paint::decode(input);
        }
        Target::Url => {
            let text = String::from_utf8_lossy(input);
            if let Ok((origin, path)) = net::parse_url(&text) {
                // Resolution is the part with the segment walking in it, and it
                // only runs on a URL that parsed.
                let _ = net::resolve(&origin, &path, "a.html");
                let _ = net::resolve(&origin, &path, "../../../b.html");
                let _ = net::resolve(&origin, &path, &text);
                let _ = net::policy::to_file_path(&path);
            }
            let _ = net::policy::has_scheme(&text);
            let _ = net::policy::is_drive_path(&text);
        }
        Target::Render => {
            let (html, ..) = net::encoding::decode_document(input, None);
            // Narrow and short on purpose: layout cost scales with both, and
            // the bugs live in the box tree rather than in the pixel count.
            // No base URL, so nothing is fetched — this is a parser and layout
            // test, not a network one.
            let _ = shell::render::render(&html, 200, 400, fonts);
        }
    }
}

/// What a run found.
#[derive(Debug, Default)]
pub struct Report {
    /// Inputs run.
    pub iterations: usize,
    /// Inputs that panicked. Each is written to the crashers directory.
    pub crashes: Vec<PathBuf>,
    /// Inputs that took longer than [`Report::threshold`]. Also written out.
    pub slow: Vec<(PathBuf, Duration)>,
    /// The longest any single input took.
    pub worst: Duration,
    /// The slowest unmutated seed, which the threshold is derived from.
    pub baseline: Duration,
    /// What counted as too slow in this run.
    pub threshold: Duration,
}

impl Report {
    /// Whether the run found nothing.
    pub fn is_clean(&self) -> bool {
        self.crashes.is_empty() && self.slow.is_empty()
    }
}

/// How many times slower than an unmutated seed an input may be.
///
/// Relative rather than absolute, because an absolute number would mean
/// different things on different machines and in different profiles — and the
/// gap is enormous. The same pathological document takes 3.9 s in the test
/// profile and 99 ms in release: a threshold tuned in one is meaningless in the
/// other, and a CI runner slower than a laptop turns a real margin into a flake.
/// Calibrating against a real fixture on the same machine, in the same build,
/// removes both problems.
///
/// The factor is set from measurement rather than taste. Across the soaks run
/// so far the slowest mutated document came in at about 11x the slowest real
/// fixture — mutation makes pages pathological, and flagging that would be
/// noise rather than findings. 50x leaves better than four times headroom over
/// anything observed while still catching a change in *shape*: the accidentally
/// quadratic loop, the retry that never converges.
///
/// Both numbers scale with the machine, which is the point: a CI runner three
/// times slower moves the input and the threshold together.
pub const SLOW_FACTOR: u32 = 50;

/// How many findings of each kind a run will record before it stops writing.
///
/// A target that is broken for *every* input — which is what a regression in a
/// parser looks like — otherwise writes one file per iteration. Filling the
/// repository with a thousand copies of the same bug helps nobody, and on a
/// long soak it is a way to run a machine out of disk. The run continues and
/// keeps counting; only the writing stops.
const MAX_RECORDED: usize = 16;

/// Floor under the computed threshold.
///
/// A target whose seeds all parse in microseconds — `url` — would otherwise get
/// a threshold in the low milliseconds, where scheduler noise alone trips it.
pub const SLOW_FLOOR: Duration = Duration::from_secs(2);

/// A fuzzing run against one target.
pub struct Session {
    target: Target,
    /// How many leading corpus entries are real documents rather than recorded
    /// findings. Only these are calibrated against.
    genuine: usize,
    corpus: Vec<Vec<u8>>,
    directory: PathBuf,
    fonts: text::FontStore,
}

impl Session {
    /// Loads the seed corpus for `target`.
    ///
    /// Seeds are the reference fixtures rather than a corpus of their own: they
    /// are the documents this browser is meant to render, so a mutation of one
    /// is a plausible broken version of something real. A separate corpus would
    /// be a second set of files to keep current.
    pub fn new(target: Target) -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut corpus = Vec::new();
        for directory in [
            root.join("../ref/fixtures"),
            root.join("../ref/fixtures/styles"),
            root.join("../ref/fixtures/assets"),
        ] {
            collect(&directory, target.seed_extensions(), &mut corpus);
        }
        // Everything above is a real document, which is what "normal" means
        // when the harness asks how long normal takes.
        let genuine = corpus.len();
        // Recorded findings are mutated in too, so a fixed bug stays fixed and
        // a near miss keeps being explored — but they are *not* calibrated on.
        // A recorded crasher is garbage by construction and often slow by
        // construction, and letting one set the baseline raises the threshold
        // until nothing can ever trip it.
        collect_all(&root.join("corpus").join(target.name()), &mut corpus);

        if target == Target::Url {
            corpus.extend(
                [
                    "https://example.com/a/b.html?q=1#frag",
                    "http://user:pw@example.com:8080/x",
                    "file:///home/user/pages/index.html",
                    r"file://D:\site\pages\a.html",
                    "file:///C:/site/a.html",
                    "mailto:someone@example.com",
                    "//example.com/protocol-relative",
                ]
                .iter()
                .map(|url| url.as_bytes().to_vec()),
            );
        }
        // A run with nothing to mutate would report a clean pass having tested
        // nothing, which is the failure mode this project cares most about.
        assert!(
            !corpus.is_empty(),
            "no seed inputs for target `{}` — the fixtures are missing",
            target.name()
        );

        Self {
            target,
            genuine: genuine.max(1).min(corpus.len()),
            corpus,
            directory: root.join("corpus").join(target.name()),
            fonts: text::FontStore::new(),
        }
    }

    /// How many seeds this session starts from.
    pub fn corpus_len(&self) -> usize {
        self.corpus.len()
    }

    /// Times the unmutated seeds, which is what "too slow" is measured against.
    ///
    /// Every *real* seed, taking the slowest: a corpus of small stylesheets and
    /// one large page has a range in it, and calibrating on the average would
    /// make the biggest real document look like a finding.
    ///
    /// Recorded findings are excluded. They are mutated garbage, frequently
    /// slow garbage, and one of them setting the baseline would raise the
    /// threshold past anything that could ever trip it — a check that cannot
    /// fail, which is worse than no check.
    pub fn calibrate(&mut self) -> Duration {
        let mut worst = Duration::ZERO;
        for index in 0..self.genuine {
            let seed = self.corpus[index].clone();
            let started = Instant::now();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_once(self.target, &seed, &mut self.fonts);
            }));
            worst = worst.max(started.elapsed());
        }
        worst
    }

    /// Runs `iterations` mutated inputs from `seed`.
    pub fn run(&mut self, seed: u64, iterations: usize) -> Report {
        let mut rng = Rng::new(seed);
        let mut report = Report::default();

        report.baseline = self.calibrate();
        report.threshold = (report.baseline * SLOW_FACTOR).max(SLOW_FLOOR);

        for iteration in 0..iterations {
            let base = rng.choose(&self.corpus).cloned().unwrap_or_default();
            let input = mutate(&mut rng, &base, &self.corpus);

            // Written before it runs. An abort — which is what the release
            // profile does with a panic — leaves nothing to catch, so the only
            // reliable record is one made in advance.
            let in_flight = self.directory.join("in-flight.bin");
            let _ = write_input(&in_flight, &input);

            let started = Instant::now();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_once(self.target, &input, &mut self.fonts);
            }));
            let elapsed = started.elapsed();

            report.iterations += 1;
            report.worst = report.worst.max(elapsed);

            if outcome.is_err() {
                let at = self.record(&input, "crash", seed, iteration, report.crashes.len());
                report.crashes.push(at);
            } else if elapsed > report.threshold {
                let at = self.record(&input, "slow", seed, iteration, report.slow.len());
                report.slow.push((at, elapsed));
            }
            let _ = std::fs::remove_file(&in_flight);
        }
        report
    }

    /// Saves an input that found something, and returns where it went.
    ///
    /// Past [`MAX_RECORDED`] the path is returned without the file being
    /// written: a target that fails on every input would otherwise write one
    /// per iteration. The finding is still counted and still named, so the
    /// report says how many there were and the seed still reproduces them.
    fn record(
        &self,
        input: &[u8],
        kind: &str,
        seed: u64,
        iteration: usize,
        already: usize,
    ) -> PathBuf {
        let path = self
            .directory
            .join(format!("{kind}-{seed:016x}-{iteration}.bin"));
        if already < MAX_RECORDED {
            let _ = write_input(&path, input);
        }
        path
    }
}

fn write_input(path: &Path, input: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(path)?;
    file.write_all(input)?;
    // Flushed rather than left to the drop, because the process this is
    // guarding against may not get to run destructors.
    file.flush()
}

/// Reads every file in `directory` with one of `extensions`.
fn collect(directory: &Path, extensions: &[&str], into: &mut Vec<Vec<u8>>) {
    read_sorted(directory, into, |path| {
        path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            extensions
                .iter()
                .any(|wanted| wanted.eq_ignore_ascii_case(e))
        })
    });
}

/// Reads every file in `directory`, which is where findings and hand-written
/// regression seeds are kept.
fn collect_all(directory: &Path, into: &mut Vec<Vec<u8>>) {
    read_sorted(directory, into, |path| {
        let name = path.file_name().and_then(|name| name.to_str());
        // The in-flight file is whatever a previous run was holding when it
        // died: a duplicate of a recorded crasher at best, noise at worst. A
        // dotfile is repository furniture rather than a test input.
        !name.is_some_and(|name| name == "in-flight.bin" || name.starts_with('.'))
    });
}

/// Reads matching files in a fixed order.
///
/// Sorted by path, because `read_dir` hands back entries in whatever order the
/// filesystem keeps them and that differs between machines. Corpus order
/// decides which seed each iteration mutates, so an unsorted read would mean a
/// recorded seed reproduced a *different* run on the machine it was reported
/// from — quietly breaking the one property this harness is built on.
fn read_sorted(directory: &Path, into: &mut Vec<Vec<u8>>, wanted: impl Fn(&Path) -> bool) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && wanted(path))
        .collect();
    paths.sort();
    for path in paths {
        if let Ok(bytes) = std::fs::read(&path) {
            into.push(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generator_is_deterministic() {
        // The property the whole harness rests on: a seed from a failing CI run
        // has to reproduce the same inputs here.
        let first: Vec<u64> = (0..8).map(|_| Rng::new(42).next_u64()).collect();
        assert!(first.iter().all(|value| *value == first[0]));

        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        assert_ne!(Rng::new(7).next_u64(), Rng::new(8).next_u64());
    }

    #[test]
    fn a_zero_seed_still_generates() {
        // xorshift is stuck at zero, and a seed of zero is exactly what someone
        // types first.
        let mut rng = Rng::new(0);
        let values: Vec<u64> = (0..4).map(|_| rng.next_u64()).collect();
        assert!(values.iter().any(|value| *value != 0), "{values:?}");
    }

    #[test]
    fn mutation_actually_changes_the_input() {
        // A mutator that returned its input would make every run a clean pass
        // that tested one thing.
        let seed = b"<html><body><p>hello</p></body></html>".to_vec();
        let corpus = vec![seed.clone()];
        let mut rng = Rng::new(1);
        let changed = (0..200)
            .filter(|_| mutate(&mut rng, &seed, &corpus) != seed)
            .count();
        assert!(changed > 190, "only {changed} of 200 differed");
    }

    #[test]
    fn mutation_stays_within_the_size_ceiling() {
        // Growth mutations compound. Without a ceiling a long run measures
        // memcpy speed rather than correctness.
        let corpus = vec![vec![b'x'; 4096]];
        let mut rng = Rng::new(3);
        let mut input = corpus[0].clone();
        for _ in 0..500 {
            input = mutate(&mut rng, &input, &corpus);
            assert!(input.len() <= MAX_INPUT, "grew to {}", input.len());
        }
    }

    #[test]
    fn mutating_an_empty_input_does_not_panic() {
        // Truncation reaches empty quickly, and every mutation then indexes
        // into nothing.
        let corpus = vec![Vec::new(), b"x".to_vec()];
        let mut rng = Rng::new(5);
        let mut input = Vec::new();
        for _ in 0..500 {
            input = mutate(&mut rng, &input, &corpus);
            input.clear();
        }
        assert!(input.is_empty());
    }

    #[test]
    fn every_target_has_a_name_that_round_trips() {
        for target in Target::ALL {
            assert_eq!(Target::from_name(target.name()), Some(target));
        }
        assert_eq!(Target::from_name("nonsense"), None);
    }

    #[test]
    fn the_slow_threshold_scales_with_the_machine() {
        // The whole reason it is relative: the same pathological document takes
        // 3.9 s in the test profile and 99 ms in release, and a CI runner is
        // slower again. An absolute number would be a flake on one of them.
        let mut session = Session::new(Target::Css);
        let report = session.run(1, 1);
        assert!(report.baseline > Duration::ZERO, "nothing was timed");
        assert_eq!(
            report.threshold,
            (report.baseline * SLOW_FACTOR).max(SLOW_FLOOR)
        );
        assert!(
            report.threshold >= SLOW_FLOOR,
            "a fast target needs a floor"
        );
    }

    #[test]
    fn calibration_ignores_recorded_findings() {
        // A recorded crasher is garbage by construction and often slow by
        // construction. One of them setting the baseline would raise the
        // threshold past anything that could trip it — a check that cannot
        // fail, which is worse than no check.
        for target in Target::ALL {
            let session = Session::new(target);
            assert!(
                session.genuine <= session.corpus_len(),
                "{} counts more real seeds than it has",
                target.name()
            );
            assert!(
                session.genuine > 0,
                "{} calibrates on nothing",
                target.name()
            );
        }
    }

    #[test]
    fn every_target_has_something_to_mutate() {
        // A target with no seeds would report a clean run having tested
        // nothing — the one result this project treats as worse than a failure.
        for target in Target::ALL {
            let session = Session::new(target);
            assert!(session.corpus_len() > 0, "{} has no seeds", target.name());
        }
    }
}
