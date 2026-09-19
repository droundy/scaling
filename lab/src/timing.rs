//! The recording format, and reading it back.
//!
//! A recording is the product of this lab: the runner measures, writes one of
//! these, and every question about algorithms is then arithmetic over it. So
//! the format has to be cheap to write at a million samples a second, cheap
//! to read a hundred million rows of, and self-describing enough that `head`
//! tells you what a file holds.

use std::collections::HashMap;

/// One timing, as it is kept in memory while a slice runs.
///
/// Three fields, and the two that used to be here are gone. `round` was
/// redundant with position - the runner takes exactly one sample per
/// workload per round - and `t_offset` cost eight bytes a sample to support
/// one measurement, the per-sample overhead, which the recorder now makes
/// once per rung and writes in the header instead.
#[derive(Clone, Debug)]
pub struct Sample {
    /// Position within the round. Kept because order is data: a memory-heavy
    /// neighbour immediately before a payload leaves that payload's cache
    /// cold, so anything that wants to ask about position still can.
    pub slot: usize,
    pub workload: String,
    /// Whole-batch time in ns.
    pub ns: f64,
}

/// What the header says about one rung.
#[derive(Clone, Copy, Debug)]
pub struct RungMeta {
    /// Batch size, in iterations.
    pub n: usize,
    /// Nanoseconds per stored unit; see [`Timing::write`].
    pub scale_ns: f64,
    /// Wall-clock cost of taking one sample beyond the batch itself: the
    /// harness loop, and whatever the workload does to prepare its inputs.
    ///
    /// Measured by the recorder, per rung, because it is not one number - a
    /// workload that prepares an input per iteration pays that per iteration
    /// too, so for those it grows with the batch.
    pub overhead_ns: f64,
}

pub struct Timing {
    pub rungs: HashMap<String, RungMeta>,
    /// Every timing taken, in the order taken.
    pub log: Vec<Sample>,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            rungs: HashMap::new(),
            log: Vec::new(),
        }
    }
}

impl Timing {
    pub fn new() -> Timing {
        Timing::default()
    }

    /// Time one batch and record it.
    pub fn time(&mut self, slot: usize, name: &str, f: impl FnOnce() -> f64) -> f64 {
        let ns = f();
        self.log.push(Sample {
            slot,
            workload: name.to_string(),
            ns,
        });
        ns
    }

    /// Write the recording.
    ///
    /// Four bytes a sample: slot, workload index, and the batch time as a
    /// `u16` count of a unit declared per rung in the header.
    ///
    /// A single file holds batches from 42ns to 12.5ms - a 300000-fold range
    /// - so no one integer width works for all of them at a fixed
    /// resolution: `u16` nanoseconds reaches 65us and `u24` reaches 16.7ms,
    /// which barely covers the slowest rung and leaves nothing for an
    /// outlier. But the rung's nominal size is known when it is written, so
    /// the scale can be per rung, sized to cover `nominal + 20us` - room for
    /// a few scheduler ticks on top of the expected batch.
    ///
    /// That is finer than the 1ns integers it replaces, not coarser: 1ns is
    /// 2.4% of a 42ns batch, where this gives 0.7% there and better than
    /// 0.01% everywhere above a microsecond.
    ///
    /// The header stays text so `head` still tells you what a file holds.
    pub fn write(&self, path: &str) {
        use std::fmt::Write as _;
        let mut names: Vec<&String> = self.rungs.keys().collect();
        names.sort();
        if names.len() > 256 {
            eprintln!("{path}: more than 256 rung names; not writing");
            return;
        }
        let idx: HashMap<&str, u8> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i as u8))
            .collect();

        let mut head = String::new();
        head.push_str("LABBIN2\n");
        for n in &names {
            let m = &self.rungs[*n];
            let _ = writeln!(
                head,
                "# rung {n} {} {:e} {:e}",
                m.n, m.scale_ns, m.overhead_ns
            );
        }
        head.push_str("DATA\n");

        let mut buf: Vec<u8> = Vec::with_capacity(head.len() + self.log.len() * 4);
        buf.extend_from_slice(head.as_bytes());
        let mut saturated = 0usize;
        for x in &self.log {
            let m = match self.rungs.get(&x.workload) {
                Some(m) => m,
                None => continue,
            };
            buf.push(x.slot as u8);
            buf.push(idx[x.workload.as_str()]);
            let units = (x.ns / m.scale_ns).round().max(0.0);
            if units > u16::MAX as f64 {
                saturated += 1;
            }
            buf.extend_from_slice(&(units.min(u16::MAX as f64) as u16).to_le_bytes());
        }
        // Saying so rather than letting it pass, because a saturated sample
        // is an outlier whose size has been thrown away - fine for a trimmed
        // estimate, not fine for anything studying the tail.
        if saturated > 0 {
            eprintln!(
                "{path}: {saturated} of {} samples ran past their rung's scale and were clamped",
                self.log.len()
            );
        }
        if let Err(e) = std::fs::write(path, buf) {
            eprintln!("could not write {path}: {e}");
        }
    }
}

