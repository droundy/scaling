//! Turning a pile of unordered registrations into a plan for a suite.
//!
//! Everything that can go wrong with a set of registrations is decided here,
//! before any benchmark runs. That matters for a reason particular to this
//! crate: running means claiming the machine, which pins threads and blocks
//! until whatever else is benchmarking gives the reserved CPUs back. A
//! mistake reported after that wait has cost real time and says nothing that
//! could not have been said immediately - the same reasoning a comparison set
//! already applies when it checks its alternative count before claiming.
//!
//! [`plan`](crate::assemble::plan) is therefore a pure function over slices:
//! it takes registrations
//! and returns either a plan or a list of complaints, touching nothing and
//! measuring nothing. That makes every diagnostic below testable without a
//! benchmark, a machine claim, or a linker.

use crate::registry::{Candidate, ErasedInput, Input, Registered};
use std::any::TypeId;
use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};

/// A crate version, ordered the way versions actually order.
///
/// Only for comparing the registrations of one crate against each other, so
/// it parses what `CARGO_PKG_VERSION` produces and no more: a leading
/// `major.minor.patch`, with anything after it - a `-rc.1`, a `+build` -
/// kept for display and ignored for ordering.
///
/// # Why not compare the strings
///
/// Because `"0.10.0" < "0.9.0"` as strings, which is backwards, and the
/// failure is quiet: a policy that picks the newest would pick 0.9.0 over
/// 0.10.0 and nothing would look wrong.
///
/// # Why not `semver`
///
/// Three integers do not justify a dependency, in a crate whose default
/// build has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    /// Parse `major.minor.patch`, treating anything unparseable as zero.
    ///
    /// A version that does not parse should not stop a benchmark run - the
    /// worst it can do is order oddly against its siblings, and every
    /// registration says which crate and version it came from anyway.
    pub fn parse(s: &str) -> Version {
        // Drop a pre-release or build suffix; neither orders here.
        let core = s.split(['-', '+']).next().unwrap_or(s);
        let mut it = core.split('.');
        let mut next = || {
            it.next()
                .and_then(|p| p.trim().parse::<u64>().ok())
                .unwrap_or(0)
        };
        Version {
            major: next(),
            minor: next(),
            patch: next(),
        }
    }
}

/// Where one registration came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Origin {
    pub crate_name: &'static str,
    pub crate_version: &'static str,
}

impl Origin {
    fn version(&self) -> Version {
        Version::parse(self.crate_version)
    }
}

/// What makes two registrations the same thing, apart from where they came
/// from.
///
/// Everything here has to match before two registrations can be versions of
/// each other. The `type_name` is the part it is easy to leave out and
/// expensive to: without it, the registrations `types(A, B)` makes from one
/// generic function look like one function registered twice, and asking for
/// only the latest of each crate drops all but one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Key {
    /// The matrix or comparison group, or `""` for a standalone benchmark.
    pub scope: &'static str,
    /// The input type as the source spells it, or `""` where there is none.
    pub type_name: &'static str,
    /// The registered name.
    pub name: &'static str,
}

/// One registration, with the name it should be reported under.
///
/// The name is owned because it is not always the registered one: when
/// several crates or versions register the same name they have to be told
/// apart, and the distinguishing part is worked out from the others present
/// rather than written down anywhere.
#[derive(Debug)]
pub struct Named<T: 'static> {
    pub name: String,
    pub reg: &'static T,
    pub origin: Origin,
}

/// Decide what to keep and what to call it, when a name is registered more
/// than once.
///
/// This arises when a crate pulls in an older copy of itself, or a rival
/// crate, as a dev-dependency with registrations enabled: both register,
/// and both may use the same name for the same idea. Every version is
/// kept, told apart by where it came from - `latest_per_crate` narrows
/// that to the newest per crate, which is what inputs are always
/// deduplicated with (see the call in `lanes_for_group`), regardless of
/// how candidates are treated.
///
/// Returns the survivors in the input's order; callers sort afterwards.
pub(crate) fn resolve_versions<T: 'static>(
    items: &[&'static T],
    key_of: impl Fn(&'static T) -> (Key, Origin),
    latest_per_crate: bool,
) -> Vec<Named<T>> {
    // Group on the whole key, not the name alone. Two things sharing a name
    // are versions of each other only if everything *else* about what they
    // are matches - the matrix they belong to and the type they take. Two
    // candidates called `sort` in different matrices are unrelated, and so
    // are the two registrations `types(String, Vec<u8>)` makes from one
    // generic function: they differ in type, which is the thing being
    // varied, not in version.
    let mut by_key: BTreeMap<Key, Vec<(&'static T, Origin)>> = BTreeMap::new();
    for it in items {
        let (key, origin) = key_of(it);
        by_key.entry(key).or_default().push((*it, origin));
    }

    let mut out = Vec::new();
    for (Key { name, .. }, mut sharers) in by_key {
        if sharers.len() == 1 {
            let (reg, origin) = sharers.pop().expect("just checked");
            out.push(Named {
                name: name.to_string(),
                reg,
                origin,
            });
            continue;
        }

        if latest_per_crate {
            // Newest of each crate, so rivals all survive and only a crate's
            // own strictly older copies are dropped. Ties within one crate -
            // two registrations at the same version - are kept rather than
            // one silently winning: a real version difference is exactly one
            // crate replacing an older copy of itself, but an equal version
            // is not that, it is the same crate registering one name twice.
            // Collapsing it here, the same way an older copy is dropped,
            // would hide precisely the duplicate the fallthrough below - and
            // the ordinary "keep every version" path below - exists to
            // report.
            let mut newest: BTreeMap<&'static str, Vec<(&'static T, Origin)>> = BTreeMap::new();
            for (reg, origin) in sharers {
                let held = newest.entry(origin.crate_name).or_default();
                match held.first() {
                    None => held.push((reg, origin)),
                    Some((_, held_origin)) => match origin.version().cmp(&held_origin.version()) {
                        std::cmp::Ordering::Greater => {
                            held.clear();
                            held.push((reg, origin));
                        }
                        std::cmp::Ordering::Equal => held.push((reg, origin)),
                        std::cmp::Ordering::Less => {}
                    },
                }
            }
            sharers = newest.into_values().flatten().collect();
            if sharers.len() == 1 {
                let (reg, origin) = sharers.pop().expect("just checked");
                out.push(Named {
                    name: name.to_string(),
                    reg,
                    origin,
                });
                continue;
            }
        }

        // Still more than one, so they need telling apart - by the least
        // that actually does it. Asking merely whether crates *differ* is
        // not the same question: with one implementation per crate, the
        // versions differ too, and appending them would make
        // `sort@mine-2.0.0` where `sort@mine` says everything.
        let mut crates: Vec<&str> = sharers.iter().map(|(_, o)| o.crate_name).collect();
        crates.sort_unstable();
        let n = crates.len();
        crates.dedup();
        let crate_alone_is_enough = crates.len() == n;
        let one_crate = crates.len() == 1;
        for (reg, origin) in sharers {
            let suffix = if crate_alone_is_enough {
                origin.crate_name.to_string()
            } else if one_crate {
                origin.crate_version.to_string()
            } else {
                format!("{}-{}", origin.crate_name, origin.crate_version)
            };
            // Two registrations of one name from one crate at one version
            // really are duplicates. They fall through here with the same
            // suffix as each other, so they stay sharing a name and the
            // duplicate check downstream reports them - which is what should
            // happen, since nothing distinguishes them.
            let full = if suffix.is_empty() {
                name.to_string()
            } else {
                format!("{name}@{suffix}")
            };
            out.push(Named {
                name: full,
                reg,
                origin,
            });
        }
    }
    out
}

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
    /// A comparison group where more than one alternative claims to be the
    /// baseline.
    ManyBaselines {
        group: String,
        claimants: Vec<String>,
    },
    /// A matrix candidate whose lane holds no inputs, or vice versa.
    ///
    /// Not a contradiction, so not an error - but almost always a typo or a
    /// type that is not what the writer thought, so it is said out loud and
    /// the entry skipped.
    OrphanCandidate {
        matrix: String,
        name: String,
        type_name: &'static str,
    },
    /// A matrix input no candidate in its matrix takes.
    OrphanInput {
        matrix: String,
        name: String,
        type_name: &'static str,
    },
    /// Two candidates, or two inputs, of one name in one matrix.
    DuplicateMatrixEntry {
        matrix: String,
        /// `"candidate"` or `"input"`.
        what: &'static str,
        name: String,
    },
    /// Two types spelled alike in one lane are not the same type.
    MatrixTypeMismatch {
        matrix: String,
        candidate: String,
        type_name: &'static str,
    },
}

