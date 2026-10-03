//! Counting allocations: [`Allocator`], which counts, and [`Allocations`],
//! what it counted. Both are exported at the crate root.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

/// What a candidate allocated during its call: what
/// [`Metrics::allocations`](crate::Metrics::allocations) returns, and what the
/// allocation metrics show.
///
/// Only the candidate's own call, on its own thread, is counted. Its input is
/// not, since it was handed that, and neither is what a metrics function does
/// afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct Allocations {
    /// How many times it asked for memory: allocating, and growing or
    /// shrinking an allocation, each count once.
    pub allocation_count: u64,
    /// The most memory it held at once, beyond what was already held when it
    /// started.
    pub peak_allocated_bytes: u64,
    /// How much memory it asked for in all, counting what it asked for again
    /// each time. Growing an allocation counts only the growth.
    pub total_allocated_bytes: u64,
    /// How much more memory it held when it ended than when it began: what it
    /// allocated less what it freed. That is usually what it returned, but
    /// also anything it kept some other way, and it is negative when it freed
    /// memory it was holding to begin with. Counted before its result is
    /// dropped.
    pub net_allocated_bytes: i64,
}

/// One thread's running totals. Plain cells: they are only ever touched by
/// their own thread, and are const-initialised and never dropped, which is
/// what makes them safe to use from inside an allocator - nothing here
/// allocates.
struct Counters {
    /// Bytes held now, as a difference from some earlier point, so it can go
    /// below zero when memory allocated before the point is freed.
    live: Cell<i64>,
    /// The highest `live` has been since the window began.
    peak: Cell<i64>,
    allocations: Cell<u64>,
    allocated: Cell<u64>,
}

thread_local! {
    static COUNTERS: Counters = const {
        Counters {
            live: Cell::new(0),
            peak: Cell::new(0),
            allocations: Cell::new(0),
            allocated: Cell::new(0),
        }
    };
}

thread_local! {
    /// The counts of the run whose metrics function is being called, for
    /// [`current`]. Apart from the counters above because it is only touched
    /// outside the allocator, where nothing stops it from being richer.
    static CURRENT: Cell<Option<Allocations>> = const { Cell::new(None) };
}

/// Set by the first allocation through [`Allocator`], which is how the
/// rest of the crate can tell that it is the global allocator.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Whether [`Allocator`] is the global allocator. See
/// [`Metrics::allocator_installed`](crate::Metrics::allocator_installed).
pub(crate) fn installed() -> bool {
    INSTALLED.load(Ordering::Relaxed)
}

/// The counts of the run a metrics function is being called for, if it is
/// being called for one that was counted. See
/// [`Metrics::allocations`](crate::Metrics::allocations).
pub(crate) fn current() -> Option<Allocations> {
    CURRENT.with(Cell::get)
}

/// Makes [`current`] return `stats` until it is dropped, and then what it
/// returned before.
pub(crate) struct Provided(Option<Allocations>);

pub(crate) fn provide(stats: Option<Allocations>) -> Provided {
    Provided(CURRENT.with(|current| current.replace(stats)))
}

impl Drop for Provided {
    fn drop(&mut self) {
        // Also on a panic, so that a metrics function that fails does not
        // leave its counts to be read by the next.
        CURRENT.with(|current| current.set(self.0));
    }
}

/// A global allocator that counts what a benchmark allocates, and otherwise
/// leaves everything to the system's.
///
/// A library cannot install a global allocator for the program using it, so
/// this takes one line in the benchmark binary:
///
/// ```ignore
/// #[global_allocator]
/// static ALLOC: scaling::Allocator = scaling::Allocator::new();
/// ```
///
/// After that, a metrics function marked `allocation` is given the numbers for
/// its candidate's run, and asks for the ones it wants to see with
/// [`Metrics::peak_allocated_bytes`](crate::Metrics::peak_allocated_bytes) and
/// the others beside it, or reads them with
/// [`Metrics::allocations`](crate::Metrics::allocations).
///
/// # What is counted
///
/// Every allocation that goes through Rust's global allocator, made by the
/// thread that is running the candidate. Memory the program gets some other way
/// (a memory map, a C library's own `malloc`) is not seen, and neither are other
/// threads' allocations: the counters are per thread, so a benchmark that hands
/// its work to a thread pool is counted only for what it does itself.
///
/// # What it costs
///
/// The counters run whenever the allocator is installed, not only on the run
/// that is counted, which costs a few instructions on every allocation -
/// including in a timing loop. A benchmark that allocates heavily is therefore a
/// little slower than it would be without the allocator, and the baseline slows
/// with the rest. There is no switch for it other than not installing it.
pub struct Allocator;

