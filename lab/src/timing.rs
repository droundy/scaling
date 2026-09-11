//! Where a timing comes from: the machine, or a file.
//!
//! Replay exists so that a change to the *algorithm* can be tested against
//! byte-identical data. If two variants disagree on replayed timings, the
//! difference is the variant. If they disagree on fresh timings, it might
//! just have been a busy afternoon.
//!
//! This matters most for variants that change *how many* samples get taken -
//! a stopping rule, say - because those cannot be evaluated by re-reading a
//! finished CSV. A variant that only changes how the numbers are combined
//! does not need this at all: use `lab compare` on recorded runs instead,
//! which is simpler and needs no rebuild.
//!
//! ```none
//! LAB_REPLAY=runs/a.csv cargo run --release -- run 2000 /dev/null
//! ```

use std::collections::HashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// One timing, with everything needed to put it back where it happened.
///
/// A raw dump that keeps only "these are the timings for this workload" has
/// thrown away most of what makes it raw. Three things are worth the
/// columns:
///
/// * **`slot`**, the position within the round. The order is reshuffled
///   every round on purpose, because position matters - a memory canary
///   immediately before a payload leaves that payload's cache cold. Without
///   `slot` you cannot ask whether it did.
/// * **`round`**, kept explicitly rather than inferred from position, so a
///   gap is visible as a gap.
/// * **`t_ms`**, wall clock, so a run can be lined up against something that
///   happened outside it - a build starting, a laptop being unplugged.
#[derive(Clone, Debug)]
pub struct Sample {
    pub round: usize,
    pub slot: usize,
    pub workload: String,
    pub t_ms: u128,
    /// Raw, for the whole batch. Divide by the iteration count to compare
    /// anything across runs.
    pub ns: f64,
}

pub struct Timing {
    replay: Option<HashMap<(usize, String), f64>>,
    /// Calibrated iteration counts, from the replayed file when replaying.
    pub iters: HashMap<String, u64>,
    /// Every timing taken, in the order taken.
    pub log: Vec<Sample>,
}

impl Timing {
    /// Read `LAB_REPLAY` and open a source of timings.
    pub fn from_env() -> Timing {
        match std::env::var("LAB_REPLAY") {
            Ok(path) => {
                let rec = read(&path);
                eprintln!("replaying {} timings from {path}", rec.samples.len());
                let map = rec
                    .samples
                    .iter()
                    .map(|s| ((s.round, s.workload.clone()), s.ns))
                    .collect();
                Timing { replay: Some(map), iters: rec.iters, log: Vec::new() }
            }
            Err(_) => Timing { replay: None, iters: HashMap::new(), log: Vec::new() },
        }
    }

    pub fn replaying(&self) -> bool {
        self.replay.is_some()
    }

    /// Time one batch, or look up what it cost last time.
    ///
    /// Keyed for replay by round and workload rather than by `slot`, so a
    /// driver change that reshuffles the order can still replay old data.
    ///
    /// In replay mode `f` is *not* run. That is the point - replay is meant
    /// to be fast and deterministic - but it does mean a workload's side
    /// effects do not happen, so do not put anything load-bearing in one.
    pub fn time(
        &mut self,
        round: usize,
        slot: usize,
        name: &str,
        f: impl FnOnce() -> u64,
    ) -> f64 {
        let t_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let ns = match &self.replay {
            Some(m) => *m.get(&(round, name.to_string())).unwrap_or_else(|| {
                panic!(
                    "no recorded timing for round {round} of {name}; the recording \
                     is shorter than this run, or the workload was renamed"
                )
            }),
            None => {
                let t = Instant::now();
                let sink = f();
                let ns = t.elapsed().as_nanos() as f64;
                // Consume the result so the optimiser cannot delete the work.
                std::hint::black_box(sink);
                ns
            }
        };
        self.log.push(Sample { round, slot, workload: name.to_string(), t_ms, ns });
        ns
    }

    /// Write everything taken so far, plus the calibration, as CSV.
    pub fn write(&self, path: &str) {
        use std::fmt::Write as _;
        let mut s = String::with_capacity(self.log.len() * 48);
        let mut names: Vec<_> = self.iters.iter().collect();
        names.sort();
        for (name, n) in names {
            let _ = writeln!(s, "# iters {name} {n}");
        }
        // `seq` is the line's own index. Redundant with file order, and
        // written anyway so the order survives being sorted or filtered by
        // something else later.
        s.push_str("seq,round,slot,workload,t_ms,ns\n");
        for (seq, x) in self.log.iter().enumerate() {
            let _ = writeln!(
                s,
                "{seq},{},{},{},{},{:.0}",
                x.round, x.slot, x.workload, x.t_ms, x.ns
            );
        }
        if let Err(e) = std::fs::write(path, s) {
            eprintln!("could not write {path}: {e}");
        }
    }
}

/// A recording, as read back.
pub struct Recording {
    pub iters: HashMap<String, u64>,
    /// In the order they were taken.
    pub samples: Vec<Sample>,
}

/// Parse a recording: `# iters <name> <n>` lines, then the sample rows.
pub fn read(path: &str) -> Recording {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("could not read {path}: {e}"));
    let mut iters = HashMap::new();
    let mut samples = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# iters ") {
            let mut f = rest.split_whitespace();
            if let (Some(name), Some(n)) = (f.next(), f.next()) {
                if let Ok(n) = n.parse() {
                    iters.insert(name.to_string(), n);
                }
            }
            continue;
        }
        if line.starts_with('#') || line.starts_with("seq,") || line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 6 {
            continue;
        }
        if let (Ok(round), Ok(slot), Ok(t_ms), Ok(ns)) =
            (f[1].parse(), f[2].parse(), f[4].parse(), f[5].parse())
        {
            samples.push(Sample { round, slot, workload: f[3].to_string(), t_ms, ns });
        }
    }
    Recording { iters, samples }
}