impl Diagnostic {
    /// Whether this one means work the caller wrote is not being measured.
    ///
    /// The distinction matters because these travel by different routes. An
    /// orphan is a remark: something registered is unused, and everything
    /// else still runs. A contradiction is not - the lane holding it is
    /// discarded, so benchmarks that were written produce nothing, and
    /// saying so only in a field of the returned value means a caller who
    /// writes `suite.try_add_registered().unwrap();` and drops the
    /// result sees a run that silently measured nothing.
    pub fn is_fatal(&self) -> bool {
        !matches!(
            self,
            Diagnostic::OrphanCandidate { .. } | Diagnostic::OrphanInput { .. }
        )
    }
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
            Diagnostic::ManyBaselines { group, claimants } => write!(
                f,
                "group `{group}` has more than one baseline: {}",
                list(claimants),
            ),
            Diagnostic::OrphanCandidate {
                matrix: group,
                name,
                type_name,
            } => write!(
                f,
                "in group `{group}`, `{name}` takes `{type_name}` but no input of \
                 that type is registered, so it was measured on nothing",
            ),
            Diagnostic::OrphanInput {
                matrix: group,
                name,
                type_name,
            } => write!(
                f,
                "in group `{group}`, the input `{name}` produces `{type_name}` but no \
                 candidate takes that type, so nothing was measured on it",
            ),
            Diagnostic::DuplicateMatrixEntry {
                matrix: group,
                what,
                name,
            } => {
                write!(f, "group `{group}` has two {what}s called `{name}`",)
            }
            Diagnostic::MatrixTypeMismatch {
                matrix: group,
                candidate,
                type_name,
            } => write!(
                f,
                "in group `{group}`, `{candidate}` takes a different `{type_name}` from \
                 the one the inputs produce - two types of the same name are still two types",
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
    pub flat: Vec<Named<Registered>>,
    /// Comparison lanes, sorted by group name and then by type name.
    pub lanes: Vec<Lane>,
}

/// One group's candidates and inputs of a single type, paired up.
///
/// A group partitions into lanes rather than being one grid, because
/// candidates and inputs are registered independently and need not all agree
/// about the type. Pairing within a lane is what lets one group hold several
/// unrelated type families and still be correct - a `String` candidate is
/// simply never handed a `Vec<u8>`.
///
/// A lane with one candidate still uses an input group, but its result is a
/// plain [`Stats`](crate::Stats) rather than a comparison report. The runner
/// makes that distinction when it consumes a `Lane`.
#[derive(Debug)]
pub struct Lane {
    /// The group this lane belongs to.
    pub group: &'static str,
    /// The input type shared by everything in it, as the source spells it.
    pub type_name: &'static str,
    /// Candidates, baseline first, then sorted by name.
    ///
    /// Named rather than bare registrations, because when several crates or
    /// versions register one name they have to be told apart, and the name
    /// that distinguishes them is worked out from what else is present.
    pub candidates: Vec<Named<Candidate>>,
    /// Inputs, sorted by name, one per name.
    ///
    /// Unlike candidates, inputs are *deduplicated* across versions rather
    /// than disambiguated. Two versions of one implementation are the point
    /// of a cross-version comparison; two versions of one input generator
    /// are meant to build the same data, so measuring on both would double
    /// the work to no end - and worse, an old generator paired with new
    /// implementations quietly changes what is being measured if the
    /// generator itself has changed since.
    ///
    /// A group with candidates but no registered input at all gets a single
    /// synthetic entry here whose `name` is empty - the unit input every
    /// no-input group has always implicitly taken. `comparison_name` and
    /// `flat_name` both recognise that empty name and omit the `@input`
    /// suffix it would otherwise produce, which is what keeps a plain
    /// `group = "sort"` declaration, with no `#[input]` anywhere, printing
    /// as `sort` rather than `sort@`. A group with a real, named
    /// `#[input]` - even just the one - is named `group@input` regardless,
    /// the same convention a matrix's lanes have always used: only the
    /// implicit unit input is special-cased away.
    pub inputs: Vec<Named<Input>>,
    /// Whether this lane shares an input name with another lane of the same
    /// group - two different types, both with an input called `small`,
    /// say - which `comparison_name` needs to know: it disambiguates only
    /// when that has actually happened, the same "carries only what
    /// distinguishes them" rule `resolve_versions` follows for names that
    /// collide across crates or versions. Set once, after every lane of a
    /// group is known, by comparing input names across them.
    pub needs_type_suffix: bool,
}

impl Lane {
    /// What a cell of this lane is called.
    ///
    /// A lane with two or more candidates is named for the group and input.
    /// A lone candidate is named for the candidate as well, since it has no
    /// baseline to compare against - see [`Lane::flat_name`].
    ///
    /// The type is appended only when [`Lane::needs_type_suffix`] says
    /// another lane of this group has an input of the same name - two lanes
    /// each with a `small` input, say. Without that, `group@small` from one
    /// lane collides with `group@small` from the other: both resolve to the
    /// same report entry, and the second silently overwrites the first's
    /// result.
    pub fn comparison_name(&self, input: &Named<Input>) -> String {
        if input.name.is_empty() {
            if self.needs_type_suffix {
                format!("{} ({})", self.group, self.type_name)
            } else {
                self.group.to_string()
            }
        } else if self.needs_type_suffix {
            format!("{}@{} ({})", self.group, input.name, self.type_name)
        } else {
            format!("{}@{}", self.group, input.name)
        }
    }

