//! Benchmarks registered in a crate of their own, to check that a program
//! that only depends on this crate still finds them.
//!
//! That is not a given on wasm. `wasm-ld` takes an object file out of a
//! library only to resolve a symbol something else uses, and a registration
//! is a static constructor that nothing uses, so a crate that is linked but
//! not referenced loses its benchmarks without a word. See "Running under
//! WebAssembly" in the README, and `../runner`, which fails if they go
//! missing.

/// What a program has to refer to, so that the linker keeps this crate's
/// object code and the registrations in it.
///
/// A static, because a small function can be copied into the program that
/// calls it, which leaves nothing in this crate referenced. And one per
/// crate is enough only because this crate is built as a single codegen unit
/// (see the workspace `Cargo.toml`): a crate built as several is several
/// object files, and the registrations in the others are not pulled in.
pub static SCALING_ANCHOR: u8 = 0;

#[scaling::bench]
fn at_the_root() -> u64 {
    (0..100u64).fold(0, |a, x| a.wrapping_mul(31).wrapping_add(x))
}

mod first {
    #[scaling::bench]
    fn in_a_module() -> u64 {
        (0..110u64).fold(0, |a, x| a.wrapping_mul(33).wrapping_add(x))
    }
}

mod second {
    #[scaling::bench]
    fn in_another_module() -> u64 {
        (0..120u64).fold(0, |a, x| a.wrapping_mul(35).wrapping_add(x))
    }
}
