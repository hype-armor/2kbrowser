//! Network requests, off the thread that draws the window (#207).
//!
//! Every fetch the browser made used to happen inside the winit event handler
//! that asked for it. `show` called `fetch_raw` and waited; so did opening a
//! tab, sending a form, and saving a picture. A request is a DNS lookup, a TCP
//! connection, a TLS handshake and however long the server takes — seconds,
//! against a window that is one thread — and for all of it the browser drew
//! nothing, answered nothing, and could not even be scrolled.
//!
//! That is the bug behind "if one tab is busy, it hangs the whole UI". It was
//! not the *tab* that was busy. It was the only thread there is.
//!
//! So a request is asked for here and answered later. The window keeps drawing,
//! the reader keeps scrolling the page they are already on, and the answer
//! arrives as a wake-up like a painted band does — the same mechanism, for the
//! same reason.
//!
//! ## What this is not
//!
//! Not a connection pool, not a scheduler, and not a cache. The policy still
//! decides every request (ADR-0006) and it decides it on the worker exactly as
//! it did on the main thread, because a `Fetcher` is its policy and nothing
//! else — there is no shared state here to get wrong. What arrives back is the
//! same `Result` the caller used to get inline.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};

/// Which tab asked. Stable across reordering and closing, which an index is
/// not: a fetch outlives the arrangement of the tab strip it started in.
pub type TabId = u64;

/// How many requests can be in flight at once.
///
/// More than one because tabs are independent: a reader who opens a link in a
/// background tab and goes back to reading should not have the page they are
/// reading wait behind it. Few, because these are the era's servers and this
/// browser has no business opening dozens of connections to one — and because
/// every one of them is a thread.
const WORKERS: usize = 4;

/// What a request is *for*, which is also what to do with the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    /// Go to this address.
    Navigate {
        /// Where to.
        url: String,
    },
    /// Send this form, and go wherever it leads (#110).
    Submit {
        /// Where to.
        url: String,
        /// The encoded pairs.
        body: String,
    },
    /// Fetch this picture and write it to disk (#205).
    SaveImage {
        /// The picture.
        url: String,
        /// The page it is on, so the policy sees the same third-party question
        /// it saw when the page asked for it.
        document: Box<net::Origin>,
        /// Which directory to write into.
        into: PathBuf,
    },
}

impl Want {
    /// The address being asked for.
    pub fn url(&self) -> &str {
        match self {
            Want::Navigate { url } | Want::Submit { url, .. } | Want::SaveImage { url, .. } => url,
        }
    }
}

/// A request, and enough to know where its answer belongs.
#[derive(Debug, Clone)]
pub struct Asked {
    /// The tab that asked.
    pub tab: TabId,
    /// Which request of that tab's this is.
    ///
    /// A reader who types an address, waits, and types a different one has two
    /// requests in flight and wants the second. The window remembers the
    /// number it last asked for, and an answer that does not match it is one
    /// nobody is waiting for any more.
    pub seq: u64,
    /// What it is for.
    pub want: Want,
}

/// A request that has come back.
#[derive(Debug)]
pub struct Done {
    /// What was asked.
    pub asked: Asked,
    /// What came back. The error is already a sentence, because `FetchError`
    /// does not cross a thread boundary any better than its message does and
    /// the window only ever shows the message.
    pub outcome: Result<Box<net::Fetched>, String>,
}