    pub fn flat_name(&self, candidate: &Named<Candidate>, input: &Named<Input>) -> String {
        if input.name.is_empty() {
            format!("{}::{}", self.group, candidate.name)
        } else {
            format!("{}::{}@{}", self.group, candidate.name, input.name)
        }
    }
}

/// The unit input every group with candidates but no declared `#[input]`
/// implicitly takes - see [`Lane::inputs`].
fn unit_input() -> Named<Input> {
    static UNIT: Input = Input {
        groups: &[],
        name: "",
        crate_name: "",
        crate_version: "",
        type_id: TypeId::of::<()>,
        type_name: "()",
        make: || ErasedInput::new(()),
    };
    Named {
        name: String::new(),
        reg: &UNIT,
        origin: Origin {
            crate_name: "",
            crate_version: "",
        },
    }
}

/// Partition one group's candidates and inputs into lanes and pair them up.
///
/// Pure, like [`plan`], and for the same reason: everything that can be
/// wrong is decided before the machine is claimed. `group` is fixed for the
/// whole call - [`plan`] calls this once per group name, having already
/// exploded every candidate and input across the (possibly several) groups
/// it belongs to.
///
/// # Orphans are warnings, not errors
///
/// A candidate whose lane has no inputs, or an input whose lane has no
/// candidates, is almost always a typo or a type that does not match what
/// the writer thought - but it is not a contradiction, and rejecting the
/// whole run over it would be unhelpful when the rest is fine. So orphans
/// are reported and skipped. The one exception is a candidate lane with no
/// input at all whose type is `()`: that is not an orphan, it is a group
/// with nothing to say about its input, which has always meant "the unit
/// input" - see [`unit_input`].
fn lanes_for_group(
    group: &'static str,
    candidates: &[&'static Candidate],
    inputs: &[&'static Input],
) -> (Vec<Lane>, Vec<Diagnostic>) {
    let mut problems = Vec::new();

    // Decide what survives and what each is called, before anything else, so
    // that two versions of one implementation stop looking like a duplicate
    // and start looking like the comparison they are. Every version is
    // kept - see `resolve_versions`.
    let named_c = resolve_versions(
        candidates,
        |c| {
            (
                Key {
                    scope: group,
                    type_name: c.input_type_name,
                    name: c.name,
                },
                Origin {
                    crate_name: c.crate_name,
                    crate_version: c.crate_version,
                },
            )
        },
        false,
    );
    // Inputs are deduplicated rather than disambiguated - see `Lane::inputs`
    // - so they are always narrowed to the newest per crate.
    let named_i = resolve_versions(
        inputs,
        |i| {
            (
                Key {
                    scope: group,
                    type_name: i.type_name,
                    name: i.name,
                },
                Origin {
                    crate_name: i.crate_name,
                    crate_version: i.crate_version,
                },
            )
        },
        true,
    );
    // That leaves one per crate; an input registered by two different
    // crates is still redundant, so keep the newest of those too.
    // Keyed on the type as well as the name, matching the lane key. Inputs
    // get named for their character - "small", "large" - so one group
    // holding two type lanes very naturally has an input called `small` in
    // each. Keyed on the name alone those two look redundant, and dropping
    // one orphans a whole lane.
    let mut best: BTreeMap<(&'static str, &'static str), Named<Input>> = BTreeMap::new();
    for n in named_i {
        let key = (n.reg.type_name, n.reg.name);
        match best.get(&key) {
            // Same crate, same version: not redundancy, a genuine
            // duplicate - two registrations of one input, from the same
            // source. Silently keeping either would be exactly the
            // "regardless of origin" collapse this diagnostic exists to
            // catch, so it is reported rather than resolved. `held` is left
            // as it is, matching how the candidate duplicate check below
            // reports every extra claimant rather than only the first.
            Some(held)
                if held.origin.crate_name == n.origin.crate_name
                    && Version::parse(held.origin.crate_version)
                        == Version::parse(n.origin.crate_version) =>
            {
                problems.push(Diagnostic::DuplicateMatrixEntry {
                    matrix: group.to_string(),
                    what: "input",
                    name: n.reg.name.to_string(),
                });
            }
            // A different crate, or an older version of this one: this is
            // the intended case the comment above describes - genuinely
            // redundant, so keep the newest and say nothing.
            Some(held)
                if Version::parse(held.origin.crate_version)
                    >= Version::parse(n.origin.crate_version) => {}
            _ => {
                best.insert(key, n);
            }
        }
    }
    let named_i: Vec<Named<Input>> = best.into_values().collect();

    // Input type is the lane key. `TypeId` is not `Ord`, so bucket by the
    // type's name, which the macro takes from the source: two different
    // types cannot spell themselves the same way within one crate, and the
    // id is checked below in any case.
    let mut cands: BTreeMap<&'static str, Vec<Named<Candidate>>> = BTreeMap::new();
    let mut ins: BTreeMap<&'static str, Vec<Named<Input>>> = BTreeMap::new();
    for c in named_c {
        cands.entry(c.reg.input_type_name).or_default().push(c);
    }
    for i in named_i {
        ins.entry(i.reg.type_name).or_default().push(i);
    }

    // Names that are still shared after version resolution really are
    // duplicates - two registrations of one name from one crate at one
    // version - and would make two rows indistinguishable.
    for cs in cands.values() {
        // Counted on the resolved name, since that is what would actually
        // collide in a report - but *reported* under the registered one,
        // which is what the writer typed. A message about `same@1.0.0` when
        // the source says `same` sends the reader looking for the wrong
        // thing.
        let mut seen: BTreeMap<&str, (usize, &'static str)> = BTreeMap::new();
        for c in cs {
            let e = seen.entry(c.name.as_str()).or_insert((0, c.reg.name));
            e.0 += 1;
        }
        for (_, (n, registered)) in seen {
            if n > 1 {
                problems.push(Diagnostic::DuplicateMatrixEntry {
                    matrix: group.to_string(),
                    what: "candidate",
                    name: registered.to_string(),
                });
            }
        }
    }

    let mut lanes = Vec::new();
    let mut keys: Vec<&'static str> = cands.keys().chain(ins.keys()).copied().collect();
    keys.sort_unstable();
    keys.dedup();

    for type_name in keys {
        let mut cs = cands.remove(type_name).unwrap_or_default();
        let mut is = ins.remove(type_name).unwrap_or_default();

        if is.is_empty() {
            // No input was registered for this (group, type) at all. When
            // the type is `()` that is not a mistake - it is what a group
            // with nothing to say about its input has always meant - so the
            // unit input is synthesized rather than treating every candidate
            // as an orphan.
            if type_name == "()" {
                is.push(unit_input());
            } else {
                for c in &cs {
                    problems.push(Diagnostic::OrphanCandidate {
                        matrix: group.to_string(),
                        name: c.name.clone(),
                        type_name: c.reg.input_type_name,
                    });
                }
                continue;
            }
        }
        if cs.is_empty() {
            for i in &is {
                problems.push(Diagnostic::OrphanInput {
                    matrix: group.to_string(),
                    name: i.name.clone(),
                    type_name: i.reg.type_name,
                });
            }
            continue;
        }

        // The name-keyed lane must agree on the actual type too, or a
        // downcast would go wrong later on two types that happen to be
        // spelled alike in different modules.
        let want = (is[0].reg.type_id)();
        let mut mismatched = false;
        // The inputs are checked against each other as well as the
        // candidates against them. Checking only the candidates left a hole:
        // the lane is bucketed on the type's *name*, so two inputs spelling
        // their types alike sit in one lane whatever they really are, and a
        // candidate that agrees with the first would still be handed the
        // second - a downcast panic from inside the scheduler, which is
        // precisely what this check exists to turn into a named error.
        for i in &is {
            if (i.reg.type_id)() != want {
                problems.push(Diagnostic::MatrixTypeMismatch {
                    matrix: group.to_string(),
                    candidate: i.name.clone(),
                    type_name: i.reg.type_name,
                });
                mismatched = true;
            }
        }
        for c in &cs {
            if (c.reg.input_type)() != want {
                problems.push(Diagnostic::MatrixTypeMismatch {
                    matrix: group.to_string(),
                    candidate: c.name.clone(),
                    type_name: c.reg.input_type_name,
                });
                mismatched = true;
            }
        }
        if mismatched {
            continue;
        }

        is.sort_by(|a, b| a.name.cmp(&b.name));
        cs.sort_by(|a, b| a.name.cmp(&b.name));

        // Baseline: whoever said so, else the first by name. Several
        // claimants is what happens when the same source is registered at
        // two versions - both say `baseline`, because they are the same
        // line of code - so a policy settles it rather than an error.
        let claimants: Vec<usize> = cs
            .iter()
            .enumerate()
            .filter(|(_, c)| c.reg.is_baseline)
            .map(|(i, _)| i)
            .collect();
        // Several claimants is expected when one declaration is registered at
        // several versions - they all say `baseline` because they are the
        // same line of code - and a policy settles that. Two *different*
        // functions claiming it within one crate at one version is not that:
        // it is a plain contradiction, and resolving it by version would
        // silently pick one and hide the mistake.
        let mut per_origin: BTreeMap<(&str, &str), usize> = BTreeMap::new();
        for i in &claimants {
            *per_origin
                .entry((cs[*i].origin.crate_name, cs[*i].origin.crate_version))
                .or_insert(0) += 1;
        }
        let contradicts_itself = per_origin.values().any(|n| *n > 1);
        let baseline: String = match claimants.len() {
            0 => cs[0].name.clone(),
            1 => cs[claimants[0]].name.clone(),
            _ if contradicts_itself => {
                problems.push(Diagnostic::ManyBaselines {
                    group: group.to_string(),
                    claimants: claimants
                        .iter()
                        .map(|i| {
                            format!(
                                "{}@{} ({})",
                                cs[*i].origin.crate_name, cs[*i].origin.crate_version, cs[*i].name,
                            )
                        })
                        .collect(),
                });
                continue;
            }
            _ => cs[oldest_claimant(&cs, &claimants)].name.clone(),
        };
        cs.sort_by(|a, b| (a.name != baseline, &a.name).cmp(&(b.name != baseline, &b.name)));

        lanes.push(Lane {
            group,
            type_name,
            candidates: cs,
            inputs: is,
            // Set in `plan`, once every lane of every group is known - a
            // lane cannot tell by itself whether another lane of its group
            // shares one of its input names.
            needs_type_suffix: false,
        });
    }

    (lanes, problems)
}

/// The oldest of several claimants to a comparison's baseline - the one
/// that makes a regression read the right way round: "is the new code
/// faster than the old code" wants the old code as the thing being measured
/// against. `claimants` is never empty where this is called.
fn oldest_claimant<T: 'static>(cs: &[Named<T>], claimants: &[usize]) -> usize {
    claimants
        .iter()
        .copied()
        .min_by_key(|i| Version::parse(cs[*i].origin.crate_version))
        .expect("claimants is non-empty")
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
///
/// # Multi-group membership
///
/// A candidate or input can name several groups - `quicksort` compared
/// under both `"small_sort"` and `"big_sort"`, say, or one shared input
/// feeding `"sort"`, `"dedup"` and `"contains"` without those being compared
/// with each other. So the first step here explodes every candidate and
/// input across each group it names, before anything is paired up; from
/// that point on, each group name is handled entirely independently.
pub fn plan(
    regs: &[&'static Registered],
    candidates: &[&'static Candidate],
    inputs: &[&'static Input],
) -> (Plan, Vec<Diagnostic>) {
    let mut problems = Vec::new();

    let named = resolve_versions(
        regs,
        |r| {
            (
                Key {
                    scope: "",
                    type_name: "",
                    name: r.name,
                },
                Origin {
                    crate_name: r.crate_name,
                    crate_version: r.crate_version,
                },
            )
        },
        false,
    );

    // Names still shared afterwards are genuine duplicates. Checked first
    // because a duplicate makes every later message ambiguous about which
    // one it means.
    let mut by_name: BTreeMap<&str, Vec<&Named<Registered>>> = BTreeMap::new();
    for r in &named {
        by_name.entry(r.name.as_str()).or_default().push(r);
    }
    for rs in by_name.values() {
        if rs.len() > 1 {
            problems.push(Diagnostic::DuplicateName {
                // The registered name, not the resolved one: a message about
                // `same@1.0.0` when the source says `same` sends the reader
                // looking for the wrong thing.
                name: rs[0].reg.name.to_string(),
                sources: rs.iter().map(|r| source(r.reg)).collect(),
            });
        }
    }

    let mut flat: Vec<Named<Registered>> = named;
    flat.sort_by(|a, b| a.name.cmp(&b.name));

    // Explode by group membership: one candidate or input registered under
    // several groups lands in every one of those groups' buckets.
    let mut cands_by_group: BTreeMap<&'static str, Vec<&'static Candidate>> = BTreeMap::new();
    for c in candidates {
        for g in c.groups {
            cands_by_group.entry(g).or_default().push(c);
        }
    }
    let mut inputs_by_group: BTreeMap<&'static str, Vec<&'static Input>> = BTreeMap::new();
    for i in inputs {
        for g in i.groups {
            inputs_by_group.entry(g).or_default().push(i);
        }
    }
    let mut group_names: Vec<&'static str> = cands_by_group
        .keys()
        .chain(inputs_by_group.keys())
        .copied()
        .collect();
    group_names.sort_unstable();
    group_names.dedup();

    let mut lanes = Vec::new();
    for group in group_names {
        let group_cands = cands_by_group.remove(group).unwrap_or_default();
        let group_inputs = inputs_by_group.remove(group).unwrap_or_default();
        let (group_lanes, group_problems) = lanes_for_group(group, &group_cands, &group_inputs);
        lanes.extend(group_lanes);
        problems.extend(group_problems);
    }

    // A lane's input names can collide with another lane's in the same
    // group - two different types, each with an input called `small`, say.
    // Disambiguate only the lanes where that has actually happened, the
    // same rule `resolve_versions` follows for names that collide across
    // crates or versions: carry only what distinguishes them.
    // Owned `String` keys in the inner map, not `&str` borrowed from
    // `lanes` itself - a name lives inside `lane.inputs`, which lives
    // inside `lanes`, so borrowing it here would keep `lanes` immutably
    // borrowed right through the `&mut lanes` pass below.
    let mut counts_by_group: BTreeMap<&str, BTreeMap<String, usize>> = BTreeMap::new();
    for lane in &lanes {
        let counts = counts_by_group.entry(lane.group).or_default();
        for i in &lane.inputs {
            *counts.entry(i.name.clone()).or_insert(0) += 1;
        }
    }
    for lane in &mut lanes {
        let counts = &counts_by_group[lane.group];
        lane.needs_type_suffix = lane.inputs.iter().any(|i| counts[&i.name] > 1);
    }

    (Plan { flat, lanes }, problems)
}

#[cfg(test)]
mod tests {
    use super::lane_tests::*;
    use super::*;
    use crate::registry::{Kind, Suite};
    use std::any::TypeId;

    // Shim that does nothing. Assembly never calls it - it decides what
    // *would* be run - so a plan can be checked without a machine claim, a
    // benchmark, or a linker.
    fn noop_flat(adder: &mut Suite<'_>, name: &str) {
        adder.add(name, || ());
    }

    /// A standalone benchmark.
    fn flat(name: &'static str) -> Registered {
        Registered {
            name,
            crate_name: "testcrate",
            crate_version: "1.0.0",
            kind: Kind::Flat(noop_flat),
        }
    }

    /// `plan` borrows `'static` registrations, which in a test have to be
    /// leaked - they really are `'static` in the real thing, coming from
    /// linker sections.
    fn leak(rs: Vec<Registered>) -> Vec<&'static Registered> {
        rs.into_iter().map(|r| &*Box::leak(Box::new(r))).collect()
    }

    /// Registrations arrive in whatever order the linker chose, so a report
    /// built from them must impose its own - or two runs of the same
    /// benchmarks could not be diffed.
    #[test]
    fn flat_benchmarks_come_out_sorted() {
        let regs = leak(vec![flat("zebra"), flat("apple"), flat("middle")]);
        let (plan, problems) = plan(&regs, &[], &[]);
        assert!(problems.is_empty(), "{problems:?}");
        let names: Vec<&str> = plan.flat.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["apple", "middle", "zebra"]);
        assert!(plan.lanes.is_empty());
    }

    /// Reversing the input must not change the plan: that is what "sorted"
    /// is for, and a sort that happened to agree with the input order would
    /// pass a single-order test while proving nothing.
    #[test]
    fn the_order_registrations_arrive_in_does_not_matter() {
        let forward = leak(vec![flat("a"), flat("b"), flat("c")]);
        let backward = leak(vec![flat("c"), flat("b"), flat("a")]);
        let (one, _) = plan(&forward, &[], &[]);
        let (two, _) = plan(&backward, &[], &[]);
        let names =
            |p: &Plan| -> Vec<String> { p.flat.iter().map(|r| r.name.to_string()).collect() };
        assert_eq!(names(&one), names(&two));
    }

    /// The baseline goes first because `InputGroup` takes its first
    /// alternative as the baseline - that ordering is how the decision made
    /// here reaches the set built later.
    #[test]
    fn the_baseline_is_placed_first() {
        let cs = leak_c(vec![
            cand::<()>("g", "a_first_alphabetically", "()", false),
            cand::<()>("g", "z_the_baseline", "()", true),
            cand::<()>("g", "m_middle", "()", false),
        ]);
        let (plan, problems) = plan(&[], &cs, &[]);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(plan.lanes.len(), 1);
        let names: Vec<&str> = plan.lanes[0]
            .candidates
            .iter()
            .map(|c| c.name.as_str())
            .collect();
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
        let (_, problems) = plan(&regs, &[], &[]);
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

    /// A lone candidate has no difference to report, but remains a valid
    /// singleton lane. The consumer decides whether to report its result as
    /// `Stats` or as a one-entry group.
    #[test]
    fn a_lone_candidate_is_kept_not_rejected() {
        let cs = leak_c(vec![cand::<()>("lonely", "only", "()", true)]);
        let (plan, problems) = plan(&[], &cs, &[]);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(plan.lanes.len(), 1);
        assert_eq!(plan.lanes[0].candidates.len(), 1);
    }

    /// With nobody marked, the first by name is the baseline - a group
    /// should be writable without ceremony, matching a matrix's behaviour.
    #[test]
    fn an_unmarked_group_takes_the_first_name_as_baseline() {
        let cs = leak_c(vec![
            cand::<()>("g", "b", "()", false),
            cand::<()>("g", "a", "()", false),
        ]);
        let (plan, problems) = plan(&[], &cs, &[]);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(plan.lanes[0].candidates[0].name, "a");
    }

    #[test]
    fn a_group_with_two_baselines_is_rejected() {
        let cs = leak_c(vec![
            cand::<()>("g", "a", "()", true),
            cand::<()>("g", "b", "()", true),
        ]);
        let (plan, problems) = plan(&[], &cs, &[]);
        assert!(plan.lanes.is_empty(), "the lane cannot be trusted");
        match &problems[0] {
            Diagnostic::ManyBaselines { group, claimants } => {
                assert_eq!(group, "g");
                assert_eq!(claimants.len(), 2);
            }
            other => panic!("wrong diagnostic: {other:?}"),
        }
    }

    /// A candidate expecting a different input type from the rest of its
    /// group is not a hard error the way it was for today's groups - it
    /// simply lands in its own, orphaned, lane. This is the behaviour
    /// change unifying with matrices brings.
    #[test]
    fn candidates_expecting_different_input_types_auto_split() {
        let cs = leak_c(vec![
            cand::<Vec<i32>>("g", "right", "Vec<i32>", true),
            cand::<String>("g", "wrong", "String", false),
        ]);
        let is = leak_i(vec![inp::<Vec<i32>>("g", "data", "Vec<i32>")]);
        let (plan, problems) = plan(&[], &cs, &is);
        assert_eq!(plan.lanes.len(), 1, "only the matching type forms a lane");
        assert_eq!(plan.lanes[0].candidates[0].name, "right");
        assert!(
            problems
                .iter()
                .any(|p| matches!(p, Diagnostic::OrphanCandidate { name, .. } if name == "wrong")),
            "{problems:?}",
        );
    }

    /// A group with no declared input takes the unit input implicitly, and
    /// keeps the plain name a no-input group has always had.
    #[test]
    fn a_group_without_an_input_takes_the_unit_input() {
        let cs = leak_c(vec![
            cand::<()>("g", "a", "()", true),
            cand::<()>("g", "b", "()", false),
        ]);
        let (plan, problems) = plan(&[], &cs, &[]);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(plan.lanes.len(), 1);
        assert_eq!(plan.lanes[0].inputs.len(), 1);
        assert_eq!((plan.lanes[0].inputs[0].reg.type_id)(), TypeId::of::<()>());
        assert_eq!(
            plan.lanes[0].comparison_name(&plan.lanes[0].inputs[0]),
            "g",
            "a no-input group keeps its plain name",
        );
    }

    /// Every complaint at once, so that fixing a set of registrations does
    /// not mean one rebuild per mistake.
    #[test]
    fn every_problem_is_reported_not_just_the_first() {
        let regs = leak(vec![flat("dup"), flat("dup")]);
        let cs = leak_c(vec![
            cand::<()>("one", "a", "()", true),
            cand::<()>("one", "b", "()", true),
        ]);
        let (_, problems) = plan(&regs, &cs, &[]);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems
            .iter()
            .any(|p| matches!(p, Diagnostic::DuplicateName { .. })));
        assert!(problems
            .iter()
            .any(|p| matches!(p, Diagnostic::ManyBaselines { .. })));
    }

    /// Groups are sorted too, for the same reason the flat ones are.
    #[test]
    fn groups_come_out_sorted() {
        let cs = leak_c(vec![
            cand::<()>("zebra", "a", "()", true),
            cand::<()>("zebra", "b", "()", false),
            cand::<()>("apple", "c", "()", true),
            cand::<()>("apple", "d", "()", false),
        ]);
        let (plan, _) = plan(&[], &cs, &[]);
        let names: Vec<&str> = plan.lanes.iter().map(|l| l.group).collect();
        assert_eq!(names, ["apple", "zebra"]);
    }

    /// Diagnostics are read by someone who has to go and find the
    /// registration, so they must name it.
    #[test]
    fn diagnostics_name_what_they_are_about() {
        let cs = leak_c(vec![
            cand::<()>("g", "a", "()", true),
            cand::<()>("g", "b", "()", true),
        ]);
        let (_, problems) = plan(&[], &cs, &[]);
        let shown = format!("{}", problems[0]);
        assert!(shown.contains('g'), "{shown}");
    }

    /// A candidate can belong to several groups at once - `quicksort`
    /// compared under both `small_sort` and `big_sort`, say - without those
    /// groups sharing anything else. This is the multi-membership case
    /// nothing before this redesign exercised.
    #[test]
    fn a_candidate_can_belong_to_more_than_one_group() {
        let quicksort = cand_in::<()>(&["small_sort", "big_sort"], "quicksort", "()", false);
        let insertion = cand::<()>("small_sort", "insertion", "()", true);
        let cs = leak_c(vec![quicksort, insertion]);
        let (plan, problems) = plan(&[], &cs, &[]);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(plan.lanes.len(), 2);

        let small = plan
            .lanes
            .iter()
            .find(|l| l.group == "small_sort")
            .expect("quicksort named this group");
        assert_eq!(
            small.candidates.len(),
            2,
            "quicksort is compared against insertion here",
        );

        let big = plan
            .lanes
            .iter()
            .find(|l| l.group == "big_sort")
            .expect("quicksort named this group too");
        assert_eq!(
            big.candidates.len(),
            1,
            "quicksort has nothing to compare against here",
        );
        assert_eq!(big.candidates[0].name, "quicksort");
    }

    /// One input can feed several groups at once without those groups being
    /// compared with each other - a shared sorted `Vec` used by `sort` and
    /// `dedup`, say.
    #[test]
    fn an_input_can_feed_more_than_one_group() {
        let shared = inp_in::<Vec<i32>>(&["sort", "dedup"], "sorted_vec", "Vec<i32>");
        let cs = leak_c(vec![
            cand::<Vec<i32>>("sort", "std_sort", "Vec<i32>", true),
            cand::<Vec<i32>>("dedup", "std_dedup", "Vec<i32>", true),
        ]);
        let is = leak_i(vec![shared]);
        let (plan, problems) = plan(&[], &cs, &is);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(plan.lanes.len(), 2);
        for lane in &plan.lanes {
            assert_eq!(lane.inputs.len(), 1);
            assert_eq!(lane.inputs[0].name, "sorted_vec");
            assert_eq!(
                lane.candidates.len(),
                1,
                "sort and dedup share an input, not a comparison",
            );
        }
    }
}

#[cfg(test)]
mod pairing {
    use crate::registry::ErasedInput;
    use crate::testutil::{quiesced, XorShift};
    use crate::Config;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    fn sum(v: &[u64]) -> u64 {
        v.iter().fold(0u64, |a, x| a.wrapping_add(*x))
    }

    /// Erasing a group's input must not cost it its pairing.
    ///
    /// This is the assumption `ErasedInput` exists to keep, and it is not
    /// visible to the type system: a comparison whose alternatives were
    /// handed *different* inputs still runs, still prints a number, and is
    /// simply worse. Nothing would say so.
    ///
    /// # Why this asks what the alternatives saw, rather than timing them
    ///
    /// The first version of this test timed the two and asserted the paired
    /// error bar came out narrower than combining the halves. That measured
    /// the right thing on a quiet machine - about 0.15 here - and failed on
    /// CI at 0.75. Nor was measuring a control instead enough: under a
    /// saturated machine both come out at 1.0 and the comparison is a coin
    /// flip. That is not a flaw in the assertion but a fact about pairing.
    /// It cancels the spread in the *workload*; a busy machine adds a second
    /// spread, between one instant and the next, which is not shared between
    /// two alternatives and so does not cancel. When that one dominates,
    /// there is nothing left to see.
    ///
    /// What actually has to be true is simpler and has no timing in it: both
    /// alternatives are handed the same value. So that is what is asserted,
    /// and it holds on any machine. The statistical consequence is checked
    /// separately, where it can be.
    #[test]
    fn erasing_the_input_hands_both_alternatives_the_same_value() {
        // What each alternative was given, round by round.
        let seen_a: Rc<RefCell<Vec<Vec<u64>>>> = Rc::new(RefCell::new(Vec::new()));
        let seen_b: Rc<RefCell<Vec<Vec<u64>>>> = Rc::new(RefCell::new(Vec::new()));
        let (rec_a, rec_b) = (seen_a.clone(), seen_b.clone());

        let cfg = Config::relative(0.5).with_max_time(Duration::from_millis(50));
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
        let _ = cfg
            .input_group_make_input(move || {
                // Varying, so that "they saw the same thing" is a real
                // claim rather than one a constant would satisfy.
                let n = 4 + (rng.next() % 16) as usize;
                ErasedInput::new(
                    (0..n as u64)
                        .map(|x| x * 7 + n as u64)
                        .collect::<Vec<u64>>(),
                )
            })
            .add_input("a", |e: &mut ErasedInput| {
                let v = e.get_mut::<Vec<u64>>();
                rec_a.borrow_mut().push(v.clone());
                sum(v)
            })
            .add_input("b", |e: &mut ErasedInput| {
                let v = e.get_mut::<Vec<u64>>();
                rec_b.borrow_mut().push(v.clone());
                sum(v)
            })
            .run();

        let a = seen_a.borrow();
        let b = seen_b.borrow();
        assert!(!a.is_empty(), "the comparison ran at all");
        assert_eq!(a.len(), b.len(), "both alternatives ran equally often");
        assert!(
            a.iter().any(|v| v.len() != a[0].len()),
            "the generator must actually vary, or this asserts nothing",
        );
        // The alternatives run in a rotating order, so the *i*th value each
        // saw is the *i*th one generated for both of them.
        for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            assert_eq!(
                x, y,
                "on round {i} the two alternatives were handed different \
                 inputs, so their difference carries the gap between two \
                 draws as well as the one it meant to measure",
            );
        }
    }

    /// And the consequence: with the input shared, the paired error bar is
    /// narrower than combining the two halves.
    ///
    /// Only checkable on a machine quiet enough for a timing assertion to
    /// mean anything, for the reason given above - so it is gated, like the
    /// crate's other statistical tests. The mechanism it follows from is
    /// checked unconditionally by the test before it.
    #[test]
    fn sharing_the_input_narrows_the_error_bar() {
        if !quiesced() {
            println!("SKIPPED: machine is not quiesced (see `quiet-bench reserve`)");
            return;
        }
        let cfg = Config::relative(0.02).with_max_time(Duration::from_millis(300));
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
        let results = cfg
            .input_group_make_input(move || {
                let n = 100 + (rng.next() % 8000) as usize;
                ErasedInput::new((0..n as u64).collect::<Vec<u64>>())
            })
            .add_input("a", |e: &mut ErasedInput| sum(e.get_mut::<Vec<u64>>()))
            .add_input("b", |e: &mut ErasedInput| sum(e.get_mut::<Vec<u64>>()))
            .run();
        let (_, c) = results.against_baseline().next().expect("one alternative");
        let combined = (c.candidate.std_error.powi(2) + c.baseline.std_error.powi(2)).sqrt();
        let paired = c.std_error();
        println!("combined={combined:.4} paired={paired:.4}");
        assert!(
            paired < combined / 2.0,
            "paired {paired:.4} against combined {combined:.4}",
        );
    }
}

#[cfg(test)]
pub(crate) mod lane_tests {
    use super::*;
    use crate::registry::{noop_alt, ErasedInput};
    use std::any::TypeId;

    /// Leaks a one-element group list, for the (common) single-group test
    /// helpers below.
    pub(crate) fn one_group(g: &'static str) -> &'static [&'static str] {
        Box::leak(vec![g].into_boxed_slice())
    }

    pub(crate) fn cand_in<I: 'static>(
        groups: &'static [&'static str],
        name: &'static str,
        ty: &'static str,
        is_baseline: bool,
    ) -> Candidate {
        cand_in_from::<I>(groups, name, ty, is_baseline, "testcrate", "1.0.0")
    }

    pub(crate) fn cand_in_from<I: 'static>(
        groups: &'static [&'static str],
        name: &'static str,
        ty: &'static str,
        is_baseline: bool,
        crate_name: &'static str,
        crate_version: &'static str,
    ) -> Candidate {
        Candidate {
            groups,
            name,
            input_type: TypeId::of::<I>,
            input_type_name: ty,
            is_baseline,
            crate_name,
            crate_version,
            add_alt: noop_alt,
        }
    }

    pub(crate) fn cand<I: 'static>(
        group: &'static str,
        name: &'static str,
        ty: &'static str,
        is_baseline: bool,
    ) -> Candidate {
        cand_in::<I>(one_group(group), name, ty, is_baseline)
    }

