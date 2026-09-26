//! Benchmarks registered from anywhere in a crate, collected without a
//! central list.
//!
//! A benchmark in this crate is a closure, and a closure cannot be a
//! `static`. [`inventory`] can only collect `'static` values with no captured
//! environment, so what gets registered is not the benchmark but a plain
//! `fn` item that knows how to add it - see `Registered::add` for why
//! "add" rather than "run".
//!
//! Nothing here is written by hand. The attribute macros emit it, and it is
//! documented so that what they emit can be read and checked rather than
//! taken on faith.

use std::any::{Any, TypeId};
use std::fmt;

pub use crate::kway::InputGroup;
pub use crate::suite::Suite;

/// A comparison alternative shim that does nothing, for tests that check
/// what assembly *decides* rather than what it *measures* - a plan or a lane
/// can be built and inspected without a real alternative behind it.
#[cfg(test)]
pub(crate) fn noop_alt(set: InputGroup<ErasedInput>, _: &str) -> InputGroup<ErasedInput> {
    set
}

/// One registered benchmark.
///
/// Submitted by [`inventory::submit!`] from wherever the benchmark is
/// written, and collected by [`inventory::iter`] in whatever binary links
/// it. Nothing orders these: two registrations from different compilation
/// units arrive in an order the linker chose, so anything that depends on
/// order - which alternative is a comparison's baseline, what order a report
/// prints in - has to be decided from the fields rather than from position.
#[derive(Debug)]
pub struct Registered {
    /// What the report calls this, conventionally module-qualified so that
    /// two benchmarks of the same name in different modules do not collide.
    pub name: &'static str,
    /// The crate this was registered from, as its own `Cargo.toml` spells
    /// it - `env!("CARGO_PKG_NAME")` evaluates where the registration is
    /// written, not where it is collected.
    pub crate_name: &'static str,
    /// That crate's version, from `env!("CARGO_PKG_VERSION")`.
    ///
    /// Together with `crate_name` this says where a registration came from,
    /// which is what distinguishes the same benchmark registered by two
    /// versions of one crate - an old one pulled in as a dev-dependency to
    /// compare against.
    pub crate_version: &'static str,
    /// Adds itself with [`Suite::add`], [`Suite::add_input`],
    /// [`Suite::add_make_input`], [`Suite::add_scaling`] or
    /// [`Suite::add_scaling_gen`] - which one, and any input generator or
    /// `nmin`, is baked into the shim, since this signature has nowhere to
    /// pass them.
    ///
    /// A bare `fn` pointer, which is what makes this registrable: a `fn`
    /// item captures nothing and is `'static`, and `'static` outlives any
    /// `'a`, so it satisfies a suite's bounds whatever borrow the caller ends
    /// up with.
    ///
    /// Always standalone - a function naming one or more `group`s registers a
    /// [`Candidate`] instead, never a `Registered`, so nothing here needs to
    /// carry group membership or baseline status. See [`Candidate`] for that
    /// side.
    ///
    /// # Why this adds rather than runs
    ///
    /// The obvious shape - `fn(&Config) -> Stats`, run it and hand back the
    /// answer - would be wrong. [`Config::bench`] and friends drive their
    /// sampling loop to completion with `block_on`, so a registry of those
    /// would run every benchmark start to finish, one after another. That is
    /// precisely what a suite exists not to do: its scheduler interleaves
    /// samples so that no benchmark is measured in a machine state its
    /// neighbours never saw.
    ///
    /// So a shim is handed the suite and adds itself to it, and the sampling
    /// happens later, interleaved with everyone else's.
    ///
    /// # Why no generics survive
    ///
    /// A benchmark is generic in its closure, its input and its return type;
    /// none of that can appear here, because a registry holds one type. It
    /// does not need to: the macro that writes a shim knows the concrete
    /// types at the point it writes it, so `F`, `I` and `O` are resolved
    /// there and the shim that comes out has a fixed signature.
    ///
    /// [`Config::bench`]: crate::Config::bench
    pub add: fn(&mut Suite, &str),
}

