//! How a sample is timed, and how many calls and inputs it takes.
//!
//! Every sample of an alternative is timed as consecutive **laps** of calls:
//! a warm-up lap whose time is thrown away, then a short lap and a long one
//! (see [`SHAPE`]). The warm-up absorbs whatever the sample before it, or
//! the machine, left behind - a cold cache, a sleeping vector unit, a clock
//! that has not caught up - so the laps that count describe the function
//! running steadily, whatever ran before it. Reading the clock between laps
//! costs something fixed, and may set off something else; each lap carries
//! it once, so the long lap less the short one has none of it.
//!
//! A group's laps all last about the same time, whatever each alternative's
//! speed, so that they meet the same stretch of the machine's behaviour.
//! That fixes how many calls each lap holds, and so how many inputs a sample
//! needs - and inputs are made before the timing starts but held until it is
//! over, so a quick function on an input that is slow to make would want more
//! than there is time or memory for. [`plan`] gives each alternative the laps
//! it can have, and [`calibrate`] finds out what they cost.

use super::*;
use crate::input_group::{Entry, Inputs};
use std::time::{Duration, Instant};

/// The base length of a lap, in nanoseconds.
///
/// Long enough, on the machine this was worked out on, for what lingers from
/// before a sample - a cold cache, a sleeping vector unit, a clock that has
/// not caught up - to have settled by the end of its warm-up, so that a
/// function's result no longer depended on its neighbours; and short enough
/// that the eleven units of a sample cost little. It is a constant, not
/// something found out at run time: a machine where more lingers would want
/// longer laps.
const LAP_UNIT_NS: f64 = 1e6;

/// A sample's laps, in units: warm-up, short, long.
///
/// The estimate is the long lap less the short one, divided by the
/// difference in their calls. The long lap is nine times the short one
/// because the subtraction costs precision: against simply adding the laps,
/// the variance grows by (K+1)²/(K-1)² for a long lap K times the short one,
/// which is 9 at K=2 and 1.56 at K=9.
const SHAPE: [usize; 3] = [1, 1, 9];

/// The sample used instead when [`SHAPE`] cannot be had: a warm-up and one
/// lap. Without a second lap there is nothing to subtract, so what the clock
/// read costs stays in, which for laps this long, or this costly to supply,
/// is small beside them.
const SHORT_SHAPE: [usize; 3] = [1, 1, 0];

/// Laps this many base units long, or longer, use [`SHORT_SHAPE`]. Such a
/// lap exists only because some alternative's single call is that long, so
/// whatever reading the clock costs is a negligible part of it, and the long
/// lap would cost nine more of that call every round.
///
/// It is also the limit to how long a group's laps are made: a quick function
/// grouped with a slow one is not made to run for as long as the slow one's
/// call, and beyond this the machine's moods are no longer something that
/// matching laps could do anything about.
pub(crate) const LONG_LAP_UNITS: f64 = 10.0;

/// An alternative whose single call is this many base units long, or longer,
/// is given no warm-up call.
///
/// The warm-up exists to absorb what lingers from before, which settles
/// within about a base unit. Against a call this long that is under one part
/// in a hundred, so a warm-up would only double the cost of a function that
/// is already slow.
const NO_WARMUP_UNITS: f64 = 100.0;

/// The most a sample's inputs may cost to prepare, in nanoseconds.
///
/// Preparing inputs is not timed, but it is not free: it comes out of the
/// budget, and every input prepared is held until the sample has run. A
/// function that is quick to call on an input that is slow to make - a cheap
/// query on a freshly built vector - would otherwise need a million inputs to
/// fill one lap. Bounding what a sample may spend making them bounds both
/// that time and, without having to see inside the inputs, their memory; the
/// laps are then as long as this allows, which for such a function is shorter
/// than the base unit.
const MAX_PREPARATION_NS: f64 = 5e6;

/// The most inputs a sample may take, whatever else is said: a backstop for a
/// function and input both so trivial that the optimiser deletes the work,
/// when nothing else grows with the number of calls.
const MAX_INPUTS: usize = 2_000_000;

/// The most memory one copy of a sample's inputs may take. A sample holds
/// the round's inputs and, for an alternative that may change its input, a
/// clone of them.
///
/// It limits how many inputs a sample takes, and cannot make one input
/// smaller: a sample takes at least a warm-up's and a lap's, which is two
/// inputs (one, for a call so slow that it has no warm-up). Where a single
/// input is over this, a sample takes two of it, and a clone of each,
/// whatever this says.
const MAX_SAMPLE_BYTES: usize = 16 * 1024 * 1024;