    pub(crate) fn cand_from<I: 'static>(
        group: &'static str,
        name: &'static str,
        ty: &'static str,
        is_baseline: bool,
        crate_name: &'static str,
        crate_version: &'static str,
    ) -> Candidate {
        cand_in_from::<I>(
            one_group(group),
            name,
            ty,
            is_baseline,
            crate_name,
            crate_version,
        )
    }

    pub(crate) fn inp_in<I: 'static>(
        groups: &'static [&'static str],
        name: &'static str,
        ty: &'static str,
    ) -> Input {
        inp_in_from::<I>(groups, name, ty, "testcrate", "1.0.0")
    }

    pub(crate) fn inp_in_from<I: 'static>(
        groups: &'static [&'static str],
        name: &'static str,
        ty: &'static str,
        crate_name: &'static str,
        crate_version: &'static str,
    ) -> Input {
        Input {
            groups,
            name,
            crate_name,
            crate_version,
            type_id: TypeId::of::<I>,
            type_name: ty,
            make: || ErasedInput::new(()),
        }
    }

    pub(crate) fn inp<I: 'static>(
        group: &'static str,
        name: &'static str,
        ty: &'static str,
    ) -> Input {
        inp_in::<I>(one_group(group), name, ty)
    }

    pub(crate) fn inp_from<I: 'static>(
        group: &'static str,
        name: &'static str,
        ty: &'static str,
        crate_name: &'static str,
        crate_version: &'static str,
    ) -> Input {
        inp_in_from::<I>(one_group(group), name, ty, crate_name, crate_version)
    }

    pub(crate) fn leak_c(v: Vec<Candidate>) -> Vec<&'static Candidate> {
        v.into_iter().map(|x| &*Box::leak(Box::new(x))).collect()
    }
    pub(crate) fn leak_i(v: Vec<Input>) -> Vec<&'static Input> {
        v.into_iter().map(|x| &*Box::leak(Box::new(x))).collect()
    }

    /// Test-only convenience matching the old, single-call `lanes()`'s
    /// shape: builds a full plan from candidates and inputs alone, with no
    /// flat benchmarks, and hands back just the lanes and diagnostics.
    pub(crate) fn lanes(
        candidates: &[&'static Candidate],
        inputs: &[&'static Input],
    ) -> (Vec<Lane>, Vec<Diagnostic>) {
        let (p, problems) = plan(&[], candidates, inputs);
        (p.lanes, problems)
    }

    /// The cross-product forms from declarations that never mention each
    /// other, which is the whole point.
    #[test]
    fn candidates_and_inputs_pair_up_by_type() {
        let cs = leak_c(vec![
            cand::<Vec<u8>>("m", "b", "Vec<u8>", false),
            cand::<Vec<u8>>("m", "a", "Vec<u8>", true),
        ]);
        let is = leak_i(vec![
            inp::<Vec<u8>>("m", "big", "Vec<u8>"),
            inp::<Vec<u8>>("m", "small", "Vec<u8>"),
        ]);
        let (lanes, problems) = lanes(&cs, &is);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].candidates.len(), 2);
        assert_eq!(lanes[0].inputs.len(), 2);
        // Baseline first, then by name; inputs by name.
        assert_eq!(
            lanes[0]
                .candidates
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"],
        );
        assert_eq!(
            lanes[0]
                .inputs
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>(),
            ["big", "small"],
        );
    }

    /// One matrix, two unrelated types: two lanes, and nothing crosses.
    ///
    /// This is what makes a heterogeneous matrix work rather than being a
    /// type error - a `String` candidate is simply never offered a `Vec<u8>`.
    #[test]
    fn a_matrix_of_two_types_becomes_two_lanes() {
        let cs = leak_c(vec![
            cand::<Vec<u8>>("m", "bytes_a", "Vec<u8>", true),
            cand::<Vec<u8>>("m", "bytes_b", "Vec<u8>", false),
            cand::<String>("m", "text_a", "String", true),
            cand::<String>("m", "text_b", "String", false),
        ]);
        let is = leak_i(vec![
            inp::<Vec<u8>>("m", "buf", "Vec<u8>"),
            inp::<String>("m", "words", "String"),
        ]);
        let (lanes, problems) = lanes(&cs, &is);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(lanes.len(), 2);
        for lane in &lanes {
            let ty = lane.type_name;
            assert!(
                lane.candidates.iter().all(|c| c.reg.input_type_name == ty),
                "a lane holds one type only",
            );
            assert!(lane.inputs.iter().all(|i| i.reg.type_name == ty));
        }
    }

    /// Two lanes of one matrix with an input of the same name -
    /// `comparison_name` must tell their cells apart, or the second lane's
    /// result silently overwrites the first's under one shared report
    /// entry. Only these two lanes disambiguate: a third, unrelated lane in
    /// the same matrix keeps its plain name.
    #[test]
    fn two_lanes_sharing_an_input_name_are_told_apart() {
        let cs = leak_c(vec![
            cand::<Vec<u8>>("m", "bytes_a", "Vec<u8>", true),
            cand::<Vec<u8>>("m", "bytes_b", "Vec<u8>", false),
            cand::<String>("m", "text_a", "String", true),
            cand::<String>("m", "text_b", "String", false),
            cand::<u8>("m", "byte_a", "u8", true),
            cand::<u8>("m", "byte_b", "u8", false),
        ]);
        let is = leak_i(vec![
            // Both lanes register an input called "small" - the collision.
            inp::<Vec<u8>>("m", "small", "Vec<u8>"),
            inp::<String>("m", "small", "String"),
            // Unrelated third lane, name shared with nothing.
            inp::<u8>("m", "one", "u8"),
        ]);
        let (lanes, problems) = lanes(&cs, &is);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(lanes.len(), 3);

        let names: Vec<String> = lanes
            .iter()
            .flat_map(|lane| lane.inputs.iter().map(|i| lane.comparison_name(i)))
            .collect();
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            names.len(),
            unique.len(),
            "every cell name must be unique: {names:?}",
        );
        assert!(names.iter().any(|n| n == "m@small (Vec<u8>)"), "{names:?}",);
        assert!(names.iter().any(|n| n == "m@small (String)"), "{names:?}");
        assert!(
            names.iter().any(|n| n == "m@one"),
            "the uncontested lane should not be disambiguated: {names:?}",
        );
    }

    /// With nobody marked, the first by name is the baseline - a matrix
    /// should be writable without ceremony.
    #[test]
    fn an_unmarked_lane_takes_the_first_name_as_baseline() {
        let cs = leak_c(vec![
            cand::<u8>("m", "zulu", "u8", false),
            cand::<u8>("m", "alpha", "u8", false),
        ]);
        let is = leak_i(vec![inp::<u8>("m", "i", "u8")]);
        let (lanes, problems) = lanes(&cs, &is);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(lanes[0].candidates[0].name.as_str(), "alpha");
    }

    /// And a candidate sorting earlier displaces it, which is the documented
    /// cost of not marking one. Asserted deliberately so the behaviour is
    /// pinned rather than discovered.
    #[test]
    fn adding_an_earlier_name_moves_an_unmarked_baseline() {
        let is = leak_i(vec![inp::<u8>("m", "i", "u8")]);
        let before = leak_c(vec![
            cand::<u8>("m", "bravo", "u8", false),
            cand::<u8>("m", "charlie", "u8", false),
        ]);
        assert_eq!(lanes(&before, &is).0[0].candidates[0].name, "bravo");

        let after = leak_c(vec![
            cand::<u8>("m", "bravo", "u8", false),
            cand::<u8>("m", "charlie", "u8", false),
            cand::<u8>("m", "alpha", "u8", false),
        ]);
        assert_eq!(
            lanes(&after, &is).0[0].candidates[0].name,
            "alpha",
            "an unmarked baseline is whichever name sorts first, so adding one \
             ahead of it re-bases every reported difference",
        );
    }

    /// An explicit mark beats the alphabet.
    #[test]
    fn a_marked_baseline_beats_the_alphabet() {
        let cs = leak_c(vec![
            cand::<u8>("m", "alpha", "u8", false),
            cand::<u8>("m", "zulu", "u8", true),
        ]);
        let is = leak_i(vec![inp::<u8>("m", "i", "u8")]);
        assert_eq!(lanes(&cs, &is).0[0].candidates[0].name, "zulu");
    }

    /// Two marked is a contradiction, unlike none.
    #[test]
    fn two_marked_baselines_are_rejected() {
        let cs = leak_c(vec![
            cand::<u8>("m", "a", "u8", true),
            cand::<u8>("m", "b", "u8", true),
        ]);
        let is = leak_i(vec![inp::<u8>("m", "i", "u8")]);
        let (lanes, problems) = lanes(&cs, &is);
        assert!(lanes.is_empty(), "the lane is skipped");
        assert!(
            matches!(&problems[0], Diagnostic::ManyBaselines { .. }),
            "{problems:?}",
        );
    }

    /// A candidate no input matches is almost always a typo, but it is not a
    /// contradiction - so it is said out loud and skipped, leaving the rest
    /// of the run to happen.
    #[test]
    fn a_candidate_with_no_matching_input_warns() {
        let cs = leak_c(vec![cand::<String>("m", "lonely", "String", true)]);
        let is = leak_i(vec![inp::<u8>("m", "i", "u8")]);
        let (lanes, problems) = lanes(&cs, &is);
        assert!(lanes.is_empty());
        assert!(
            problems
                .iter()
                .any(|p| matches!(p, Diagnostic::OrphanCandidate { name, .. } if name == "lonely")),
            "{problems:?}",
        );
        assert!(
            problems
                .iter()
                .any(|p| matches!(p, Diagnostic::OrphanInput { name, .. } if name == "i")),
            "{problems:?}",
        );
    }

    /// A lone candidate has no difference to report. It is still measured
    /// as a singleton input group rather than being dropped.
    #[test]
    fn a_lane_with_one_candidate_is_kept_for_plain_measurement() {
        let cs = leak_c(vec![cand::<u8>("m", "only", "u8", false)]);
        let is = leak_i(vec![inp::<u8>("m", "i", "u8")]);
        let (lanes, problems) = lanes(&cs, &is);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].candidates.len(), 1);
        assert_eq!(
            lanes[0].flat_name(&lanes[0].candidates[0], &lanes[0].inputs[0]),
            "m::only@i"
        );
    }

    #[test]
    fn duplicate_names_within_a_matrix_are_rejected() {
        let cs = leak_c(vec![
            cand::<u8>("m", "same", "u8", true),
            cand::<u8>("m", "same", "u8", false),
        ]);
        let is = leak_i(vec![inp::<u8>("m", "i", "u8")]);
        let (_, problems) = lanes(&cs, &is);
        assert!(
            problems.iter().any(|p| matches!(
                p,
                Diagnostic::DuplicateMatrixEntry { what, name, .. }
                    if *what == "candidate" && name == "same"
            )),
            "{problems:?}",
        );
    }

    /// Two types spelled alike in different modules are still two types, and
    /// pairing them would be a downcast panic later.
    #[test]
    fn a_name_collision_between_two_real_types_is_caught() {
        // Same spelling, different actual type.
        let cs = leak_c(vec![
            cand::<u8>("m", "a", "Thing", true),
            cand::<u16>("m", "b", "Thing", false),
        ]);
        let is = leak_i(vec![inp::<u8>("m", "i", "Thing")]);
        let (lanes, problems) = lanes(&cs, &is);
        assert!(lanes.is_empty(), "the lane cannot be trusted");
        assert!(
            problems.iter().any(
                |p| matches!(p, Diagnostic::MatrixTypeMismatch { candidate, .. } if candidate == "b")
            ),
            "{problems:?}",
        );
    }

    /// Lanes come out sorted, for the reason everything else does.
    #[test]
    fn lanes_come_out_sorted() {
        let cs = leak_c(vec![
            cand::<u8>("zebra", "a", "u8", true),
            cand::<u8>("zebra", "b", "u8", false),
            cand::<u8>("apple", "a", "u8", true),
            cand::<u8>("apple", "b", "u8", false),
        ]);
        let is = leak_i(vec![
            inp::<u8>("zebra", "i", "u8"),
            inp::<u8>("apple", "i", "u8"),
        ]);
        let (lanes, _) = lanes(&cs, &is);
        assert_eq!(
            lanes.iter().map(|l| l.group).collect::<Vec<_>>(),
            ["apple", "zebra"],
        );
    }
}

