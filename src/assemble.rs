//! Turning a pile of unordered registrations into a plan for a suite.
//!
//! Everything that can go wrong with a set of registrations is decided here,
//! before any benchmark runs. That matters for a reason particular to this
//! crate: running means claiming the machine, which pins threads and blocks
//! until whatever else is benchmarking gives the reserved CPUs back. A
//! mistake reported after that wait has cost real time and says nothing that
//! could not have been said immediately - the same reasoning
//! [`crate::ComparisonSet::run`] already applies when it checks its
//! alternative count before claiming.
//!
//! [`plan`](crate::assemble::plan) is therefore a pure function over slices:
//! it takes registrations
//! and returns either a plan or a list of complaints, touching nothing and
//! measuring nothing. That makes every diagnostic below testable without a
//! benchmark, a machine claim, or a linker.
//!
//! Stage 2 of `REGISTRATION.md`.

use crate::registry::{ErasedInput, GenInputRegistration, Kind, Registered};
use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};

/// Something wrong with a set of registrations.
///
/// Named rather than numbered, and carrying the sources it is complaining
/// about, because the whole point of collecting registrations from anywhere
/// is that the reader does not know where they all are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    /// Two registrations claim the same name, so a report could not tell
    /// their rows apart.
    DuplicateName {
        name: String,
        /// Where each came from, as `crate@version (module)`.
        sources: Vec<String>,
    },
    /// A comparison group with fewer than two alternatives. There is nothing
    /// to compare a lone alternative against, and
    /// [`crate::Suite::add_comparison`] would panic.
    LonelyGroup { group: String, members: Vec<String> },
    /// A comparison group where nobody is the baseline.
    ///
    /// A hand-built [`crate::ComparisonSet`] takes its first alternative as
    /// the baseline, but registrations have no order, so one of them has to
    /// say.
    NoBaseline { group: String, members: Vec<String> },
    /// A comparison group where more than one alternative claims to be the
    /// baseline.
    ManyBaselines {
        group: String,
        claimants: Vec<String>,
    },
    /// More than one input generator declared for one group. They cannot
    /// both be the group's shared input.
    ManyGenerators { group: String, sources: Vec<String> },
    /// An alternative expects a different input type from the one its
    /// group's generator produces.
    ///
    /// Caught here so that it is a named error rather than a downcast panic
    /// from somewhere inside the scheduler.
    InputTypeMismatch {
        group: String,
        member: String,
        /// What the generator makes, and what this member wanted. Type
        /// *names* rather than `TypeId`s, since a `TypeId` says nothing to a
        /// reader.
        generator_type: &'static str,
        member_type: &'static str,
    },
}

impl Display for Diagnostic {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Diagnostic::DuplicateName { name, sources } => {
                write!(
                    f,
                    "two benchmarks are both called `{name}`: {}",
                    list(sources)
                )
            }
            Diagnostic::LonelyGroup { group, members } => write!(
                f,
                "comparison group `{group}` has only {}; a comparison needs at least two",
                list(members),
            ),
            Diagnostic::NoBaseline { group, members } => write!(
                f,
                "comparison group `{group}` has no baseline; mark one of {} as the baseline",
                list(members),
            ),
            Diagnostic::ManyBaselines { group, claimants } => write!(
                f,
                "comparison group `{group}` has more than one baseline: {}",
                list(claimants),
            ),
            Diagnostic::ManyGenerators { group, sources } => write!(
                f,
                "comparison group `{group}` has more than one input generator: {}",
                list(sources),
            ),
            Diagnostic::InputTypeMismatch {
                group,
                member,
                generator_type,
                member_type,
            } => write!(
                f,
                "in comparison group `{group}`, `{member}` takes `{member_type}` \
                 but the group's input generator produces `{generator_type}`",
            ),
        }
    }
}

fn list(items: &[String]) -> String {
    items.join(", ")
}