impl Allocator {
    /// For `static ALLOC: Allocator = Allocator::new();`.
    pub const fn new() -> Self {
        Allocator
    }
}

impl Default for Allocator {
    fn default() -> Self {
        Allocator::new()
    }
}

/// Records `bytes` more held, `asked` more requested, and one more request.
fn grew(held: i64, asked: u64) {
    if !INSTALLED.load(Ordering::Relaxed) {
        INSTALLED.store(true, Ordering::Relaxed);
    }
    // `try_with`, not `with`: it must not panic however late in a thread's
    // life an allocation comes.
    let _ = COUNTERS.try_with(|c| {
        let live = c.live.get() + held;
        c.live.set(live);
        if live > c.peak.get() {
            c.peak.set(live);
        }
        c.allocations.set(c.allocations.get() + 1);
        c.allocated.set(c.allocated.get() + asked);
    });
}

fn shrank(held: i64) {
    let _ = COUNTERS.try_with(|c| c.live.set(c.live.get() - held));
}

// SAFETY: every method defers to `System`, which is a correct allocator, and
// only adds bookkeeping that does not allocate and cannot unwind.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            grew(layout.size() as i64, layout.size() as u64);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            grew(layout.size() as i64, layout.size() as u64);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        shrank(layout.size() as i64);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            let old = layout.size();
            if new_size >= old {
                grew((new_size - old) as i64, (new_size - old) as u64);
            } else {
                // Shrinking is still a request, though it asks for nothing.
                grew(-((old - new_size) as i64), 0);
            }
        }
        moved
    }
}

/// Run `f`, and report what it allocated.
///
/// All zeros if [`Allocator`] is not the global allocator, which
/// [`Metrics::allocator_installed`](crate::Metrics::allocator_installed) says.
///
/// # Nesting
///
/// A `measure` inside another restarts the counting, so the outer one
/// reports only what came after the inner one ended.
pub(crate) fn measure<R>(f: impl FnOnce() -> R) -> (R, Allocations) {
    let start = COUNTERS.with(|c| {
        c.peak.set(c.live.get());
        c.allocations.set(0);
        c.allocated.set(0);
        c.live.get()
    });
    let result = f();
    // Read while `result` is still alive: what the closure hands back is
    // part of what it kept.
    let stats = COUNTERS.with(|c| Allocations {
        allocation_count: c.allocations.get(),
        peak_allocated_bytes: (c.peak.get() - start).max(0) as u64,
        total_allocated_bytes: c.allocated.get(),
        net_allocated_bytes: c.live.get() - start,
    });
    (result, stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(retained: i64) -> Allocations {
        Allocations {
            net_allocated_bytes: retained,
            ..Allocations::default()
        }
    }

    #[test]
    fn counts_are_there_only_while_provided() {
        assert_eq!(current(), None);
        {
            let _outer = provide(Some(stats(5)));
            assert_eq!(current(), Some(stats(5)));
            {
                let _inner = provide(None);
                assert_eq!(current(), None);
            }
            assert_eq!(current(), Some(stats(5)));
        }
        assert_eq!(current(), None);
    }

    #[test]
    fn a_panic_does_not_leave_counts_behind() {
        let caught = std::panic::catch_unwind(|| {
            let _provided = provide(Some(stats(9)));
            panic!("a metrics function failing");
        });
        assert!(caught.is_err());
        assert_eq!(current(), None);
    }
}
