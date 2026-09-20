//! The recording format, and reading it back.
//!
//! `LABBIN4`. A recording is the product of this lab: the runner measures, writes one of
//! these, and every question about algorithms is then arithmetic over it. So
//! the format has to be cheap to write at a million samples a second, cheap
//! to read a hundred million rows of, and self-describing enough that `head`
//! tells you what a file holds.

use std::collections::HashMap;

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

/// Writes a recording as it is taken.
///
/// Encoded and flushed as samples arrive, rather than buffered whole and
/// written at the end. The in-memory form of a sample is about twenty times
/// its encoded size - a `String` name and its heap allocation against a byte
/// and a varint - so a cheap composition running its full slice would hold
/// some 47GB against a 2.4GB file. That is not a hypothetical: it is what
/// killed the first attempt at this collection, and it failed in the worst
/// way, dying after hours of measuring with nothing written.
///
/// Streaming also takes a heap allocation per sample out of the measuring
/// loop, since a name no longer has to be copied to be recorded.
pub struct Timing {
    pub rungs: HashMap<String, RungMeta>,
    out: Option<std::io::BufWriter<std::fs::File>>,
    path: String,
    buf: Vec<u8>,
    pub written: usize,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            rungs: HashMap::new(),
            out: None,
            path: String::new(),
            buf: Vec::with_capacity(16),
            written: 0,
        }
    }
}

impl Timing {
    pub fn new() -> Timing {
        Timing::default()
    }

    /// Write the header and begin streaming samples.
    ///
    /// Call once `rungs` is complete: the header names every rung, and the
    /// one-byte index on each sample is a position in that list, so nothing
    /// can be recorded before it is fixed.
    ///
    /// Returns the index of each rung, in the same order, so a caller can
    /// resolve a name once rather than per sample.
    pub fn open(&mut self, path: &str) -> HashMap<String, u8> {
        use std::fmt::Write as _;
        let mut names: Vec<&String> = self.rungs.keys().collect();
        names.sort();
        if names.len() > 256 {
            eprintln!("{path}: more than 256 rung names; not writing");
            return HashMap::new();
        }
        let idx: HashMap<String, u8> = names
            .iter()
            .enumerate()
            .map(|(i, n)| ((*n).clone(), i as u8))
            .collect();

        let mut head = String::new();
        head.push_str("LABBIN4\n");
        for n in &names {
            let m = &self.rungs[*n];
            let _ = writeln!(head, "# rung {n} {} {:e}", m.n, m.overhead_ns);
        }
        head.push_str("DATA\n");

        match std::fs::File::create(path) {
            Ok(f) => {
                use std::io::Write as _;
                let mut w = std::io::BufWriter::with_capacity(1 << 20, f);
                if let Err(e) = w.write_all(head.as_bytes()) {
                    eprintln!("could not write {path}: {e}");
                }
                self.out = Some(w);
                self.path = path.to_string();
            }
            Err(e) => eprintln!("could not create {path}: {e}"),
        }
        idx
    }

    /// Time one batch at rung `idx` and append it.
    ///
    /// The index rather than the name, so the measuring loop neither hashes
    /// nor allocates per sample.
    pub fn time(&mut self, idx: u8, f: impl FnOnce() -> f64) -> f64 {
        let ns = f();
        self.buf.clear();
        self.buf.push(idx);
        put_leb128(&mut self.buf, ns.round().max(0.0) as u64);
        if let Some(w) = self.out.as_mut() {
            use std::io::Write as _;
            if let Err(e) = w.write_all(&self.buf) {
                eprintln!("could not write {}: {e}", self.path);
                self.out = None;
            }
        }
        self.written += 1;
        ns
    }

    /// Flush and close. Reported here rather than left to `Drop`, so a
    /// failure to write is seen rather than swallowed.
    pub fn finish(&mut self) {
        if let Some(mut w) = self.out.take() {
            use std::io::Write as _;
            if let Err(e) = w.flush() {
                eprintln!("could not flush {}: {e}", self.path);
            }
        }
    }
}

impl Drop for Timing {
    fn drop(&mut self) {
        self.finish();
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
    /// Read a recording, or `None` if the file is not one.
    ///
    /// An interrupted collection leaves a file that was opened and never
    /// written to, so this has to be a normal outcome rather than a panic:
    /// globbing a directory of recordings should not die on the one the run
    /// was killed during. A *truncated* file is fine and is read as far as
    /// it goes - the records are self-delimiting, so a half-written last one
    /// is simply dropped.
    pub fn load(path: &str) -> Option<Run> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("skipping {path}: {e}");
                return None;
            }
        };
        if !bytes.starts_with(b"LABBIN4\n") {
            // The magic moves with the layout every time, because each of
            // these would otherwise parse as the next without complaining:
            // LABBIN2 had an extra header number, LABBIN3 an extra byte per
            // record, and either would misread every sample after the first
            // rather than fail.
            eprintln!(
                "skipping {path}: not a LABBIN4 recording ({} bytes)",
                bytes.len()
            );
            return None;
        }
        let Some(split) = bytes.windows(5).position(|w| w == b"DATA\n") else {
            eprintln!("skipping {path}: no DATA marker");
            return None;
        };
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
                eprintln!(
                    "{path}: a sample names rung {widx}, but the header lists {}; \
                     stopping there",
                    order.len()
                );
                break;
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
        Some(Run {
            names,
            by_name,
            rungs,
        })
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
