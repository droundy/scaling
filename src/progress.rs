//! Saying how a long run is going.

use crate::suite::Sweep;
use std::time::{Duration, Instant};

/// How long a run goes before it first says anything, and then waits before
/// saying it again. The wait doubles each time, up to [`LONGEST`].
const FIRST: Duration = Duration::from_secs(5);
const LONGEST: Duration = Duration::from_secs(60);
/// A run with this few left names them.
const NAMED: usize = 3;

pub(crate) struct Progress {
    started: Instant,
    next: Duration,
    wait: Duration,
}

impl Progress {
    pub(crate) fn new() -> Self {
        Progress {
            started: Instant::now(),
            next: FIRST,
            wait: FIRST,
        }
    }

    /// Say how the run is going on stderr, if it has been long enough since
    /// it last did.
    pub(crate) fn show(&mut self, sweep: &Sweep<'_>, names: &[String]) {
        let elapsed = self.started.elapsed();
        if elapsed < self.next {
            return;
        }
        self.wait = (self.wait * 2).min(LONGEST);
        self.next = elapsed + self.wait;
        eprintln!("{}", line(elapsed, sweep, names));
    }
}

fn line(elapsed: Duration, sweep: &Sweep<'_>, names: &[String]) -> String {
    let done = sweep.total - sweep.live.len();
    let mut line = format!(
        "[{}] {done}/{} done, at most {} to go",
        clock(elapsed),
        sweep.total,
        clock(sweep.at_most)
    );
    if sweep.live.len() <= NAMED {
        let waiting: Vec<&str> = sweep.live.iter().map(|&i| names[i].as_str()).collect();
        line += &format!(": waiting for {}", waiting.join(", "));
    }
    line
}

/// `45s`, `2m05s`, `1h02m05s`.
fn clock(time: Duration) -> String {
    let secs = time.as_secs_f64().round() as u64;
    let (hours, minutes, seconds) = (secs / 3600, secs / 60 % 60, secs % 60);
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, _) => format!("{minutes}m{seconds:02}s"),
        _ => format!("{hours}h{minutes:02}m{seconds:02}s"),
    }
}
