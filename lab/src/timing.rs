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
use std::time::Instant;

pub struct Timing {
    replay: Option<HashMap<(usize, String), f64>>,
    /// Calibrated iteration counts, from the replayed file when replaying.
    pub iters: HashMap<String, u64>,
    /// Every timing taken, in the order taken: `(round, workload, ns)`.
    pub log: Vec<(usize, String, f64)>,
}

impl Timing {
    /// Read `LAB_REPLAY` and open a source of timings.
    pub fn from_env() -> Timing {
        match std::env::var("LAB_REPLAY") {
            Ok(path) => {
                let (iters, times) = read(&path);
                eprintln!("replaying {} timings from {path}", times.len());
                Timing { replay: Some(times), iters, log: Vec::new() }
            }
            Err(_) => Timing { replay: None, iters: HashMap::new(), log: Vec::new() },
        }
    }

    pub fn replaying(&self) -> bool {
        self.replay.is_some()
    }

    /// Time one call, or look up what it cost last time.
    ///
    /// In replay mode `f` is *not* run. That is the point - replay is meant
    /// to be fast and deterministic - but it does mean a workload's side
    /// effects do not happen, so do not put anything load-bearing in one.
    pub fn time(&mut self, round: usize, name: &str, f: impl FnOnce() -> u64) -> f64 {
        let ns = match &self.replay {
            Some(m) => *m.get(&(round, name.to_string())).unwrap_or_else(|| {
                panic!("no recorded timing for round {round} of {name}; \
                        the recording is shorter than this run, or the workload was renamed")
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
        self.log.push((round, name.to_string(), ns));
        ns
    }

    /// Write everything taken so far, plus the calibration, as CSV.
    pub fn write(&self, path: &str) {
        use std::fmt::Write as _;
        let mut s = String::with_capacity(self.log.len() * 32);
        let mut names: Vec<_> = self.iters.iter().collect();
        names.sort();
        for (name, n) in names {
            let _ = writeln!(s, "# iters {name} {n}");
        }
        s.push_str("round,workload,ns\n");
        for (r, name, ns) in &self.log {
            let _ = writeln!(s, "{r},{name},{ns:.0}");
        }
        if let Err(e) = std::fs::write(path, s) {
            eprintln!("could not write {path}: {e}");
        }
    }
}

/// Parse a recording: `# iters <name> <n>` lines, then `round,workload,ns`.
pub fn read(path: &str) -> (HashMap<String, u64>, HashMap<(usize, String), f64>) {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("could not read {path}: {e}"));
    let mut iters = HashMap::new();
    let mut times = HashMap::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# iters ") {
            let mut f = rest.split_whitespace();
            if let (Some(name), Some(n)) = (f.next(), f.next()) {
                if let Ok(n) = n.parse() {
                    iters.insert(name.to_string(), n);
                }
            }
        } else if !line.starts_with('#') && !line.starts_with("round,") {
            let mut f = line.split(',');
            if let (Some(r), Some(w), Some(ns)) = (f.next(), f.next(), f.next()) {
                if let (Ok(r), Ok(ns)) = (r.parse(), ns.parse()) {
                    times.insert((r, w.to_string()), ns);
                }
            }
        }
    }
    (iters, times)
}