/// What stands in for the heap behind an input, in bytes, where the counting
/// allocator is not installed to say. `size_of` sees an owning type such as
/// `Vec` as its handle only, so this is enough for the smallest allocation
/// and not for what a large input really takes. Inputs that are large are
/// held to what can be done with them in a sample's time instead: how long
/// they take to make and clone ([`MAX_PREPARATION_NS`]), and how long a call
/// takes to touch them, which together bound how much of them a sample
/// touches to what the machine can move in that time. What this does not see
/// is memory reserved and not touched, such as a large zeroed buffer of which
/// a call writes a page: a megabyte of those, held for each of thousands of
/// calls, is address space and little else.
const HEAP_ALLOWANCE: usize = 32;

/// The most inputs in the pool an alternative that reuses its input goes
/// round. Enough that the inputs a randomised benchmark draws are a fair
/// sample of what it would see, and that a long run of them is not one the
/// machine can learn the answers to.
const MAX_POOL_INPUTS: usize = 1 << 16;

/// The most memory such a pool may take: a few megabytes, whatever the inputs
/// are.
const MAX_POOL_BYTES: usize = 8 * 1024 * 1024;

/// The most such a pool may cost to make, in nanoseconds. It is made afresh
/// every round, and is meant to be a small part of one.
const MAX_POOL_PREPARATION_NS: f64 = 1e6;

/// The most calls a probe of an alternative that reuses its inputs makes: it
/// consumes none, so only this and the clock bound it.
const MAX_REUSED_CALLS: usize = 1 << 28;

/// How many inputs of `input_bytes` each a pool may hold: the most that
/// [`MAX_POOL_INPUTS`] and [`MAX_POOL_BYTES`] allow, and always at least one.
fn pool_limit(input_bytes: usize) -> usize {
    match input_bytes {
        0 => MAX_POOL_INPUTS,
        bytes => MAX_POOL_INPUTS.min(MAX_POOL_BYTES / bytes),
    }
    .max(1)
}

/// What calibration found out about a group.
pub(crate) struct Calibration {
    /// For each alternative, what one call costs in nanoseconds.
    per_call: Vec<f64>,
    /// For each alternative, what making one input, and cloning it for that
    /// alternative, costs in nanoseconds.
    prep: Vec<f64>,
    /// What one input takes in memory: itself, and what it owns.
    input_bytes: usize,
    /// How many inputs one sample may use when each call has one of its own.
    input_cap: usize,
    /// How many iterations each alternative's probes ran, which count towards
    /// [`Timing::iterations`](crate::Timing::iterations) even though their
    /// timings are discarded.
    pub(crate) probed: Vec<u64>,
}

impl Calibration {
    /// What making an input, and cloning it for an alternative, costs, taken
    /// once for the whole group.
    ///
    /// It is a property of the input, not of any alternative, so every
    /// alternative that is given a fresh input to a call is held to the same
    /// figure. Measured separately it would differ from one alternative to
    /// the next by nothing but noise - the first to be calibrated is the
    /// coldest and measures the dearest - and the laps it allowed would differ
    /// with it. Identical code on laps of different lengths does not measure
    /// alike, and the baseline is calibrated first.
    ///
    /// The smallest is used, since what noise and a cold start do is add.
    fn group_prep(&self, reuse: &[bool]) -> f64 {
        self.prep
            .iter()
            .zip(reuse)
            .filter(|(_, &pooled)| !pooled)
            .map(|(&make, _)| make)
            .fold(f64::INFINITY, f64::min)
    }

    /// Whether samples laid out as `shape` can be afforded, one input to a
    /// call, by every alternative that is not given a pool.
    ///
    /// Not when the inputs are so large, or so slow to make, that the units of
    /// a sample would not fit in what a sample may use even at one call to a
    /// unit.
    fn affords(&self, shape: [usize; 3], reuse: &[bool]) -> bool {
        let units: usize = shape.iter().sum();
        let make = self.group_prep(reuse);
        // An alternative given a pool needs one input however long its laps.
        reuse.iter().all(|&pooled| pooled)
            || (self.input_cap >= units && make * units as f64 <= MAX_PREPARATION_NS)
    }
}

