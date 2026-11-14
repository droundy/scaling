//! The xorshift64 generator that drives the crate's shuffles.
//!
//! Every user draws from this one type, so that the sequences stay exactly
//! as they always were: any change here changes which order benchmarks run
//! in, and which random draws the tests see.

/// A xorshift64 generator with shifts 13, 7 and 17.
///
/// Zero is a fixed point, so the state must be seeded nonzero (callers
/// typically use `seed | 1`).
pub(crate) struct XorShift(pub(crate) u64);

impl XorShift {
    /// Advance the state, and return the new state.
    pub(crate) fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Put `items` into a fresh uniformly random order, Fisher-Yates.
    pub(crate) fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }
}