/// A recording, as read back.
///
/// Per-iteration timings by rung, in the order taken.
#[derive(Debug, Clone)]
pub struct Run {
    pub names: Vec<String>,
    /// Per-rung timings in the order taken, per iteration.
    ///
    /// Only this, and not also a flat list of `Sample`. Keeping both meant
    /// holding a `String` per sample alongside the numbers, which on a
    /// recording of a few million samples is most of the memory and is
    /// read by nothing.
    by_name: HashMap<String, Vec<f64>>,
    pub rungs: HashMap<String, RungMeta>,
}

impl Run {
    pub fn load(path: &str) -> Run {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("could not read {path}: {e}"));
        if !bytes.starts_with(b"LABBIN2\n") {
            panic!("{path}: not a LABBIN2 recording");
        }
        let split = bytes
            .windows(5)
            .position(|w| w == b"DATA\n")
            .unwrap_or_else(|| panic!("{path}: no DATA marker"));
        let head = String::from_utf8_lossy(&bytes[..split]);
        let mut rungs: HashMap<String, RungMeta> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for line in head.lines() {
            let Some(rest) = line.strip_prefix("# rung ") else {
                continue;
            };
            let f: Vec<&str> = rest.split_whitespace().collect();
            if f.len() < 4 {
                continue;
            }
            let (Ok(n), Ok(scale_ns), Ok(overhead_ns)) =
                (f[1].parse(), f[2].parse(), f[3].parse())
            else {
                continue;
            };
            rungs.insert(
                f[0].to_string(),
                RungMeta {
                    n,
                    scale_ns,
                    overhead_ns,
                },
            );
            order.push(f[0].to_string());
        }

        let body = &bytes[split + 5..];
        let mut by_name: HashMap<String, Vec<f64>> = HashMap::new();
        for r in body.chunks_exact(4) {
            let Some(workload) = order.get(r[1] as usize) else {
                panic!(
                    "{path}: a sample names rung {}, but the header lists {}",
                    r[1],
                    order.len()
                )
            };
            let m = &rungs[workload];
            let units = u16::from_le_bytes([r[2], r[3]]) as f64;
            // Per iteration from here on. Calibration happens once, at
            // whatever clock speed prevailed then, so batch sizes differ
            // between runs; comparing batch durations across runs inherits
            // all of that.
            by_name
                .entry(workload.clone())
                .or_default()
                .push(units * m.scale_ns / m.n as f64);
        }

        let mut names: Vec<String> = by_name.keys().cloned().collect();
        names.sort();
        Run {
            names,
            by_name,
            rungs,
        }
    }

    pub fn get(&self, name: &str) -> &[f64] {
        self.by_name.get(name).map(|v| v.as_slice()).unwrap_or(&[])
    }
}

/// How a rung is named in a recording: the workload for rung zero, and
/// `workload@k` above it.
pub fn rung_name(name: &str, k: usize) -> String {
    if k == 0 {
        name.to_string()
    } else {
        format!("{name}@{k}")
    }
}

/// Nanoseconds of headroom above a rung's nominal batch, for choosing its
/// scale.
///
/// A scheduler tick adds about 5us to whatever batch it lands in, and the
/// headroom is absolute rather than proportional for that reason: the same
/// 5us is a rounding error on a 12ms batch and a hundredfold excursion on a
/// 42ns one, so a proportional margin would clamp exactly the outliers that
/// matter most on the rungs where they matter most.
pub const SCALE_HEADROOM_NS: f64 = 20_000.0;

/// The scale to record a rung at, given what one batch is expected to cost.
pub fn scale_for(nominal_ns: f64) -> f64 {
    ((nominal_ns + SCALE_HEADROOM_NS) / u16::MAX as f64).max(f64::MIN_POSITIVE)
}