#[cfg(test)]
mod version_tests {
    use super::lane_tests::*;
    use super::*;

    /// Versions order numerically, not as text.
    ///
    /// The string comparison is not merely different, it is backwards, and
    /// silently so: a policy picking the newest would take 0.9.0 over 0.10.0
    /// and nothing about the output would look wrong.
    #[test]
    fn versions_order_numerically() {
        assert!(Version::parse("0.10.0") > Version::parse("0.9.0"));
        assert!(
            "0.10.0" < "0.9.0",
            "which is why the string form is unusable"
        );
        assert!(Version::parse("1.0.0") > Version::parse("0.99.99"));
        assert!(Version::parse("1.2.10") > Version::parse("1.2.9"));
    }

    /// A pre-release or build suffix does not stop a version being ordered.
    /// It is dropped rather than being made to mean something.
    #[test]
    fn suffixes_are_ignored_rather_than_fatal() {
        assert_eq!(Version::parse("1.2.3-rc.1"), Version::parse("1.2.3"));
        assert_eq!(Version::parse("1.2.3+build7"), Version::parse("1.2.3"));
        // And nonsense parses as zeroes rather than panicking: a version
        // that cannot be read should not stop a benchmark run.
        assert_eq!(Version::parse("not-a-version"), Version::parse("0.0.0"));
        assert_eq!(Version::parse(""), Version::parse("0.0.0"));
    }