/// What to run, in what order, once the registrations check out.
///
/// Holds borrows of the registrations themselves rather than copies of them:
/// they are `'static` already, being collected from linker sections.
#[derive(Debug)]
pub struct Plan {
    /// Benchmarks that stand alone, sorted by name.
    pub flat: Vec<&'static Registered>,
    /// Comparison groups, sorted by group name, each with its baseline
    /// first.
    pub groups: Vec<Group>,
}

/// One comparison group, ready to become a [`crate::ComparisonSet`].
#[derive(Debug)]
pub struct Group {
    pub name: &'static str,
    /// The baseline first, then the rest sorted by name. `ComparisonSet`
    /// takes its first alternative as the baseline, so this ordering is what
    /// carries the decision made here into the set built later.
    pub members: Vec<&'static Registered>,
    /// The group's shared input generator, if one was declared.
    ///
    /// `None` means the group takes no input, and assembly will use
    /// `ErasedInput::new(())` - which is what makes a no-input group and a
    /// generated-input group one code path rather than two.
    pub gen_input: Option<&'static GenInputRegistration>,
}

impl Group {
    /// A maker for this group's input, defaulting to the unit input.
    pub fn make_input(&self) -> fn() -> ErasedInput {
        match self.gen_input {
            Some(g) => g.make,
            None => || ErasedInput::new(()),
        }
    }
}

/// Where a registration came from, for a diagnostic to name.
fn source(r: &Registered) -> String {
    format!("{}@{} ({})", r.crate_name, r.crate_version, r.name)
}

