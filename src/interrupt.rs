//! Hearing Ctrl-C while a run is going, so that it can stop early and print
//! what it has.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Once;

/// How many runs are listening.
static LISTENING: AtomicUsize = AtomicUsize::new(0);
static ASKED: AtomicBool = AtomicBool::new(false);
static INSTALL: Once = Once::new();

/// The status of a process ended by Ctrl-C, by the shell's convention.
const STATUS: i32 = 130;

/// Whether anyone has asked to stop since the first run began listening.
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
pub(crate) struct Listen(());

impl Listen {
    pub(crate) fn start() -> Listen {
        INSTALL.call_once(|| {
            // A handler can be set once, and stays. So this one does what
            // Ctrl-C would have done, except for the first ask of a run that
            // is listening. If the program already has a handler, it keeps it
            // and a run is never asked to stop.
            let _ = ctrlc::try_set_handler(|| {
                if LISTENING.load(Ordering::SeqCst) == 0 || ASKED.swap(true, Ordering::SeqCst) {
                    std::process::exit(STATUS);
                }
            });
        });
        LISTENING.fetch_add(1, Ordering::SeqCst);
        Listen(())
    }
}

impl Drop for Listen {
    fn drop(&mut self) {
        LISTENING.fetch_sub(1, Ordering::SeqCst);
    }
}