/// The pool, and the way back to the window.
pub struct Fetches {
    jobs: Sender<Asked>,
    done: Receiver<Done>,
    /// The next unused request number, shared across every tab.
    ///
    /// One counter rather than one per tab: it only has to be unique per tab,
    /// and a single counter cannot get out of step with itself.
    next: u64,
    /// The worker threads.
    ///
    /// Held to count them and for no other reason — see [`Fetches::drop`],
    /// which deliberately does not wait for them.
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl Fetches {
    /// Starts the pool.
    ///
    /// `wake` is called on a worker thread whenever an answer is ready. It is
    /// what turns "the window is asleep" into "the window redraws", exactly as
    /// a painted band does.
    pub fn start(fetcher: net::Fetcher, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (jobs, inbox) = channel::<Asked>();
        let (outbox, done) = channel::<Done>();
        // One receiver behind a lock rather than a queue per worker: whichever
        // thread is free takes the next request, which is what stops a slow
        // site holding up a fast one behind it.
        let inbox = Arc::new(Mutex::new(inbox));
        let wake = Arc::new(wake);

        let workers = (0..WORKERS)
            .map(|_| {
                let inbox = Arc::clone(&inbox);
                let outbox = outbox.clone();
                let wake = Arc::clone(&wake);
                let fetcher = fetcher.clone();
                std::thread::spawn(move || {
                    loop {
                        // The lock is held only for the take, never for the
                        // request — otherwise the pool would be one worker
                        // wearing four hats.
                        let asked = {
                            let Ok(inbox) = inbox.lock() else { return };
                            match inbox.recv() {
                                Ok(asked) => asked,
                                // The window is gone.
                                Err(_) => return,
                            }
                        };
                        let outcome = perform(&fetcher, &asked.want);
                        if outbox.send(Done { asked, outcome }).is_err() {
                            return;
                        }
                        wake();
                    }
                })
            })
            .collect();

        Self {
            jobs,
            done,
            next: 1,
            workers,
        }
    }

    /// Asks for something, and does not wait.
    ///
    /// Returns the request number, which the caller records so it can tell the
    /// answer it is waiting for from one it has moved on from.
    pub fn ask(&mut self, tab: TabId, want: Want) -> u64 {
        let seq = self.next;
        self.next += 1;
        // A send that fails means every worker is gone, which cannot be
        // recovered from here and is not worth a second error path: the answer
        // simply never arrives, and the request stays pending in the bar — the
        // same thing the reader sees from a server that never replies.
        let _ = self.jobs.send(Asked { tab, seq, want });
        seq
    }

    /// Takes whatever has come back. Never blocks.
    pub fn take(&mut self) -> Vec<Done> {
        let mut out = Vec::new();
        loop {
            match self.done.try_recv() {
                Ok(done) => out.push(done),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return out,
            }
        }
    }

    /// How many threads are serving requests. For the tests.
    #[cfg(test)]
    fn workers(&self) -> usize {
        self.workers.len()
    }
}

impl Drop for Fetches {
    fn drop(&mut self) {
        // Closing the job channel is what tells an idle worker to stop.
        // Replacing the sender with one whose receiver is already gone is how
        // to close it while `self` is still borrowed.
        let (dead, _) = channel();
        self.jobs = dead;
        // And then *not* waiting for them, which is the whole point. A worker
        // in the middle of a request is inside a socket read with no timeout on
        // it, so joining here would make closing the window hang for as long as
        // some server felt like holding the connection open — which is the
        // failure this module exists to remove, moved to the one moment a
        // person is least willing to tolerate it.
        //
        // Nothing is leaked by letting go. A worker holds a clone of the
        // policy and a channel; when its request finishes it finds the channel
        // closed and returns. In the browser the process is exiting anyway.
        self.workers.clear();
    }
}

/// Carries out one request.
///
/// The policy check is inside `fetch_raw` and `post`, so it happens here on the
/// worker exactly as it happened on the main thread. Nothing about which
/// requests are allowed changed by moving where they are made, which is the
/// property worth stating: ADR-0006 is enforced by the fetcher, not by the
/// thread it runs on.
fn perform(fetcher: &net::Fetcher, want: &Want) -> Result<Box<net::Fetched>, String> {
    let fetched = match want {
        Want::Navigate { url } => fetcher.fetch_raw(url, None, net::RequestKind::Navigation),
        Want::Submit { url, body } => fetcher.post(url, body),
        // A subresource, not a navigation: it is the same request the page
        // made, so it answers to the same third-party rule. A picture the page
        // was not allowed to load cannot be had by asking a second time.
        Want::SaveImage { url, document, .. } => {
            fetcher.fetch_raw(url, Some(document), net::RequestKind::Subresource)
        }
    };
    fetched.map(Box::new).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fetcher() -> net::Fetcher {
        net::Fetcher::default()
    }

    #[test]
    fn a_request_is_answered_and_the_window_is_woken() {
        let woken = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&woken);
        let mut fetches = Fetches::start(fetcher(), move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });

        // A file that is not there. What matters is that an answer comes back
        // at all and that the window hears about it — a failure travels the
        // same path a success does, and it is the path under test.
        let seq = fetches.ask(
            7,
            Want::Navigate {
                url: "file:///definitely/not/here.html".to_owned(),
            },
        );

        let done = loop {
            let mut batch = fetches.take();
            if let Some(done) = batch.pop() {
                break done;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(done.asked.tab, 7);
        assert_eq!(done.asked.seq, seq);
        assert!(done.outcome.is_err(), "a missing file came back as a page");
        assert!(
            woken.load(Ordering::SeqCst) >= 1,
            "nothing woke the window, so the answer would sit there unseen"
        );
    }

    #[test]
    fn request_numbers_are_never_reused() {
        // What tells an answer somebody is waiting for from one they have moved
        // on from. Two navigations in a row in the same tab must not be able to
        // be mistaken for each other.
        let mut fetches = Fetches::start(fetcher(), || {});
        let first = fetches.ask(
            1,
            Want::Navigate {
                url: "a".to_owned(),
            },
        );
        let second = fetches.ask(
            1,
            Want::Navigate {
                url: "b".to_owned(),
            },
        );
        let other_tab = fetches.ask(
            2,
            Want::Navigate {
                url: "c".to_owned(),
            },
        );
        assert_ne!(first, second);
        assert_ne!(second, other_tab);
        assert_ne!(first, other_tab);
    }

    #[test]
    fn taking_nothing_does_not_block() {
        // Called on every wake, including wakes that were not about a fetch.
        // If this ever blocked, the window would hang on the thing that exists
        // to stop it hanging.
        let mut fetches = Fetches::start(fetcher(), || {});
        assert!(fetches.take().is_empty());
    }

    #[test]
    fn several_requests_are_served_at_once() {
        // The point of more than one worker: a slow site must not hold up a
        // fast one queued behind it. Four missing files answer four times
        // without any of them waiting for the others to finish.
        let mut fetches = Fetches::start(fetcher(), || {});
        assert_eq!(fetches.workers(), WORKERS);
        for tab in 0..WORKERS as u64 {
            fetches.ask(
                tab,
                Want::Navigate {
                    url: format!("file:///nowhere/{tab}.html"),
                },
            );
        }
        let mut seen = 0;
        while seen < WORKERS {
            seen += fetches.take().len();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(seen, WORKERS);
    }

    #[test]
    fn a_want_knows_the_address_it_is_for() {
        // The bar shows it while the request is in flight, so every shape has
        // to be able to say it.
        assert_eq!(
            Want::Navigate {
                url: "https://example.com/".to_owned()
            }
            .url(),
            "https://example.com/"
        );
        assert_eq!(
            Want::Submit {
                url: "https://example.com/search".to_owned(),
                body: "q=x".to_owned(),
            }
            .url(),
            "https://example.com/search"
        );
    }
}