inventory::collect!(Registered);

/// The shared input of a comparison group, with its type erased.
///
/// # Why erased
///
/// A comparison set is generic over one input type shared by every
/// alternative. A registry cannot name that type - it holds registrations
/// from all over a crate, and they do not agree on one - so the input has to
/// become a single concrete type before it can be stored, and the real type
/// recovered when an alternative is handed its input.
///
/// # Why it is `Clone`, and why that matters
///
/// Assembly generates **one** input per round and clones it for each
/// alternative, so that within a round they are all measured on the same
/// input. That sharing is not a convenience: it is what makes the per-round
/// differences genuinely paired, and paired differences are the whole reason
/// a comparison's error bar is narrower than combining two separate
/// measurements. If each alternative drew its own input, and cost varied
/// with the input, every difference would carry the difference between two
/// draws as well - and it would still print a number, just a worse one, with
/// nothing to say it had happened.
///
/// So the erased input must be `Clone`, and `Box<dyn Any>` is not.
/// [`ErasedInput::new`] captures a clone function alongside the value, which
/// works because it is generic: it knows `I` even though nothing that stores
/// the result does.
pub struct ErasedInput {
    value: Box<dyn Any>,
    /// `I::clone`, monomorphised where `I` was still known.
    clone_fn: fn(&dyn Any) -> Box<dyn Any>,
    /// What `I` was, so that assembly can check a group agrees about it
    /// before any downcast is attempted.
    type_id: TypeId,
}

impl ErasedInput {
    /// Erase `v`, remembering how to clone it and what it was.
    pub fn new<I: Any + Clone>(v: I) -> Self {
        ErasedInput {
            value: Box::new(v),
            // Written here rather than stored as a trait object because
            // here is where `I` is known. A `dyn CloneAny` would need a
            // trait, a blanket impl and a vtable to say the same thing.
            clone_fn: |a| {
                Box::new(
                    a.downcast_ref::<I>()
                        .expect("clone_fn sees its own I")
                        .clone(),
                )
            },
            type_id: TypeId::of::<I>(),
        }
    }

    /// What was erased.
    pub fn type_id(&self) -> TypeId {
        self.type_id
    }

    /// The input, as the type it really is.
    ///
    /// # Panics
    ///
    /// If `I` is not the erased type. Assembly checks every alternative in a
    /// group against the group's generator before anything runs, and reports
    /// a mismatch as a named error rather than a panic - so reaching this is
    /// a bug in assembly, not a mistake a caller can make.
    pub fn get_mut<I: Any>(&mut self) -> &mut I {
        self.value
            .downcast_mut::<I>()
            .expect("input type was checked during assembly")
    }
}

impl Clone for ErasedInput {
    fn clone(&self) -> Self {
        ErasedInput {
            value: (self.clone_fn)(&*self.value),
            clone_fn: self.clone_fn,
            type_id: self.type_id,
        }
    }
}