    /// Two versions of one implementation are the *point* of a cross-version
    /// comparison, so both are kept and told apart by version.
    #[test]
    fn two_versions_of_one_name_are_both_kept_and_distinguished() {
        let cs = leak_c(vec![
            cand_from::<u8>("m", "sort", "u8", true, "mycrate", "0.9.0"),
            cand_from::<u8>("m", "sort", "u8", true, "mycrate", "0.8.0"),
        ]);
        let is = leak_i(vec![inp::<u8>("m", "i", "u8")]);
        let (lanes, problems) = lanes(&cs, &is);
        assert!(problems.is_empty(), "{problems:?}");
        let names: Vec<&str> = lanes[0]
            .candidates
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert!(names.contains(&"sort@0.8.0"), "{names:?}");
        assert!(names.contains(&"sort@0.9.0"), "{names:?}");
    }

    /// And the older one is the baseline by default, so a regression reads
    /// the right way round: the new code is measured *against* the old.
    #[test]
    fn the_older_version_is_the_baseline_by_default() {
        let cs = leak_c(vec![
            cand_from::<u8>("m", "sort", "u8", true, "mycrate", "0.9.0"),
            cand_from::<u8>("m", "sort", "u8", true, "mycrate", "0.8.0"),
        ]);
        let is = leak_i(vec![inp::<u8>("m", "i", "u8")]);
        let (lanes, _) = lanes(&cs, &is);
        assert_eq!(
            lanes[0].candidates[0].name, "sort@0.8.0",
            "the old version is what the new one is compared against",
        );
    }

