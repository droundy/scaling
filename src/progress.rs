//! Saying how a long run is going.

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
        eprintln!(
            "[{}] {}/{} done, at most {} to go",
            duration_text(elapsed),
            self.total - live,
            self.total,
            duration_text(at_most)
        );
    }
}

/// `400ms`, `45s`, `2m05s`, `1h02m05s`: the units a timing is printed in
/// below a minute, and above it the minutes and hours those do not have.
fn duration_text(time: Duration) -> String {
    let secs = time.as_secs_f64().round() as u64;
    match (secs / 3600, secs / 60 % 60) {
        _ if secs < 60 => format!("{time:.0?}"),
        (0, minutes) => format!("{minutes}m{:02}s", secs % 60),
        (hours, minutes) => format!("{hours}h{minutes:02}m{:02}s", secs % 60),
    }
}