/// One implementation to be measured against whatever candidates and inputs
/// share one of its groups and its input type.
///
/// # Why candidates and inputs are registered separately
///
/// The point of a group is to write each implementation once and each input
/// once, and have every pairing measured. Writing an entry per pairing would
/// put the cross-product back in the source - and worse, back in one place,
/// which is the central list this whole design exists to remove. Adding an
/// input would then mean editing every implementation, or a list somewhere
/// else.
///
/// So neither side names the other, beyond the group name(s) both declare.
/// A candidate says what type of input it wants, an input says what type it
/// is, and assembly pairs them up within each shared group.
///
/// "Candidate" rather than "row" because it is already this crate's word for
/// one side of a measured difference - [`crate::Timing`] holds a baseline
/// and a candidate.
pub struct Candidate {
    /// Every group this belongs to. A candidate with no groups is not
    /// constructed in practice - a function with nothing to compare against
    /// is written as a plain `#[bench]`, which registers a [`Registered`]
    /// instead - but an empty slice is not itself invalid here, just inert.
    pub groups: &'static [&'static str],
    /// What to call this candidate in the report.
    pub name: &'static str,
    /// The type of input it takes, which is what it is paired on. A function
    /// rather than the id itself, because a registration is a `static` and
    /// so must be built in a `const` context.
    pub input_type: fn() -> TypeId,
    /// That type as the source spells it, for diagnostics.
    pub input_type_name: &'static str,
    /// Whether this is the one the others are reported against, in every
    /// group it belongs to. If nobody in a lane says so, assembly picks the
    /// first by name; adding a candidate sorting earlier then moves the
    /// baseline, which is why the report names it.
    pub is_baseline: bool,
    pub crate_name: &'static str,
    pub crate_version: &'static str,
    /// One alternative of the input group this candidate belongs to,
    /// including when it is the only candidate in the group.
    pub add_alt: fn(InputGroup<ErasedInput>, &str) -> InputGroup<ErasedInput>,
}

impl fmt::Debug for Candidate {
    /// Hand-written because the shims are noise as addresses, and what is
    /// worth seeing is what this candidate is and where it came from.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Candidate")
            .field("groups", &self.groups)
            .field("name", &self.name)
            .field("input_type_name", &self.input_type_name)
            .field("is_baseline", &self.is_baseline)
            .field("crate_name", &self.crate_name)
            .field("crate_version", &self.crate_version)
            .finish()
    }
}

inventory::collect!(Candidate);

/// One input every candidate of its type, in a group this shares with it, is
/// measured on.
#[derive(Debug)]
pub struct Input {
    /// Every group this feeds. One input function can feed several groups
    /// at once - a shared sorted `Vec` used by `"sort"`, `"dedup"` and
    /// `"contains"`, say - without those groups' candidates being compared
    /// with each other: pairing only ever happens within one shared group
    /// name at a time.
    pub groups: &'static [&'static str],
    /// What to call this input in the report.
    pub name: &'static str,
    /// Which crate registered it, and at what version.
    ///
    /// Inputs carry this for the opposite reason candidates do: not to tell
    /// several versions apart, but to pick one of them. See
    /// [`crate::assemble::Lane::inputs`].
    pub crate_name: &'static str,
    pub crate_version: &'static str,
    /// The type it produces, which is what candidates are paired to it on.
    pub type_id: fn() -> TypeId,
    /// That type as the source spells it, for diagnostics.
    pub type_name: &'static str,
    /// Called once per round. With multiple candidates the value is cloned
    /// for each, so that all of them meet the same input; a singleton uses it
    /// directly. See [`ErasedInput`].
    pub make: fn() -> ErasedInput,
}