    /// Redundant inputs are dropped rather than measured twice.
    ///
    /// Two versions of one implementation are worth comparing; two versions
    /// of one input generator are meant to build the same data, so measuring
    /// on both doubles the work to no end - and an old generator paired with
    /// new implementations quietly changes what is being measured, if the
    /// generator itself has changed since.
    #[test]
    fn a_redundant_input_from_an_old_version_is_dropped() {
        let cs = leak_c(vec![
            cand_from::<u8>("m", "a", "u8", true, "mycrate", "0.9.0"),
            cand_from::<u8>("m", "b", "u8", false, "mycrate", "0.9.0"),
        ]);
        let is = leak_i(vec![
            inp_from::<u8>("m", "data", "u8", "mycrate", "0.9.0"),
            inp_from::<u8>("m", "data", "u8", "mycrate", "0.8.0"),
        ]);
        let (lanes, problems) = lanes(&cs, &is);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(lanes[0].inputs.len(), 1, "one input, not one per version");
        assert_eq!(
            lanes[0].inputs[0].origin.crate_version, "0.9.0",
            "and it is the newest, not whichever happened to register first",
        );
    }

    /// Two registrations of one input, same crate, same version, are not
    /// redundancy - nothing distinguishes them, so picking one silently
    /// would hide a real mistake rather than resolve an intended one.
    #[test]
    fn a_genuine_duplicate_input_is_reported_not_silently_kept() {
        let cs = leak_c(vec![cand::<u8>("m", "a", "u8", true)]);
        let is = leak_i(vec![
            inp_from::<u8>("m", "data", "u8", "mycrate", "0.9.0"),
            inp_from::<u8>("m", "data", "u8", "mycrate", "0.9.0"),
        ]);
        let (_lanes, problems) = lanes(&cs, &is);
        assert!(
            problems.iter().any(|p| matches!(
                p,
                Diagnostic::DuplicateMatrixEntry { what, name, .. }
                    if *what == "input" && name == "data"
            )),
            "{problems:?}",
        );
    }

    /// With versions kept, a name has to carry both crate and version when
    /// both differ.
    #[test]
    fn names_carry_only_what_distinguishes_them() {
        let cs = leak_c(vec![
            cand_from::<u8>("m", "sort", "u8", true, "mine", "2.0.0"),
            cand_from::<u8>("m", "sort", "u8", false, "mine", "1.0.0"),
            cand_from::<u8>("m", "sort", "u8", false, "theirs", "0.3.0"),
        ]);
        let is = leak_i(vec![inp::<u8>("m", "i", "u8")]);
        let (lanes, _) = lanes(&cs, &is);
        let names: Vec<&str> = lanes[0]
            .candidates
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert!(names.contains(&"sort@mine-2.0.0"), "{names:?}");
        assert!(names.contains(&"sort@mine-1.0.0"), "{names:?}");
        assert!(names.contains(&"sort@theirs-0.3.0"), "{names:?}");
    }

