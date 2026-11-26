//! Benchmarks registered in a crate of their own, to check that a program
//! that only depends on this crate still finds them, which on wasm is not a
//! given: see "Running under WebAssembly" in the README, and `../runner`,
//! which fails if they go missing.

/// What a program refers to, so that the wasm linker keeps this crate's
/// registrations. It keeps them all because the workspace `Cargo.toml` builds
/// this crate as one codegen unit.
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