inventory::collect!(Input);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    /// The point of `ErasedInput`: a clone that goes through the erased type
    /// is still a real, deep clone of the original.
    ///
    /// A shallow copy sharing a buffer would not be an independent input,
    /// and an alternative handed one is not being measured on its own input.
    #[test]
    fn erased_inputs_clone_deeply() {
        let mut a = ErasedInput::new(vec![1i32, 2, 3]);
        let mut b = a.clone();
        b.get_mut::<Vec<i32>>().push(4);
        assert_eq!(a.get_mut::<Vec<i32>>().as_slice(), &[1, 2, 3]);
        assert_eq!(b.get_mut::<Vec<i32>>().as_slice(), &[1, 2, 3, 4]);
    }

    /// Cloning must not quietly forget what the input was, or a later
    /// downcast would fail on a value that is perfectly fine.
    #[test]
    fn cloning_preserves_the_type() {
        let a = ErasedInput::new(String::from("x"));
        assert_eq!(a.clone().type_id(), a.type_id());
        assert_eq!(a.type_id(), TypeId::of::<String>());
    }

    /// The unit input, which is what a group with no declared generator
    /// uses. Worth its own case because it is the one every `comparison()`
    /// group takes, and because a zero-sized value is exactly where a
    /// hand-rolled erasure would be tempted to cut a corner.
    #[test]
    fn the_unit_input_erases_like_any_other() {
        let mut a = ErasedInput::new(());
        assert_eq!(a.type_id(), TypeId::of::<()>());
        let mut b = a.clone();
        *a.get_mut::<()>() = ();
        *b.get_mut::<()>() = ();
    }

    /// `inventory` actually collects, in this crate, on this platform.
    ///
    /// This is the assumption the whole approach rests on, and it is not a
    /// language guarantee: collection works by having each `submit!` place a
    /// value in a linker section and a constructor run before `main`. Link-
    /// time garbage collection has historically dropped exactly this kind of
    /// item, since nothing references it by name. If that ever happens here,
    /// every other test still passes and benchmarks simply go missing - so
    /// check it directly.
    #[test]
    fn submissions_are_collected() {
        let found: Vec<&str> = inventory::iter::<Registered>()
            .map(|r| r.name)
            .filter(|n| n.starts_with("registry-selftest::"))
            .collect();
        assert!(
            found.contains(&"registry-selftest::alpha"),
            "submitted registrations did not come back: {found:?}",
        );
        assert!(
            found.contains(&"registry-selftest::beta"),
            "submitted registrations did not come back: {found:?}",
        );
    }

    /// A shim adds its benchmark to a suite rather than running it, and the
    /// suite measures it like anything else.
    ///
    /// This is the shape claim from `Registered::add` made concrete: the
    /// registration is a `static`, the shim is a bare `fn`, and yet what
    /// comes out the far end is a normal interleaved suite entry with a real
    /// measurement in it.
    #[test]
    fn a_registered_shim_adds_itself_and_is_measured() {
        use std::time::Duration;
        let cfg = Config::default().with_max_time(Duration::from_millis(20));
        let mut suite = cfg.suite();
        let mut added = 0;
        for r in inventory::iter::<Registered>() {
            if !r.name.starts_with("registry-selftest::") {
                continue;
            }
            (r.add)(&mut suite, r.name);
            added += 1;
        }
        assert_eq!(added, 2, "expected both self-test benchmarks");
        let shown = format!("{}", suite.run());
        for name in ["registry-selftest::alpha", "registry-selftest::beta"] {
            assert!(shown.contains(name), "{name} missing from report:\n{shown}");
        }
        assert!(
            !shown.contains("(not measured)"),
            "a registered benchmark was added but never ran:\n{shown}",
        );
    }

    // Two registrations, written the way generated code will write them.
    // Deliberately at item position in a test module: that is where
    // `submit!` has to work, and it is the arrangement a macro produces.
    fn add_alpha(adder: &mut Suite, name: &str) {
        adder.add(name, || (0..32u64).sum::<u64>());
    }

    fn add_beta(adder: &mut Suite, name: &str) {
        adder.add_input(name, vec![3i32, 1, 2], |v: &mut Vec<i32>| v.sort());
    }

    inventory::submit! {
        Registered {
            name: "registry-selftest::alpha",
            crate_name: env!("CARGO_PKG_NAME"),
            crate_version: env!("CARGO_PKG_VERSION"),
            add: add_alpha,
        }
    }

    inventory::submit! {
        Registered {
            name: "registry-selftest::beta",
            crate_name: env!("CARGO_PKG_NAME"),
            crate_version: env!("CARGO_PKG_VERSION"),
            add: add_beta,
        }
    }

    /// Two different erased types must be distinguishable, since that is
    /// what assembly checks a group with.
    #[test]
    fn different_types_are_distinguishable() {
        let a = ErasedInput::new(vec![0u8]);
        let b = ErasedInput::new(String::new());
        assert_ne!(a.type_id(), b.type_id());
    }
}