/// Check a set of registrations and decide what to run.
///
/// Returns every complaint it can find rather than the first, so that a
/// caller fixing them does not have to rebuild once per mistake.
///
/// # Ordering
///
/// Registrations arrive in whatever order the linker chose, which is not
/// stable and not meaningful. Everything here is therefore sorted by name -
/// the only ordering that is reproducible across builds and machines, and so
/// the only one whose report can be diffed against yesterday's. This does
/// not affect measurement: the scheduler reshuffles every round regardless.
pub fn plan(
    regs: &[&'static Registered],
    gens: &[&'static GenInputRegistration],
) -> Result<Plan, Vec<Diagnostic>> {
    let mut problems = Vec::new();

    // Duplicate names, across everything. Checked first because a duplicate
    // makes every later message ambiguous about which one it means.
    let mut by_name: BTreeMap<&str, Vec<&Registered>> = BTreeMap::new();
    for r in regs {
        by_name.entry(r.name).or_default().push(r);
    }
    for (name, rs) in &by_name {
        if rs.len() > 1 {
            problems.push(Diagnostic::DuplicateName {
                name: (*name).to_string(),
                sources: rs.iter().map(|r| source(r)).collect(),
            });
        }
    }

    // Generators, bucketed by group, so a group can be asked for its one.
    let mut gens_by_group: BTreeMap<&str, Vec<&'static GenInputRegistration>> = BTreeMap::new();
    for g in gens {
        gens_by_group.entry(g.group).or_default().push(g);
    }
    for (group, gs) in &gens_by_group {
        if gs.len() > 1 {
            problems.push(Diagnostic::ManyGenerators {
                group: (*group).to_string(),
                // A generator has no name of its own, so say how many and
                // for which group; the type is the only distinguishing mark.
                sources: gs.iter().map(|g| g.type_name.to_string()).collect(),
            });
        }
    }

    let mut flat: Vec<&'static Registered> = Vec::new();
    let mut grouped: BTreeMap<&'static str, Vec<&'static Registered>> = BTreeMap::new();
    for r in regs {
        match r.group {
            None => flat.push(r),
            Some(g) => grouped.entry(g).or_default().push(r),
        }
    }
    flat.sort_by_key(|r| r.name);

    let mut groups = Vec::new();
    for (name, mut members) in grouped {
        members.sort_by_key(|r| r.name);
        let names: Vec<String> = members.iter().map(|r| r.name.to_string()).collect();

        if members.len() < 2 {
            problems.push(Diagnostic::LonelyGroup {
                group: name.to_string(),
                members: names,
            });
            continue;
        }

        let claimants: Vec<&&'static Registered> =
            members.iter().filter(|r| r.is_baseline).collect();
        let baseline = match claimants.len() {
            1 => claimants[0].name,
            0 => {
                problems.push(Diagnostic::NoBaseline {
                    group: name.to_string(),
                    members: names,
                });
                continue;
            }
            _ => {
                problems.push(Diagnostic::ManyBaselines {
                    group: name.to_string(),
                    claimants: claimants.iter().map(|r| source(r)).collect(),
                });
                continue;
            }
        };

        // The generator, and whether everyone agrees about its type. A group
        // with no generator takes `()`, and its members must expect `()`.
        let gen_input = gens_by_group.get(name).and_then(|gs| gs.first()).copied();
        let (gen_type, gen_name) = match gen_input {
            Some(g) => (g.type_id, g.type_name),
            None => (std::any::TypeId::of::<()>(), "()"),
        };
        let mut mismatched = false;
        for m in &members {
            if let Kind::Alt { input_type, .. } = &m.kind {
                if *input_type != gen_type {
                    problems.push(Diagnostic::InputTypeMismatch {
                        group: name.to_string(),
                        member: m.name.to_string(),
                        generator_type: gen_name,
                        member_type: m.kind.input_type_name(),
                    });
                    mismatched = true;
                }
            }
        }
        if mismatched {
            continue;
        }

        // Baseline first: `ComparisonSet` takes its first alternative as the
        // baseline, so this is where the decision above becomes the order the
        // set is built in.
        members.sort_by_key(|r| (r.name != baseline, r.name));
        groups.push(Group {
            name,
            members,
            gen_input,
        });
    }

    if problems.is_empty() {
        Ok(Plan { flat, groups })
    } else {
        Err(problems)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ErasedInput;
    use crate::{ComparisonSet, Config, Suite};
    use std::any::TypeId;

    // Shims that do nothing. Assembly never calls them - it decides what
    // *would* be run - so a plan can be checked without a machine claim, a
    // benchmark, or a linker.
    fn noop_flat(_: &mut Suite<'_>, _: &Config, _: &str) {}
    fn noop_alt<'a>(
        set: ComparisonSet<'a, ErasedInput>,
        _: &str,
    ) -> ComparisonSet<'a, ErasedInput> {
        set
    }

    /// A standalone benchmark.
    fn flat(name: &'static str) -> Registered {
        Registered {
            name,
            crate_name: "testcrate",
            crate_version: "1.0.0",
            group: None,
            is_baseline: false,
            kind: Kind::Flat(noop_flat),
        }
    }

    /// One alternative of `group`, expecting input of type `I`.
    fn alt<I: 'static>(
        name: &'static str,
        group: &'static str,
        is_baseline: bool,
        type_name: &'static str,
    ) -> Registered {
        Registered {
            name,
            crate_name: "testcrate",
            crate_version: "1.0.0",
            group: Some(group),
            is_baseline,
            kind: Kind::Alt {
                add: noop_alt,
                input_type: TypeId::of::<I>(),
                input_type_name: type_name,
            },
        }
    }

    fn generator<I: 'static>(group: &'static str, type_name: &'static str) -> GenInputRegistration {
        GenInputRegistration {
            group,
            type_id: TypeId::of::<I>(),
            type_name,
            make: || ErasedInput::new(()),
        }
    }

    /// `plan` borrows `'static` registrations, which in a test have to be
    /// leaked - they really are `'static` in the real thing, coming from
    /// linker sections.
    fn leak(rs: Vec<Registered>) -> Vec<&'static Registered> {
        rs.into_iter().map(|r| &*Box::leak(Box::new(r))).collect()
    }

    fn leak_gens(gs: Vec<GenInputRegistration>) -> Vec<&'static GenInputRegistration> {
        gs.into_iter().map(|g| &*Box::leak(Box::new(g))).collect()
    }

    /// Registrations arrive in whatever order the linker chose, so a report
    /// built from them must impose its own - or two runs of the same
    /// benchmarks could not be diffed.
    #[test]
    fn flat_benchmarks_come_out_sorted() {
        let regs = leak(vec![flat("zebra"), flat("apple"), flat("middle")]);
        let plan = plan(&regs, &[]).expect("nothing wrong with these");
        let names: Vec<&str> = plan.flat.iter().map(|r| r.name).collect();
        assert_eq!(names, ["apple", "middle", "zebra"]);
        assert!(plan.groups.is_empty());
    }

    /// Reversing the input must not change the plan: that is what "sorted"
    /// is for, and a sort that happened to agree with the input order would
    /// pass a single-order test while proving nothing.
    #[test]
    fn the_order_registrations_arrive_in_does_not_matter() {
        let forward = leak(vec![flat("a"), flat("b"), flat("c")]);
        let backward = leak(vec![flat("c"), flat("b"), flat("a")]);
        let one = plan(&forward, &[]).unwrap();
        let two = plan(&backward, &[]).unwrap();
        let names =
            |p: &Plan| -> Vec<String> { p.flat.iter().map(|r| r.name.to_string()).collect() };
        assert_eq!(names(&one), names(&two));
    }

    /// The baseline goes first because `ComparisonSet` takes its first
    /// alternative as the baseline - that ordering is how the decision made
    /// here reaches the set built later.
    #[test]
    fn the_baseline_is_placed_first() {
        let regs = leak(vec![
            alt::<()>("a_first_alphabetically", "g", false, "()"),
            alt::<()>("z_the_baseline", "g", true, "()"),
            alt::<()>("m_middle", "g", false, "()"),
        ]);
        let plan = plan(&regs, &[]).expect("a well-formed group");
        assert_eq!(plan.groups.len(), 1);
        let names: Vec<&str> = plan.groups[0].members.iter().map(|r| r.name).collect();
        assert_eq!(
            names,
            ["z_the_baseline", "a_first_alphabetically", "m_middle"],
            "baseline first, then the rest sorted",
        );
    }

    /// Two benchmarks of one name would make a report ambiguous, and the
    /// message has to say where each came from - the reader does not know
    /// where all the registrations are, which is the point of registering.
    #[test]
    fn duplicate_names_are_rejected_and_name_their_sources() {
        let regs = leak(vec![flat("same"), flat("same"), flat("fine")]);
        let problems = plan(&regs, &[]).expect_err("a duplicate is an error");
        assert_eq!(problems.len(), 1);
        match &problems[0] {
            Diagnostic::DuplicateName { name, sources } => {
                assert_eq!(name, "same");
                assert_eq!(sources.len(), 2);
                assert!(sources.iter().all(|s| s.contains("testcrate@1.0.0")));
            }
            other => panic!("wrong diagnostic: {other:?}"),
        }
    }

    /// A lone alternative has nothing to compare against, and
    /// `Suite::add_comparison` would panic on it. Say so here instead, where
    /// the message can name the group.
    #[test]
    fn a_group_of_one_is_rejected() {
        let regs = leak(vec![alt::<()>("only", "lonely", true, "()")]);
        let problems = plan(&regs, &[]).expect_err("one alternative is not a comparison");
        assert!(
            matches!(&problems[0], Diagnostic::LonelyGroup { group, .. } if group == "lonely"),
            "{problems:?}",
        );
    }

    /// Registrations have no order, so nothing can be the baseline by
    /// arriving first. One of them has to say, and if none does that is an
    /// error rather than an arbitrary pick.
    #[test]
    fn a_group_with_no_baseline_is_rejected() {
        let regs = leak(vec![
            alt::<()>("a", "g", false, "()"),
            alt::<()>("b", "g", false, "()"),
        ]);
        let problems = plan(&regs, &[]).expect_err("somebody must be the baseline");
        assert!(
            matches!(&problems[0], Diagnostic::NoBaseline { group, .. } if group == "g"),
            "{problems:?}",
        );
    }

    #[test]
    fn a_group_with_two_baselines_is_rejected() {
        let regs = leak(vec![
            alt::<()>("a", "g", true, "()"),
            alt::<()>("b", "g", true, "()"),
        ]);
        let problems = plan(&regs, &[]).expect_err("only one may be the baseline");
        match &problems[0] {
            Diagnostic::ManyBaselines { group, claimants } => {
                assert_eq!(group, "g");
                assert_eq!(claimants.len(), 2);
            }
            other => panic!("wrong diagnostic: {other:?}"),
        }
    }

    /// A group's alternatives all get the same generated input, so one that
    /// expects a different type cannot be handed it. Caught here, named, and
    /// before anything runs - otherwise it is a downcast panic from inside
    /// the scheduler.
    #[test]
    fn an_alternative_expecting_the_wrong_input_type_is_rejected() {
        let regs = leak(vec![
            alt::<Vec<i32>>("right", "g", true, "Vec<i32>"),
            alt::<String>("wrong", "g", false, "String"),
        ]);
        let gens = leak_gens(vec![generator::<Vec<i32>>("g", "Vec<i32>")]);
        let problems = plan(&regs, &gens).expect_err("types must agree");
        match &problems[0] {
            Diagnostic::InputTypeMismatch {
                group,
                member,
                generator_type,
                member_type,
            } => {
                assert_eq!(group, "g");
                assert_eq!(member, "wrong");
                assert_eq!(*generator_type, "Vec<i32>");
                assert_eq!(*member_type, "String");
            }
            other => panic!("wrong diagnostic: {other:?}"),
        }
    }

    /// A group with no declared generator takes `()`, so its alternatives
    /// must expect `()` - and one that does not is the same mistake as
    /// above, not a special case.
    #[test]
    fn a_group_without_a_generator_expects_the_unit_input() {
        let ok = leak(vec![
            alt::<()>("a", "g", true, "()"),
            alt::<()>("b", "g", false, "()"),
        ]);
        assert!(plan(&ok, &[]).is_ok());

        let bad = leak(vec![
            alt::<()>("a", "h", true, "()"),
            alt::<Vec<i32>>("b", "h", false, "Vec<i32>"),
        ]);
        let problems = plan(&bad, &[]).expect_err("b wants an input this group does not make");
        assert!(
            matches!(&problems[0], Diagnostic::InputTypeMismatch { member, .. } if member == "b"),
            "{problems:?}",
        );
    }

    #[test]
    fn two_generators_for_one_group_are_rejected() {
        let regs = leak(vec![
            alt::<()>("a", "g", true, "()"),
            alt::<()>("b", "g", false, "()"),
        ]);
        let gens = leak_gens(vec![generator::<()>("g", "()"), generator::<()>("g", "()")]);
        let problems = plan(&regs, &gens).expect_err("a group has one shared input");
        assert!(
            matches!(&problems[0], Diagnostic::ManyGenerators { group, .. } if group == "g"),
            "{problems:?}",
        );
    }

    /// Every complaint at once, so that fixing a set of registrations does
    /// not mean one rebuild per mistake.
    #[test]
    fn every_problem_is_reported_not_just_the_first() {
        let regs = leak(vec![
            flat("dup"),
            flat("dup"),
            alt::<()>("lonely", "one", true, "()"),
            alt::<()>("x", "none", false, "()"),
            alt::<()>("y", "none", false, "()"),
        ]);
        let problems = plan(&regs, &[]).expect_err("three separate mistakes");
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems
            .iter()
            .any(|p| matches!(p, Diagnostic::DuplicateName { .. })));
        assert!(problems
            .iter()
            .any(|p| matches!(p, Diagnostic::LonelyGroup { .. })));
        assert!(problems
            .iter()
            .any(|p| matches!(p, Diagnostic::NoBaseline { .. })));
    }

    /// Groups are sorted too, for the same reason the flat ones are.
    #[test]
    fn groups_come_out_sorted() {
        let regs = leak(vec![
            alt::<()>("a", "zebra", true, "()"),
            alt::<()>("b", "zebra", false, "()"),
            alt::<()>("c", "apple", true, "()"),
            alt::<()>("d", "apple", false, "()"),
        ]);
        let plan = plan(&regs, &[]).unwrap();
        let names: Vec<&str> = plan.groups.iter().map(|g| g.name).collect();
        assert_eq!(names, ["apple", "zebra"]);
    }

    /// A group with no generator still yields a usable input maker, so that
    /// assembly has one code path rather than two.
    #[test]
    fn a_group_without_a_generator_still_makes_input() {
        let regs = leak(vec![
            alt::<()>("a", "g", true, "()"),
            alt::<()>("b", "g", false, "()"),
        ]);
        let plan = plan(&regs, &[]).unwrap();
        let make = plan.groups[0].make_input();
        assert_eq!(make().type_id(), TypeId::of::<()>());
    }

    /// Diagnostics are read by someone who has to go and find the
    /// registration, so they must name it.
    #[test]
    fn diagnostics_name_what_they_are_about() {
        let regs = leak(vec![alt::<()>("only", "lonely", true, "()")]);
        let shown = format!("{}", plan(&regs, &[]).unwrap_err()[0]);
        assert!(shown.contains("lonely"), "{shown}");
        assert!(shown.contains("only"), "{shown}");
    }
}

