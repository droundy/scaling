//! Saying how a long run is going.

use crate::MetricValue;
use std::io::{self, Write};
use std::time::{Duration, Instant};

/// How long a run goes before it first says anything, and then waits before
/// saying it again. The wait doubles each time, up to [`LONGEST`].
const FIRST: Duration = Duration::from_secs(5);
const LONGEST: Duration = Duration::from_secs(60);

pub(crate) struct Progress {
    total: usize,
    started: Instant,
    next: Duration,
    wait: Duration,
}

impl Progress {
    pub(crate) fn new(total: usize) -> Self {
        Progress {
            total,
            started: Instant::now(),
            next: FIRST,
            wait: FIRST,
        }
    }

    /// Say how the run is going on stderr, if it has been long enough since
    /// it last did. `live` is how many benchmarks are still running, and
    /// `at_most` how much longer they can take.
    pub(crate) fn show(&mut self, live: usize, at_most: Duration) {
        let elapsed = self.started.elapsed();
        if elapsed < self.next {
            return;
        }
        self.wait = (self.wait * 2).min(LONGEST);
        self.next = elapsed + self.wait;
        // Not `eprintln!`, which panics if nobody is reading stderr, and a
        // run that has gone this far is not worth losing for that.
        let _ = writeln!(
            io::stderr(),
            "[{:.2}] {}/{} done, at most {:.2} to go",
            MetricValue::time(elapsed),
            self.total - live,
            self.total,
            MetricValue::time(at_most)
        );
    }
}
