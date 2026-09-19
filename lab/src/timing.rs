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
use std::time::{SystemTime, UNIX_EPOCH};

/// One timing, with everything needed to put it back where it happened.
///
/// A raw dump that keeps only "these are the timings for this workload" has
/// thrown away most of what makes it raw. Three things are worth the
/// columns:
#[derive(Clone, Debug)]
pub struct Sample {
    /// Should equal the index of this Sample in the log, but kept explicitly so a gap is visible.
    pub round: usize,
    /// Position within the round.
    pub slot: usize,
    pub workload: String,
    /// The wallclock time in ns.
    pub t_ns: u128,
    /// Raw time of the whole batch. Divide by the iteration count to compare
    /// anything across runs.
    pub ns: f64,
}

pub struct Timing {
    replay: Option<HashMap<(usize, String), f64>>,
    /// Calibrated iteration counts, from the replayed file when replaying.
    pub iters: HashMap<String, usize>,
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
                Timing {
                    replay: Some(map),
                    iters: rec.iters,
                    log: Vec::new(),
                }
            }
            Err(_) => Timing {
                replay: None,
                iters: HashMap::new(),
                log: Vec::new(),
            },
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
    pub fn time(&mut self, round: usize, slot: usize, name: &str, f: impl FnOnce() -> f64) -> f64 {
        let t_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let ns = match &self.replay {
            Some(m) => *m.get(&(round, name.to_string())).unwrap_or_else(|| {
                panic!(
                    "no recorded timing for round {round} of {name}; the recording \
                     is shorter than this run, or the workload was renamed"
                )
            }),
            None => f(),
        };
        self.log.push(Sample {
            round,
            slot,
            workload: name.to_string(),
            t_ns,
            ns,
        });
        ns
    }

    /// Write everything taken so far, plus the calibration.
    ///
    /// Binary when the path ends in `.bin`, CSV otherwise, so old recordings
    /// and old habits keep working.
    pub fn write(&self, path: &str) {
        if path.ends_with(".bin") {
            self.write_bin(path)
        } else {
            self.write_csv(path)
        }
    }

    /// Eighteen bytes a sample instead of about fifty-two.
    ///
    /// Worth doing only because of what a small sample size does to the row
    /// count: every batch writes exactly one row, so rows per second is one
    /// over the mean batch duration, whatever the workloads are. At the 200
    /// us batches of the first sweeps that is 5000 rows a second; at the 46
    /// us mean of a ladder reaching below a microsecond it is 21700, and a
    /// day of measuring lands somewhere past half a billion rows.
    ///
    /// Disk is not really the problem - parsing is. The point of a recording
    /// is that estimators can be re-scored against it without measuring
    /// anything again, and that stops being true when a pass over the data
    /// takes hours.
    ///
    /// The header stays text so `head` still tells you what a file holds.
    fn write_bin(&self, path: &str) {
        use std::fmt::Write as _;
        let mut names: Vec<&String> = self.iters.keys().collect();
        names.sort();
        let idx: HashMap<&str, u8> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i as u8))
            .collect();
        if names.len() > 256 {
            eprintln!("{path}: more than 256 rung names; not writing");
            return;
        }
        let epoch = self.log.first().map(|s| s.t_ns).unwrap_or(0);

        let mut head = String::new();
        head.push_str("LABBIN1\n");
        for n in &names {
            let _ = writeln!(head, "# iters {n} {}", self.iters[*n]);
        }
        let _ = writeln!(head, "# epoch {epoch}");
        head.push_str("DATA\n");

        let mut buf: Vec<u8> = Vec::with_capacity(head.len() + self.log.len() * 18);
        buf.extend_from_slice(head.as_bytes());
        for x in &self.log {
            buf.extend_from_slice(&(x.round as u32).to_le_bytes());
            buf.push(x.slot as u8);
            buf.push(idx[x.workload.as_str()]);
            // Offset from the file's own epoch, in ns, as u64. A u32 of
            // microseconds would fit a 71 minute block and save four bytes,
            // which is not worth having to reason about how long a block can
            // get before it silently wraps.
            buf.extend_from_slice(&((x.t_ns.saturating_sub(epoch)) as u64).to_le_bytes());
            // Integer nanoseconds. u32 reaches 4.3 s, against a longest
            // batch here of about 25 ms, and sub-nanosecond resolution on a
            // batch of hundreds of nanoseconds is not information.
            buf.extend_from_slice(
                &(x.ns.round().max(0.0).min(u32::MAX as f64) as u32).to_le_bytes(),
            );
        }
        if let Err(e) = std::fs::write(path, buf) {
            eprintln!("could not write {path}: {e}");
        }
    }

    /// Write everything taken so far, plus the calibration, as CSV.
    fn write_csv(&self, path: &str) {
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
        s.push_str("seq,round,slot,workload,t_ns,ns\n");
        for (seq, x) in self.log.iter().enumerate() {
            let _ = writeln!(
                s,
                "{seq},{},{},{},{},{:.0}",
                x.round, x.slot, x.workload, x.t_ns, x.ns
            );
        }
        if let Err(e) = std::fs::write(path, s) {
            eprintln!("could not write {path}: {e}");
        }
    }
}

