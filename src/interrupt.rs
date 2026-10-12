//! Hearing Ctrl-C while a run is going, so that it can stop early and print
//! what it has.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Once;

/// How many runs can be stopped with what they have: those in which every
/// benchmark has been calibrated.
static READY: AtomicUsize = AtomicUsize::new(0);
static ASKED: AtomicBool = AtomicBool::new(false);
static INSTALL: Once = Once::new();

/// The status of a process ended by Ctrl-C, by the shell's convention.
const STATUS: i32 = 130;

/// Whether anyone has asked to stop since the first run began.
pub(crate) fn asked() -> bool {
    ASKED.load(Ordering::SeqCst)
}

/// End the process as Ctrl-C would have, if it was asked to stop.
pub(crate) fn exit_if_asked() {
    if asked() {
        std::process::exit(STATUS);
    }
}

/// A run that is listening for Ctrl-C, for as long as this is held.
pub(crate) struct Listen {
    ready: bool,
}

impl Listen {
    pub(crate) fn start() -> Listen {
        INSTALL.call_once(|| {
            // A handler can be set once, and stays. So this one does what
            // Ctrl-C would have done, except for the first ask of a run that
            // is ready to be stopped. If the program already has a handler, it
            // keeps it and a run is never asked to stop.
            let _ = ctrlc::try_set_handler(|| {
                if READY.load(Ordering::SeqCst) == 0 || ASKED.swap(true, Ordering::SeqCst) {
                    std::process::exit(STATUS);
                }
                // Finishing what is running takes a while, and under `cargo`
                // the shell may have its prompt back already, so say so now.
                let _ = writeln!(
                    io::stderr(),
                    "Ctrl-C: finishing what is running, then the results so far; \
                     Ctrl-C again to quit at once"
                );
            });
        });
        Listen { ready: false }
    }

    /// Every benchmark of the run has been calibrated, so there are results
    /// to give. Before this a Ctrl-C ends the process at once, since a run
    /// that has measured nothing has nothing worth waiting for.
    ///
    /// Returns whether this is the first time it was said.
    pub(crate) fn ready(&mut self) -> bool {
        if self.ready {
            return false;
        }
        self.ready = true;
        READY.fetch_add(1, Ordering::SeqCst);
        true
    }
}

impl Drop for Listen {
    fn drop(&mut self) {
        if self.ready {
            READY.fetch_sub(1, Ordering::SeqCst);
        }
    }
}