#[cfg(test)]
mod pairing {
    use crate::registry::ErasedInput;
    use crate::Config;
    use std::time::Duration;

    struct XorShift(u64);
    impl XorShift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn sum(v: &[u64]) -> u64 {
        v.iter().fold(0u64, |a, x| a.wrapping_add(*x))
    }

    /// Erasing a group's input must not cost it its pairing.
    ///
    /// This is the assumption `ErasedInput` exists to keep, and it is not
    /// visible to the type system: a comparison whose alternatives were
    /// handed *different* inputs still runs, still prints a number, and is
    /// simply worse - its error bar carries the spread between two draws on
    /// top of the spread it meant to measure. Nothing would say so.
    ///
    /// So measure it. The workload's cost varies by a factor of fifteen from
    /// round to round, which is exactly the spread pairing is supposed to
    /// cancel: both alternatives meet the same draw, so it cancels out of
    /// their difference while remaining in each of their individual error
    /// bars. If the sharing works, the paired error bar comes out far
    /// narrower than the two combined; if it broke, the two would be about
    /// equal.
    ///
    /// Measured both ways when this was written, on the same workload:
    ///
    /// ```none
    ///   shared input (what the registry does)   ratio 0.154
    ///   each alternative drawing its own        ratio 0.975
    /// ```
    ///
    /// So a half is a threshold with a wide margin either side, and it fails
    /// decisively rather than marginally if the sharing is ever lost.
    #[test]
    fn erasing_the_input_keeps_the_pairing() {
        let cfg = Config::relative(0.02)
            .with_max_time(Duration::from_millis(400))
            // `ComparisonSet::run` is a family of one comparison. Stage 6
            // removes this requirement; until then a standalone comparison
            // has to say so, or `Config::drop` complains.
            .with_comparisons_planned(1);
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);

        let results = cfg
            .comparison_gen_input(move || {
                let n = 200 + (rng.next() % 3000) as usize;
                ErasedInput::new((0..n as u64).collect::<Vec<u64>>())
            })
            .add_input("a", |e: &mut ErasedInput| sum(e.get_mut::<Vec<u64>>()))
            .add_input("b", |e: &mut ErasedInput| sum(e.get_mut::<Vec<u64>>()))
            .run();

        let (name, c) = results
            .against_baseline()
            .next()
            .expect("one alternative beyond the baseline");
        let combined = (c.candidate.std_error.powi(2) + c.baseline.std_error.powi(2)).sqrt();
        let paired = c.std_error();
        println!(
            "{name}: combined={combined:.4} paired={paired:.4} ratio={:.3}",
            paired / combined,
        );
        assert!(
            paired < combined / 2.0,
            "the paired error bar ({paired:.4}) is no better than combining the \
             halves ({combined:.4}), so the alternatives were not measured on \
             the same inputs - erasure lost the pairing",
        );
    }
}