/// A recording, as read back.
pub struct Recording {
    pub iters: HashMap<String, usize>,
    /// In the order they were taken.
    pub samples: Vec<Sample>,
}

/// Parse a recording, binary or CSV, told apart by what is actually in the
/// file rather than by its name.
pub fn read(path: &str) -> Recording {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("could not read {path}: {e}"));
    if bytes.starts_with(b"LABBIN1\n") {
        return read_bin(path, &bytes);
    }
    read_csv(path, &String::from_utf8_lossy(&bytes))
}

fn read_bin(path: &str, bytes: &[u8]) -> Recording {
    let split = bytes
        .windows(5)
        .position(|w| w == b"DATA\n")
        .unwrap_or_else(|| panic!("{path}: binary recording has no DATA marker"));
    let head = String::from_utf8_lossy(&bytes[..split]);
    let mut iters = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut epoch: u128 = 0;
    for line in head.lines() {
        if let Some(rest) = line.strip_prefix("# iters ") {
            let mut f = rest.split_whitespace();
            if let (Some(name), Some(n)) = (f.next(), f.next()) {
                if let Ok(n) = n.parse() {
                    iters.insert(name.to_string(), n);
                    order.push(name.to_string());
                }
            }
        } else if let Some(rest) = line.strip_prefix("# epoch ") {
            epoch = rest.trim().parse().unwrap_or(0);
        }
    }

    let body = &bytes[split + 5..];
    let mut samples = Vec::with_capacity(body.len() / 18);
    for r in body.chunks_exact(18) {
        let round = u32::from_le_bytes([r[0], r[1], r[2], r[3]]) as usize;
        let slot = r[4] as usize;
        let widx = r[5] as usize;
        let off = u64::from_le_bytes([r[6], r[7], r[8], r[9], r[10], r[11], r[12], r[13]]);
        let ns = u32::from_le_bytes([r[14], r[15], r[16], r[17]]) as f64;
        let Some(workload) = order.get(widx) else {
            panic!(
                "{path}: sample names workload {widx}, but the header lists {}",
                order.len()
            )
        };
        samples.push(Sample {
            round,
            slot,
            workload: workload.clone(),
            t_ns: epoch + off as u128,
            ns,
        });
    }
    Recording { iters, samples }
}

/// Parse a CSV recording: `# iters <name> <n>` lines, then the sample rows.
fn read_csv(path: &str, text: &str) -> Recording {
    let _ = path;
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
        if let (Ok(round), Ok(slot), Ok(t_ns), Ok(ns)) =
            (f[1].parse(), f[2].parse(), f[4].parse(), f[5].parse())
        {
            samples.push(Sample {
                round,
                slot,
                workload: f[3].to_string(),
                t_ns,
                ns,
            });
        }
    }
    Recording { iters, samples }
}
