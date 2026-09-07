//! Benchmarks registered from anywhere in a crate, collected without a
//! central list.
//!
//! A benchmark in this crate is a closure, and a closure cannot be a
//! `static`. [`inventory`] can only collect `'static` values with no captured
//! environment, so what gets registered is not the benchmark but a plain
//! `fn` item that knows how to add it - see [`Kind`](crate::registry::Kind)
//! for why "add" rather than "run".
//!
//! Everything here is behind the `registry` feature and is additive: the
//! types below are new, nothing else changes, and a crate that does not
//! enable the feature is exactly as it was.
//!
//! See `REGISTRATION.md` for the design this implements and the stages still
//! to come.

use crate::{ComparisonSet, Config, Suite};
use std::any::{Any, TypeId};

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
    /// The comparison group this belongs to, if any. Members of a group are
    /// timed against each other rather than reported separately.
    pub group: Option<&'static str>,
    /// Whether this is its group's baseline, which every other member is
    /// reported against.
    ///
    /// Order cannot say this, as it does for a hand-built
    /// [`ComparisonSet`] where the first alternative added is the baseline,
    /// because registrations have no order. It has to be declared.
    pub is_baseline: bool,
    /// How to add this benchmark to a suite.
    pub kind: Kind,
}

/// How a registered benchmark joins a suite.
///
/// Each variant is a bare `fn` pointer, which is what makes these
/// registrable: a `fn` item captures nothing and is `'static`, and `'static`
/// outlives any `'a`, so one of these satisfies a [`Suite<'a>`]'s bounds
/// whatever borrow the caller ends up with.
///
/// # Why these add rather than run
///
/// The obvious shape - `fn(&Config) -> Stats`, run it and hand back the
/// answer - would be wrong. [`Config::bench`] and friends drive their
/// sampling loop to completion with `block_on`, so a registry of those would
/// run every benchmark start to finish, one after another. That is precisely
/// what [`Suite`] exists not to do: its scheduler interleaves samples so that
/// no benchmark is measured in a machine state its neighbours never saw.
///
/// So a shim is handed the suite and adds itself to it, and the sampling
/// happens later, interleaved with everyone else's.
///
/// # Why no generics survive
///
/// A benchmark is generic in its closure, its input and its return type;
/// none of that can appear here, because a registry holds one type. It does
/// not need to: the macro that writes a shim knows the concrete types at the
/// point it writes it, so `F`, `I` and `O` are resolved there and the shim
/// that comes out has a fixed signature. What crosses the boundary is a
/// function pointer, and the three result types a benchmark can produce -
/// [`Stats`], [`ScalingStats`], [`Comparisons`] - are concrete already.
///
/// [`Stats`]: crate::Stats
/// [`ScalingStats`]: crate::ScalingStats
/// [`Comparisons`]: crate::Comparisons
/// [`Config::bench`]: crate::Config::bench
#[derive(Debug)]
pub enum Kind {
    /// Adds itself with [`Suite::add`], [`Suite::add_input`] or
    /// [`Suite::add_gen_input`] - which of the three, and any input
    /// generator, is baked into the shim.
    Flat(fn(&mut Suite<'_>, &Config, &str)),
    /// Adds itself with [`Suite::add_scaling`] or
    /// [`Suite::add_scaling_gen`]. `nmin` is baked in too, since this
    /// signature has nowhere to pass it.
    Scaling(fn(&mut Suite<'_>, &Config, &str)),
    /// Adds itself to a comparison group's [`ComparisonSet`].
    Alt {
        /// Takes the set and gives it back because `ComparisonSet` is a
        /// consuming builder. The `for<'a>` is what lets one registration
        /// serve whatever `Config` borrow assembly ends up with, rather than
        /// being tied to a lifetime chosen at registration time - which,
        /// being a `static`, would have to be `'static`.
        add: for<'a> fn(ComparisonSet<'a, ErasedInput>, &str) -> ComparisonSet<'a, ErasedInput>,
        /// The input type this alternative expects, before erasure.
        ///
        /// Carried so that assembly can check every member of a group agrees
        /// with the group's generator *before* anything runs. Without it the
        /// first mismatched downcast would panic from inside the scheduler,
        /// naming nothing useful.
        input_type: TypeId,
        /// The same type, spelled the way the source spells it, because a
        /// `TypeId` says nothing to a reader and a diagnostic has to.
        ///
        /// Written by the registering macro with `stringify!`, which is a
        /// literal and so usable in the `static` a registration becomes.
        input_type_name: &'static str,
    },
}

impl Kind {
    /// How this spells its input type, for a diagnostic to quote. `"()"` for
    /// the kinds that take no input.
    pub fn input_type_name(&self) -> &'static str {
        match self {
            Kind::Alt {
                input_type_name, ..
            } => input_type_name,
            _ => "()",
        }
    }
}

#[cfg(feature = "registry")]
inventory::collect!(Registered);

/// The shared input of a comparison group, with its type erased.
///
/// # Why erased
///
/// [`ComparisonSet<I>`] is generic over one input type shared by every
/// alternative. A registry cannot name that type - it holds registrations
/// from all over a crate, and they do not agree on one - so the input has to
/// become a single concrete type before it can be stored, and the real type
/// recovered when an alternative is handed its input.
///
/// # Why it is `Clone`, and why that matters
///
/// [`Config::comparison_gen_input`] generates **one** input per round and
/// clones it for each alternative, so that within a round they are all
/// measured on the same input. That sharing is not a convenience: it is what
/// makes the per-round differences genuinely paired, and paired differences
/// are the whole reason a comparison's error bar is narrower than combining
/// two separate measurements. If each alternative drew its own input, and
/// cost varied with the input, every difference would carry the difference
/// between two draws as well - and it would still print a number, just a
/// worse one, with nothing to say it had happened.
///
/// So the erased input must be `Clone`, and `Box<dyn Any>` is not.
/// [`ErasedInput::new`] captures a clone function alongside the value, which
/// works because it is generic: it knows `I` even though nothing that stores
/// the result does.
///
/// [`ComparisonSet<I>`]: crate::ComparisonSet
/// [`Config::comparison_gen_input`]: crate::Config::comparison_gen_input
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

/// The input generator shared by one comparison group.
///
/// Registered separately from the group's alternatives, and exactly once per
/// group, because there is exactly one generator per group - see
/// [`ErasedInput`] for why the alternatives cannot each bring their own.
#[derive(Debug)]
pub struct GenInputRegistration {
    /// The group this generates input for.
    pub group: &'static str,
    /// `TypeId::of::<I>()`, so assembly can check the group's alternatives
    /// agree with it.
    pub type_id: TypeId,
    /// The same type as the source spells it, for diagnostics. See
    /// [`Kind::Alt::input_type_name`](Kind#variant.Alt.field.input_type_name).
    pub type_name: &'static str,
    /// Called once per round.
    pub make: fn() -> ErasedInput,
}

#[cfg(feature = "registry")]
inventory::collect!(GenInputRegistration);

#[cfg(test)]
mod tests {
    use super::*;

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
    #[cfg(feature = "registry")]
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
    /// This is the shape claim from [`Kind`] made concrete: the registration
    /// is a `static`, the shim is a bare `fn`, and yet what comes out the far
    /// end is a normal interleaved suite entry with a real measurement in it.
    #[cfg(feature = "registry")]
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
            match r.kind {
                Kind::Flat(add) => {
                    add(&mut suite, &cfg, r.name);
                    added += 1;
                }
                _ => panic!("the self-test registrations are all flat"),
            }
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
    #[cfg(feature = "registry")]
    fn add_alpha(suite: &mut Suite<'_>, _cfg: &Config, name: &str) {
        suite.add(name, || (0..32u64).sum::<u64>());
    }

    #[cfg(feature = "registry")]
    fn add_beta(suite: &mut Suite<'_>, _cfg: &Config, name: &str) {
        suite.add_input(name, vec![3i32, 1, 2], |v: &mut Vec<i32>| v.sort());
    }

    #[cfg(feature = "registry")]
    inventory::submit! {
        Registered {
            name: "registry-selftest::alpha",
            crate_name: env!("CARGO_PKG_NAME"),
            crate_version: env!("CARGO_PKG_VERSION"),
            group: None,
            is_baseline: false,
            kind: Kind::Flat(add_alpha),
        }
    }

    #[cfg(feature = "registry")]
    inventory::submit! {
        Registered {
            name: "registry-selftest::beta",
            crate_name: env!("CARGO_PKG_NAME"),
            crate_version: env!("CARGO_PKG_VERSION"),
            group: None,
            is_baseline: false,
            kind: Kind::Flat(add_beta),
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
