//! Counting allocations, so that a benchmark can report how much memory it
//! used as well as how long it took.
//!
//! A library cannot install a global allocator for the program using it, so
//! this takes one line in the benchmark binary:
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOC: scaling::alloc::CountingAlloc = scaling::alloc::CountingAlloc::new();
//! ```
//!
//! After that, a metrics function marked `allocation` is given the numbers
//! for its candidate's run, and asks for the ones it wants to see with
//! [`Metrics::peak_bytes`](crate::Metrics::peak_bytes) and the others beside
//! it. Nothing here is needed to use those; [`measure`] is for counting
//! some other piece of code.
//!
//! # What is counted
//!
//! Every allocation that goes through Rust's global allocator, made by the
//! thread that is measuring. Memory the program gets some other way (a
//! memory map, a C library's own `malloc`) is not seen, and neither are other
//! threads' allocations: the counters are per thread, so a benchmark that
//! hands its work to a thread pool is measured only for what it does itself.
//!
//! The counters are always running once the allocator is installed, which
//! costs a few instructions on every allocation - including in a timing loop.
//! A benchmark that allocates heavily is therefore a little slower than it
//! would be without the allocator, and the baseline slows with the rest.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

/// What a stretch of code allocated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AllocStats {
    /// The most memory it held at once, beyond what was already held when it
    /// started.
    pub peak_bytes: u64,
    /// How many times it asked for memory: allocating, and growing or
    /// shrinking an allocation, each count once.
    pub allocations: u64,
    /// How much memory it asked for in all, counting what it asked for again
    /// each time. Growing an allocation counts only the growth.
    pub allocated_bytes: u64,
    /// How much more memory it held when it ended than when it began: what
    /// it returned, and anything else it kept. Negative when it freed memory
    /// it was holding to begin with. Counted before its result is dropped.
    pub retained_bytes: i64,
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

/// Set by the first allocation through [`CountingAlloc`], which is how the
/// rest of the crate can tell that it is the global allocator.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Whether [`CountingAlloc`] is the global allocator.
///
/// Only meaningful once the program has allocated, which anything that has
/// reached `main` has.
pub fn installed() -> bool {
    INSTALLED.load(Ordering::Relaxed)
}

/// A global allocator that counts, and otherwise leaves everything to the
/// system's.
pub struct CountingAlloc;

impl CountingAlloc {
    /// For `static ALLOC: CountingAlloc = CountingAlloc::new();`.
    pub const fn new() -> Self {
        CountingAlloc
    }
}

impl Default for CountingAlloc {
    fn default() -> Self {
        CountingAlloc::new()
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
unsafe impl GlobalAlloc for CountingAlloc {
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
/// All zeros if [`CountingAlloc`] is not the global allocator, which
/// [`installed`] says.
///
/// # Nesting
///
/// A `measure` inside another restarts the counting, so the outer one
/// reports only what came after the inner one ended.
pub fn measure<R>(f: impl FnOnce() -> R) -> (R, AllocStats) {
    let start = COUNTERS.with(|c| {
        c.peak.set(c.live.get());
        c.allocations.set(0);
        c.allocated.set(0);
        c.live.get()
    });
    let result = f();
    // Read while `result` is still alive: what the closure hands back is
    // part of what it kept.
    let stats = COUNTERS.with(|c| AllocStats {
        peak_bytes: (c.peak.get() - start).max(0) as u64,
        allocations: c.allocations.get(),
        allocated_bytes: c.allocated.get(),
        retained_bytes: c.live.get() - start,
    });
    (result, stats)
}