/// What one input takes in memory.
///
/// Its own size, and what it owns on the heap. The heap is only visible when
/// the counting allocator is installed, and then it is measured: a few inputs
/// are made under it (as many as take ten milliseconds, up to eight) and the
/// largest kept, since inputs need not all be the same size. Without it, an
/// allowance stands in. An input with no size and nothing on the heap costs
/// no memory at all.
fn input_bytes<I>(inputs: &mut Inputs<I>) -> usize {
    let inline = std::mem::size_of::<I>();
    if !crate::alloc::installed() {
        return if inline == 0 {
            0
        } else {
            inline + HEAP_ALLOWANCE
        };
    }
    let started = Instant::now();
    let mut heap = 0;
    for _ in 0..8 {
        let (input, held) = crate::alloc::measure(|| inputs.make_one());
        drop(input);
        heap = heap.max(held.net_allocated_bytes.max(0) as usize);
        // An input that is slow to make is not one to make eight of.
        if started.elapsed() > Duration::from_millis(10) {
            break;
        }
    }
    inline + heap
}

/// What probing one alternative found out.
struct Probe {
    per_call: f64,
    prep: f64,
    /// Iterations run, probes included.
    ran: u64,
}

/// What every probe is held to.
struct Limits {
    /// How long a probe aims to time: the base unit.
    target_ns: f64,
    /// The most a probe may cost in all, setup as well as timing. When a
    /// benchmark's cost is optimised away its timed part never grows while
    /// untimed input construction does, unboundedly.
    probe_ceiling_ns: f64,
    /// The most inputs a probe of a fresh-input alternative may use.
    input_cap: usize,
    /// Whether the group is a lone alternative, which runs on the round's own
    /// inputs and needs no clone.
    alone: bool,
    /// What one input takes, to size a pool by.
    input_bytes: usize,
}

/// For each alternative, what one call and one input cost, found by timing
/// batches of growing size until one lasts a lap's base unit.
///
/// Each alternative's first call is run untimed: it is the coldest call there
/// will be, and believing it would make a first-touch cost look like the
/// function's own. Calibration yields between probes, so that in a suite it
/// is interleaved like everything else - and holds no inputs across the
/// yield, for the reason given at the round's own.
pub(crate) async fn calibrate<I>(
    inputs: &mut Inputs<I>,
    entries: &mut [Entry<I>],
    clock: &Clock,
) -> Calibration {
    let bytes = input_bytes(inputs);
    let alone = entries.len() == 1;
    let copies = if alone { 1 } else { 2 };
    let limits = Limits {
        target_ns: LAP_UNIT_NS,
        probe_ceiling_ns: (clock.budget() / 100)
            .max(Duration::from_millis(5))
            .as_secs_f64()
            * 1e9,
        input_cap: match bytes {
            0 => MAX_INPUTS,
            bytes => MAX_INPUTS.min(MAX_SAMPLE_BYTES / (bytes * copies)),
        }
        .max(1),
        alone,
        input_bytes: bytes,
    };
    let mut found = Calibration {
        per_call: Vec::new(),
        prep: Vec::new(),
        input_bytes: bytes,
        input_cap: limits.input_cap,
        probed: Vec::new(),
    };
    for entry in entries.iter_mut() {
        let probe = probe(entry, inputs, &limits, clock).await;
        found.per_call.push(probe.per_call);
        found.prep.push(probe.prep);
        found.probed.push(probe.ran);
    }
    found
}

/// Probe one alternative: see [`calibrate`].
async fn probe<I>(
    entry: &mut Entry<I>,
    inputs: &mut Inputs<I>,
    limits: &Limits,
    clock: &Clock,
) -> Probe {
    let reuse = entry.reuse;
    let mut n = 1usize;
    let mut ran = 0u64;
    let mut warm = true;
    // What making one input cost in the last probe, to hold the next to the
    // same bounds a sample is: the heap behind an input is not always
    // visible, and what it costs to make is.
    let mut last_make = 0.0f64;
    loop {
        let started = Instant::now();
        // An alternative that reuses its inputs goes round a pool of them, so
        // a probe of any length needs no more than the pool.
        let made = if reuse {
            let affordable = if last_make > 0.0 {
                (MAX_POOL_PREPARATION_NS / last_make) as usize
            } else {
                usize::MAX
            };
            n.min(pool_limit(limits.input_bytes)).min(affordable).max(1)
        } else {
            n
        };
        inputs.fill(made);
        let timed_ns = entry.alt.batch(inputs.take(made, limits.alone || reuse), n);
        let total_ns = started.elapsed().as_secs_f64() * 1e9;
        let made_ns = (total_ns - timed_ns).max(0.0);
        last_make = made_ns / made as f64;
        ran += n as u64;
        if std::mem::take(&mut warm) {
            // The untimed first call: run, and not believed.
            continue;
        }
        let at_limit = if reuse {
            n >= MAX_REUSED_CALLS
        } else {
            n >= limits.input_cap
        };
        // Inputs that are slow to make end the growth when making them has
        // cost as much as a sample may spend on it. A pool is made once and
        // gone round, so its probes are not bounded this way.
        let too_dear = !reuse && made_ns >= MAX_PREPARATION_NS;
        let mut finished = timed_ns >= limits.target_ns
            || total_ns >= limits.probe_ceiling_ns
            || at_limit
            || too_dear
            || clock.exhausted();
        if !finished {
            inputs.release();
            finished = !clock.yield_now().await;
        }
        if finished {
            return Probe {
                per_call: timed_ns / n as f64,
                prep: last_make,
                ran,
            };
        }
        // Grow towards the target, more gently as the timed part nears it,
        // the whole probe nears its ceiling, or making the inputs nears what
        // a sample may spend on that.
        let by_time = (limits.target_ns / timed_ns.max(1.0)).clamp(2.0, 100.0);
        let by_ceiling = (limits.probe_ceiling_ns / total_ns.max(1.0)).max(1.0);
        let by_making = if reuse {
            f64::INFINITY
        } else {
            (MAX_PREPARATION_NS / made_ns.max(1.0)).max(1.0)
        };
        let growth = by_time.min(by_ceiling).min(by_making);
        n = ((n as f64 * growth).ceil() as usize)
            .max(n + 1)
            .min(if reuse {
                MAX_REUSED_CALLS
            } else {
                limits.input_cap
            });
    }
}

