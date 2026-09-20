//! The recording format, and reading it back.
//!
//! `LABBIN4`. A recording is the product of this lab: the runner measures, writes one of
//! these, and every question about algorithms is then arithmetic over it. So
//! the format has to be cheap to write at a million samples a second, cheap
//! to read a hundred million rows of, and self-describing enough that `head`
//! tells you what a file holds.

use std::collections::HashMap;

/// One timing, as it is kept in memory while a slice runs.
///
/// Two fields. Three others have been dropped, each because the file
/// already said it:
///
///   - `t_offset` cost eight bytes a sample to support one derived
///     quantity, the per-sample overhead, which the recorder now measures
///     per rung and writes once in the header.
///   - `round` and `slot` are position. The runner takes exactly one sample
///     per workload per round, in slot order, and never cuts a round short
///     - the deadline and the rung cap are both tested between rounds - so
///     for sample `i` of a round holding `W` workloads, `slot` is `i % W`
///     and `round` is `i / W`. Storing either wrote down what the layout
///     already told us.
#[derive(Clone, Debug)]
pub struct Sample {
    pub workload: String,
    /// Whole-batch time in ns.
    pub ns: f64,
}

/// What the header says about one rung.
#[derive(Clone, Copy, Debug)]
pub struct RungMeta {
    /// Batch size, in iterations.
    pub n: usize,
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
    pub fn time(&mut self, name: &str, f: impl FnOnce() -> f64) -> f64 {
        let ns = f();
        self.log.push(Sample {
            workload: name.to_string(),
            ns,
        });
        ns
    }

    /// Write the recording.
    ///
    /// Workload index, then the batch time in nanoseconds as LEB128.
    ///
    /// A single file holds batches from 42ns to 12.5ms, a 300000-fold range,
    /// so no fixed integer width suits all of it. A previous version gave
    /// each rung its own sub-nanosecond scale, which was false precision:
    /// the clock delivers integer nanoseconds, so a finer unit stores
    /// resolution the measurement never had. It also had to clamp anything
    /// past its rung's range, discarding the size of exactly the outliers
    /// worth keeping.
    ///
    /// A variable-length integer costs a byte for a batch under 128ns, two
    /// under 16us, three under 2.1ms, four beyond - and since rungs are
    /// drawn weighted by inverse cost, the cheap short-batch rungs are where
    /// most of the samples are. Nothing is clamped and nothing is scaled.
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
        head.push_str("LABBIN4\n");
        for n in &names {
            let m = &self.rungs[*n];
            let _ = writeln!(
                head,
                "# rung {n} {} {:e}",
                m.n, m.overhead_ns
            );
        }
        head.push_str("DATA\n");

        let mut buf: Vec<u8> = Vec::with_capacity(head.len() + self.log.len() * 4);
        buf.extend_from_slice(head.as_bytes());
        for x in &self.log {
            let Some(&i) = idx.get(x.workload.as_str()) else {
                continue;
            };
            buf.push(i);
            put_leb128(&mut buf, x.ns.round().max(0.0) as u64);
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
        if !bytes.starts_with(b"LABBIN4\n") {
            // The magic moves with the layout every time, because each of
            // these would otherwise parse as the next without complaining:
            // LABBIN2 had an extra header number, LABBIN3 an extra byte per
            // record, and either would misread every sample after the first
            // rather than fail.
            panic!("{path}: not a LABBIN4 recording");
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
            if f.len() < 3 {
                continue;
            }
            let (Ok(n), Ok(overhead_ns)) = (f[1].parse(), f[2].parse()) else {
                continue;
            };
            rungs.insert(f[0].to_string(), RungMeta { n, overhead_ns });
            order.push(f[0].to_string());
        }

        let body = &bytes[split + 5..];
        let mut by_name: HashMap<String, Vec<f64>> = HashMap::new();
        let mut i = 0usize;
        while i + 1 < body.len() {
            let widx = body[i] as usize;
            i += 1;
            let Some(ns) = get_leb128(body, &mut i) else {
                break;
            };
            let Some(workload) = order.get(widx) else {
                panic!(
                    "{path}: a sample names rung {widx}, but the header lists {}",
                    order.len()
                )
            };
            let m = &rungs[workload];
            // Per iteration from here on. Calibration happens once, at
            // whatever clock speed prevailed then, so batch sizes differ
            // between runs; comparing batch durations across runs inherits
            // all of that.
            by_name
                .entry(workload.clone())
                .or_default()
                .push(ns as f64 / m.n as f64);
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

/// Append `v` as LEB128: seven bits a byte, high bit set while more follow.
fn put_leb128(buf: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            buf.push(byte);
            return;
        }
        buf.push(byte | 0x80);
    }
}

/// Read a LEB128 from `buf` at `i`, advancing `i`. `None` if it runs off the
/// end or is longer than a `u64` can hold.
fn get_leb128(buf: &[u8], i: &mut usize) -> Option<u64> {
    let mut v: u64 = 0;
    let mut shift = 0u32;
    loop {
        let byte = *buf.get(*i)?;
        *i += 1;
        v |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}