    /// Two different functions claiming the baseline within one version is a
    /// contradiction, not a version spread, and must not be quietly resolved.
    #[test]
    fn two_functions_claiming_baseline_in_one_version_is_still_an_error() {
        let cs = leak_c(vec![
            cand_from::<u8>("m", "a", "u8", true, "mycrate", "1.0.0"),
            cand_from::<u8>("m", "b", "u8", true, "mycrate", "1.0.0"),
        ]);
        let is = leak_i(vec![inp::<u8>("m", "i", "u8")]);
        let (lanes, problems) = lanes(&cs, &is);
        assert!(lanes.is_empty(), "the lane cannot be trusted");
        assert!(
            matches!(&problems[0], Diagnostic::ManyBaselines { .. }),
            "{problems:?}",
        );
    }

    /// One name in two different matrices is two unrelated things, not two
    /// versions of one - so neither gets renamed or dropped.
    #[test]
    fn the_same_name_in_two_matrices_stays_two_things() {
        let cs = leak_c(vec![
            cand::<u8>("apple", "sort", "u8", true),
            cand::<u8>("apple", "other", "u8", false),
            cand::<u8>("zebra", "sort", "u8", true),
            cand::<u8>("zebra", "other", "u8", false),
        ]);
        let is = leak_i(vec![
            inp::<u8>("apple", "i", "u8"),
            inp::<u8>("zebra", "i", "u8"),
        ]);
        let (lanes, problems) = lanes(&cs, &is);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(lanes.len(), 2, "both matrices survive");
        for lane in &lanes {
            let names: Vec<&str> = lane.candidates.iter().map(|c| c.name.as_str()).collect();
            assert!(
                names.contains(&"sort"),
                "unrenamed in {}: {names:?}",
                lane.group
            );
        }
    }
}

/// Regressions for the five faults a review of this work turned up. Each
/// arose where two axes met - a type varying *and* a version varying - which
/// is why none of the tests written a stage at a time had caught them.
#[cfg(test)]
mod review_regressions {
    use super::lane_tests::*;
    use super::*;
    use crate::registry::Kind;

    /// `types(A, B)` makes several registrations of one name that differ in
    /// type. They are instantiations, not versions of each other, so
    /// narrowing to the latest of each crate - the policy `resolve_versions`
    /// always applies to inputs - must not drop all but one.
    ///
    /// It did: the whole `Vec<u8>` half of a generic matrix disappeared and
    /// its input was reported as an orphan.
    #[test]
    fn generic_instantiations_survive_latest_per_crate() {
        let cs = leak_c(vec![
            cand::<String>("m", "byte_sum", "String", true),
            cand::<Vec<u8>>("m", "byte_sum", "Vec<u8>", true),
        ]);
        let named = resolve_versions(
            &cs,
            |c| {
                (
                    Key {
                        scope: "m",
                        type_name: c.input_type_name,
                        name: c.name,
                    },
                    Origin {
                        crate_name: c.crate_name,
                        crate_version: c.crate_version,
                    },
                )
            },
            true,
        );
        assert_eq!(named.len(), 2, "both instantiations must survive");
    }

    /// And under the default policy they are not renamed either: one crate
    /// at one version has nothing to disambiguate against.
    #[test]
    fn generic_instantiations_keep_their_plain_names() {
        let cs = leak_c(vec![
            cand::<String>("m", "byte_sum", "String", true),
            cand::<Vec<u8>>("m", "byte_sum", "Vec<u8>", true),
        ]);
        let is = leak_i(vec![
            inp::<String>("m", "text", "String"),
            inp::<Vec<u8>>("m", "bytes", "Vec<u8>"),
        ]);
        let (lanes, _) = lanes(&cs, &is);
        for lane in &lanes {
            assert_eq!(
                lane.candidates[0].name, "byte_sum",
                "nothing needed distinguishing, so nothing should be appended",
            );
        }
    }

    /// Inputs get named for their character - "small", "large" - so one
    /// matrix holding two type lanes very naturally has an input called
    /// `small` in each. They are not redundant copies of one another.
    ///
    /// Deduplicating on the name alone dropped one and orphaned a whole lane.
    #[test]
    fn same_input_name_in_two_lanes_is_two_inputs() {
        let cs = leak_c(vec![
            cand::<String>("m", "text_a", "String", true),
            cand::<String>("m", "text_b", "String", false),
            cand::<Vec<u8>>("m", "bytes_a", "Vec<u8>", true),
            cand::<Vec<u8>>("m", "bytes_b", "Vec<u8>", false),
        ]);
        let is = leak_i(vec![
            inp::<String>("m", "small", "String"),
            inp::<Vec<u8>>("m", "small", "Vec<u8>"),
        ]);
        let (lanes, problems) = lanes(&cs, &is);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(lanes.len(), 2, "neither lane may be orphaned");
        for lane in &lanes {
            assert_eq!(lane.inputs.len(), 1);
            assert_eq!(lane.inputs[0].name, "small");
            assert_eq!(lane.candidates.len(), 2);
        }
    }

    /// A lane is bucketed on the type's *name*, so two inputs spelling their
    /// types alike land in one lane whatever they really are. Checking only
    /// the candidates against the first input left the second free to be
    /// handed to a candidate expecting something else - a downcast panic
    /// from inside the scheduler, which is what this check exists to
    /// forestall.
    #[test]
    fn inputs_are_checked_against_each_other_too() {
        let cs = leak_c(vec![
            cand::<u8>("m", "a", "Thing", true),
            cand::<u8>("m", "b", "Thing", false),
        ]);
        let is = leak_i(vec![
            inp::<u8>("m", "first", "Thing"),
            inp::<u16>("m", "second", "Thing"),
        ]);
        let (lanes, problems) = lanes(&cs, &is);
        assert!(lanes.is_empty(), "the lane cannot be trusted");
        assert!(
            problems.iter().any(
                |p| matches!(p, Diagnostic::MatrixTypeMismatch { candidate, .. }
                                  if candidate == "second")
            ),
            "{problems:?}",
        );
    }

    /// A contradiction inside a lane discards that lane, so it has to be
    /// fatal. Reported only as a warning it meant a caller who wrote
    /// `suite.try_add_registered().unwrap();` and dropped the result
    /// saw a run that silently measured nothing.
    #[test]
    fn a_lane_contradiction_is_fatal_but_an_orphan_is_not() {
        let two_baselines = Diagnostic::ManyBaselines {
            group: "m".into(),
            claimants: vec![],
        };
        assert!(two_baselines.is_fatal());
        let orphan = Diagnostic::OrphanCandidate {
            matrix: "m".into(),
            name: "x".into(),
            type_name: "u8",
        };
        assert!(
            !orphan.is_fatal(),
            "an unused registration leaves the rest of the run perfectly good",
        );
    }

    fn flat_at(name: &'static str, version: &'static str) -> Registered {
        Registered {
            name,
            crate_name: "mycrate",
            crate_version: version,
            kind: Kind::Flat(|a, n| {
                a.add(n, || ());
            }),
        }
    }

    fn leak_r(rs: Vec<Registered>) -> Vec<&'static Registered> {
        rs.into_iter().map(|r| &*Box::leak(Box::new(r))).collect()
    }

    /// The cross-version support has to reach plain benchmarks too, not only
    /// matrices - `#[scaling::bench]` registered by an old copy of a crate
    /// used to collide with itself and refuse the whole run.
    #[test]
    fn a_plain_benchmark_can_come_from_two_versions() {
        let regs = leak_r(vec![flat_at("fib", "0.9.0"), flat_at("fib", "0.8.0")]);
        let (plan, problems) = plan(&regs, &[], &[]);
        assert!(
            problems.is_empty(),
            "two versions of one benchmark is not a duplicate: {problems:?}",
        );
        let names: Vec<&str> = plan.flat.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"fib@0.8.0"), "{names:?}");
        assert!(names.contains(&"fib@0.9.0"), "{names:?}");
    }

    /// And to comparison groups, where every version of a member marked
    /// `baseline` says so - which is what the policy is for.
    #[test]
    fn a_group_can_span_two_versions() {
        let cs = leak_c(vec![
            cand_from::<()>("g", "sort", "()", true, "mycrate", "0.9.0"),
            cand_from::<()>("g", "sort", "()", true, "mycrate", "0.8.0"),
        ]);
        let (plan, problems) = plan(&[], &cs, &[]);
        assert!(
            problems.is_empty(),
            "a group spanning two versions is the point: {problems:?}",
        );
        assert_eq!(plan.lanes.len(), 1);
        assert_eq!(
            plan.lanes[0].candidates[0].name, "sort@0.8.0",
            "the older version is the baseline by default",
        );
    }

    /// Two different members claiming the baseline within one version stays
    /// a contradiction, though - it is not a version spread, and resolving
    /// it by version would pick one and hide the mistake.
    #[test]
    fn two_members_claiming_baseline_in_one_version_is_still_an_error() {
        let cs = leak_c(vec![
            cand_from::<()>("g", "a", "()", true, "mycrate", "1.0.0"),
            cand_from::<()>("g", "b", "()", true, "mycrate", "1.0.0"),
        ]);
        let (_, problems) = plan(&[], &cs, &[]);
        assert!(
            problems
                .iter()
                .any(|p| matches!(p, Diagnostic::ManyBaselines { .. })),
            "{problems:?}",
        );
    }
}