/// How one alternative's sample is laid out.
pub(crate) struct Plan {
    /// Calls in each lap.
    pub(crate) laps: [usize; 3],
    /// Inputs it is run on: one for each call, or for an alternative that
    /// reuses its input, the pool the calls go round.
    pub(crate) inputs: usize,
}

impl Plan {
    pub(crate) fn calls(&self) -> usize {
        self.laps.iter().sum()
    }
}

/// Each alternative's laps and inputs, from what calibration found; `reuse`
/// says which alternatives are given a pool.
///
/// A lap lasts the base unit for everyone - or as long as the group's slowest
/// call if that is longer, up to [`LONG_LAP_UNITS`] - which fixes how many
/// calls it holds, as far as the inputs allow. An alternative that is given
/// one of its own for every call is held to what a sample's inputs may be: so
/// many in all, and no more than [`MAX_PREPARATION_NS`] to make. One that
/// reuses its input needs only a pool, and is held to that pool being small:
/// at most [`MAX_POOL_INPUTS`], taking at most [`MAX_POOL_BYTES`] and
/// [`MAX_POOL_PREPARATION_NS`] to make. Its laps are not otherwise limited:
/// calls on an input it hands back as it found it cost nothing to supply.
pub(crate) fn plan(cal: &Calibration, reuse: &[bool]) -> Vec<Plan> {
    let longest = cal.per_call.iter().copied().fold(LAP_UNIT_NS, f64::max);
    let lap_ns = longest.min(LONG_LAP_UNITS * LAP_UNIT_NS);
    let shape = if lap_ns >= LONG_LAP_UNITS * LAP_UNIT_NS || !cal.affords(SHAPE, reuse) {
        SHORT_SHAPE
    } else {
        SHAPE
    };
    let group_make = cal.group_prep(reuse);
    cal.per_call
        .iter()
        .zip(&cal.prep)
        .zip(reuse)
        .map(|((&per_call, &own_make), &reuse)| {
            // A call long enough to need no warm-up is not given one.
            let shape = if per_call >= NO_WARMUP_UNITS * LAP_UNIT_NS {
                [0, 1, 0]
            } else {
                shape
            };
            let units: usize = shape.iter().sum();
            // Floored so that a call the optimiser has deleted cannot ask for
            // an endless lap.
            let wanted = (lap_ns / per_call.max(0.1)).round().max(1.0) as usize;
            if reuse {
                let by_time = if own_make > 0.0 {
                    (MAX_POOL_PREPARATION_NS / own_make) as usize
                } else {
                    usize::MAX
                };
                let pool = wanted
                    .saturating_mul(units)
                    .min(pool_limit(cal.input_bytes))
                    .min(by_time)
                    .max(1);
                Plan {
                    laps: shape.map(|s| s * wanted),
                    inputs: pool,
                }
            } else {
                let by_time = if group_make > 0.0 {
                    (MAX_PREPARATION_NS / (group_make * units as f64)) as usize
                } else {
                    usize::MAX
                };
                let unit = wanted.min(by_time).clamp(1, (cal.input_cap / units).max(1));
                Plan {
                    laps: shape.map(|s| s * unit),
                    inputs: units * unit,
                }
            }
        })
        .collect()
}
