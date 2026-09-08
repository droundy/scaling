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

use crate::registry::{
    ErasedInput, GenInputRegistration, Kind, MatrixCandidate, MatrixInput, Registered,
};
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

/// Which versions of one function to benchmark, when several are registered.
///
/// This arises when a crate pulls in an older copy of itself, or a rival
/// crate, as a dev-dependency with registrations enabled: both register, and
/// both may use the same name for the same idea.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VersionPolicy {
    /// Benchmark every version, telling them apart by where they came from.
    ///
    /// The default, because it is what comparing against an older version is
    /// *for*, and because it discards nothing.
    #[default]
    All,
    /// Benchmark only the newest version **of each crate**.
    ///
    /// Per crate, not overall: when the point is to measure against other
    /// crates, dropping a rival's implementation because your own version
    /// number happens to be higher would be exactly wrong. So this keeps the
    /// newest of yours and the newest of each of theirs.
    LatestPerCrate,
}

/// Which of several claimants is a comparison's baseline.
///
/// Only consulted when more than one says it is. One version of a function
/// that calls itself the baseline stays the baseline however many other
/// versions are registered alongside it - they inherit the claim, because
/// they are the same source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BaselinePolicy {
    /// The oldest claimant.
    ///
    /// The default, and the one that makes a regression read the right way
    /// round: "is the new code faster than the old code" wants the old code
    /// as the thing being measured against. Picking the newest would report
    /// every older version as a change *from* the code being written, which
    /// is backwards.
    #[default]
    Oldest,
    /// The newest claimant.
    Newest,
    /// A named crate at a named version; an error if it is not among them.
    Exact {
        crate_name: &'static str,
        crate_version: &'static str,
    },
}

/// How to treat registrations that come from more than one crate or version.
#[derive(Debug, Clone, Copy, Default)]
pub struct RegistryOptions {
    pub versions: VersionPolicy,
    pub baseline: BaselinePolicy,
}

impl RegistryOptions {
    /// Benchmark only the newest version of each crate. See
    /// [`VersionPolicy::LatestPerCrate`].
    pub fn latest_per_crate() -> Self {
        RegistryOptions {
            versions: VersionPolicy::LatestPerCrate,
            ..Default::default()
        }
    }

    /// Choose the baseline differently when several claim it.
    pub fn with_baseline(mut self, baseline: BaselinePolicy) -> Self {
        self.baseline = baseline;
        self
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
/// Returns the survivors in the input's order; callers sort afterwards.
pub(crate) fn resolve_versions<T: 'static>(
    items: &[&'static T],
    origin_of: impl Fn(&'static T) -> (&'static str, &'static str, Origin),
    policy: VersionPolicy,
) -> Vec<Named<T>> {
    // Group by scope *and* name. Two candidates called `sort` in different
    // matrices are unrelated; only ones sharing a scope are versions or
    // rivals of the same idea. Grouping on the name alone quietly merged
    // them, which cost a whole matrix.
    let mut by_name: BTreeMap<(&'static str, &'static str), Vec<(&'static T, Origin)>> =
        BTreeMap::new();
    for it in items {
        let (scope, name, origin) = origin_of(it);
        by_name
            .entry((scope, name))
            .or_default()
            .push((*it, origin));
    }

    let mut out = Vec::new();
    for ((_scope, name), mut sharers) in by_name {
        if sharers.len() == 1 {
            let (reg, origin) = sharers.pop().expect("just checked");
            out.push(Named {
                name: name.to_string(),
                reg,
                origin,
            });
            continue;
        }

        if policy == VersionPolicy::LatestPerCrate {
            // Newest of each crate, so rivals all survive and only a crate's
            // own older copies are dropped.
            let mut newest: BTreeMap<&'static str, (&'static T, Origin)> = BTreeMap::new();
            for (reg, origin) in sharers {
                newest
                    .entry(origin.crate_name)
                    .and_modify(|held| {
                        if origin.version() > held.1.version() {
                            *held = (reg, origin);
                        }
                    })
                    .or_insert((reg, origin));
            }
            sharers = newest.into_values().collect();
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
    /// A [`BaselinePolicy::Exact`] naming something that is not among the
    /// claimants.
    NoSuchBaseline {
        group: String,
        wanted: String,
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
    /// Two types spelled alike in one matrix lane are not the same type.
    MatrixTypeMismatch {
        matrix: String,
        candidate: String,
        type_name: &'static str,
    },
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
            Diagnostic::NoSuchBaseline {
                group,
                wanted,
                claimants,
            } => write!(
                f,
                "`{group}` was told to use {wanted} as its baseline, but the ones \
                 claiming to be it are {}",
                list(claimants),
            ),
            Diagnostic::OrphanCandidate {
                matrix,
                name,
                type_name,
            } => write!(
                f,
                "in matrix `{matrix}`, `{name}` takes `{type_name}` but no input of \
                 that type is registered, so it was measured on nothing",
            ),
            Diagnostic::OrphanInput {
                matrix,
                name,
                type_name,
            } => write!(
                f,
                "in matrix `{matrix}`, the input `{name}` produces `{type_name}` but no \
                 candidate takes that type, so nothing was measured on it",
            ),
            Diagnostic::DuplicateMatrixEntry { matrix, what, name } => {
                write!(f, "matrix `{matrix}` has two {what}s called `{name}`",)
            }
            Diagnostic::MatrixTypeMismatch {
                matrix,
                candidate,
                type_name,
            } => write!(
                f,
                "in matrix `{matrix}`, `{candidate}` takes a different `{type_name}` from \
                 the one the inputs produce - two types of the same name are still two types",
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
    pub fn make_input(&self) -> crate::registry::MakeInput {
        match self.gen_input {
            Some(g) => g.make,
            None => || ErasedInput::new(()),
        }
    }
}

/// One matrix's candidates and inputs of a single type, paired up.
///
/// A matrix partitions into lanes rather than being one grid, because
/// candidates and inputs are registered independently and need not all agree
/// about the type. Pairing within a lane is what lets one matrix hold
/// several unrelated type families and still be correct - a `String`
/// candidate is simply never handed a `Vec<u8>`.
#[derive(Debug)]
pub struct Lane {
    /// The matrix this lane belongs to.
    pub matrix: &'static str,
    /// The input type shared by everything in it, as the source spells it.
    pub type_name: &'static str,
    /// Candidates, baseline first, then sorted by name - the same ordering,
    /// and for the same reason, as a comparison group's members.
    ///
    /// Named rather than bare registrations, because when several crates or
    /// versions register one name they have to be told apart, and the name
    /// that distinguishes them is worked out from what else is present.
    pub candidates: Vec<Named<MatrixCandidate>>,
    /// Inputs, sorted by name, one per name.
    ///
    /// Unlike candidates, inputs are *deduplicated* across versions rather
    /// than disambiguated. Two versions of one implementation are the point
    /// of a cross-version comparison; two versions of one input generator
    /// are meant to build the same data, so measuring on both would double
    /// the work to no end - and worse, an old generator paired with new
    /// implementations quietly changes what is being measured if the
    /// generator itself has changed since.
    pub inputs: Vec<Named<MatrixInput>>,
}

impl Lane {
    /// What a cell of this lane is called.
    ///
    /// A lane with two or more candidates becomes one comparison per input,
    /// so the comparison is named for the matrix and the input and the
    /// candidates are its alternatives. A lone candidate has nothing to
    /// compare against and is a plain benchmark, which needs its own name.
    pub fn comparison_name(&self, input: &Named<MatrixInput>) -> String {
        format!("{}@{}", self.matrix, input.name)
    }

    pub fn flat_name(
        &self,
        candidate: &Named<MatrixCandidate>,
        input: &Named<MatrixInput>,
    ) -> String {
        format!("{}::{}@{}", self.matrix, candidate.name, input.name)
    }
}

/// Partition a matrix's registrations into lanes and pair them up.
///
/// Pure, like [`plan`], and for the same reason: everything that can be
/// wrong is decided before the machine is claimed.
///
/// # Orphans are warnings, not errors
///
/// A candidate whose lane has no inputs, or an input whose lane has no
/// candidates, is almost always a typo or a type that does not match what
/// the writer thought - but it is not a contradiction, and rejecting the
/// whole run over it would be unhelpful when the rest is fine. So orphans
/// are reported and skipped.
pub fn lanes(
    candidates: &[&'static MatrixCandidate],
    inputs: &[&'static MatrixInput],
    options: RegistryOptions,
) -> (Vec<Lane>, Vec<Diagnostic>) {
    let mut problems = Vec::new();

    // Decide what survives and what each is called, before anything else, so
    // that two versions of one implementation stop looking like a duplicate
    // and start looking like the comparison they are.
    let named_c = resolve_versions(
        candidates,
        |c| {
            (
                c.matrix,
                c.name,
                Origin {
                    crate_name: c.crate_name,
                    crate_version: c.crate_version,
                },
            )
        },
        options.versions,
    );
    // Inputs are deduplicated rather than disambiguated - see `Lane::inputs`
    // - so the policy for them is always to keep one per name.
    let named_i = resolve_versions(
        inputs,
        |i| {
            (
                i.matrix,
                i.name,
                Origin {
                    crate_name: i.crate_name,
                    crate_version: i.crate_version,
                },
            )
        },
        VersionPolicy::LatestPerCrate,
    );
    // `LatestPerCrate` leaves one per crate; an input registered by two
    // different crates is still redundant, so keep the newest of those too.
    let mut best: BTreeMap<(&'static str, &'static str), Named<MatrixInput>> = BTreeMap::new();
    for n in named_i {
        match best.get(&(n.reg.matrix, n.reg.name)) {
            Some(held)
                if Version::parse(held.origin.crate_version)
                    >= Version::parse(n.origin.crate_version) => {}
            _ => {
                best.insert((n.reg.matrix, n.reg.name), n);
            }
        }
    }
    let named_i: Vec<Named<MatrixInput>> = best.into_values().collect();

    // (matrix, input type) is the lane key. `TypeId` is not `Ord`, so bucket
    // by the type's name, which the macro takes from the source: two
    // different types cannot spell themselves the same way within one crate,
    // and the id is checked below in any case.
    let mut cands: BTreeMap<(&str, &str), Vec<Named<MatrixCandidate>>> = BTreeMap::new();
    let mut ins: BTreeMap<(&str, &str), Vec<Named<MatrixInput>>> = BTreeMap::new();
    for c in named_c {
        cands
            .entry((c.reg.matrix, c.reg.input_type_name))
            .or_default()
            .push(c);
    }
    for i in named_i {
        ins.entry((i.reg.matrix, i.reg.type_name))
            .or_default()
            .push(i);
    }

    // Names that are still shared after version resolution really are
    // duplicates - two registrations of one name from one crate at one
    // version - and would make two rows indistinguishable.
    for (key, cs) in &cands {
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
                    matrix: key.0.to_string(),
                    what: "candidate",
                    name: registered.to_string(),
                });
            }
        }
    }

    let mut lanes = Vec::new();
    let mut keys: Vec<&(&str, &str)> = cands.keys().chain(ins.keys()).collect();
    keys.sort_unstable();
    keys.dedup();
    let keys: Vec<(&str, &str)> = keys.into_iter().copied().collect();

    for key in keys {
        let mut cs = cands.remove(&key).unwrap_or_default();
        let mut is = ins.remove(&key).unwrap_or_default();

        if is.is_empty() {
            for c in &cs {
                problems.push(Diagnostic::OrphanCandidate {
                    matrix: key.0.to_string(),
                    name: c.name.clone(),
                    type_name: c.reg.input_type_name,
                });
            }
            continue;
        }
        if cs.is_empty() {
            for i in &is {
                problems.push(Diagnostic::OrphanInput {
                    matrix: key.0.to_string(),
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
        for c in &cs {
            if (c.reg.input_type)() != want {
                problems.push(Diagnostic::MatrixTypeMismatch {
                    matrix: key.0.to_string(),
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
                    group: key.0.to_string(),
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
            _ => match pick_baseline(&cs, &claimants, options.baseline) {
                Some(i) => cs[i].name.clone(),
                None => {
                    problems.push(Diagnostic::NoSuchBaseline {
                        group: key.0.to_string(),
                        wanted: match options.baseline {
                            BaselinePolicy::Exact {
                                crate_name,
                                crate_version,
                            } => format!("{crate_name}@{crate_version}"),
                            other => format!("{other:?}"),
                        },
                        claimants: cs
                            .iter()
                            .filter(|c| c.reg.is_baseline)
                            .map(|c| {
                                format!(
                                    "{}@{} ({})",
                                    c.origin.crate_name, c.origin.crate_version, c.name
                                )
                            })
                            .collect(),
                    });
                    continue;
                }
            },
        };
        cs.sort_by(|a, b| (a.name != baseline, &a.name).cmp(&(b.name != baseline, &b.name)));

        lanes.push(Lane {
            matrix: key.0,
            type_name: key.1,
            candidates: cs,
            inputs: is,
        });
    }

    (lanes, problems)
}

/// Which claimant the policy picks, or `None` if it names one that is not
/// there.
fn pick_baseline<T: 'static>(
    cs: &[Named<T>],
    claimants: &[usize],
    policy: BaselinePolicy,
) -> Option<usize> {
    match policy {
        BaselinePolicy::Oldest => claimants
            .iter()
            .copied()
            .min_by_key(|i| Version::parse(cs[*i].origin.crate_version)),
        BaselinePolicy::Newest => claimants
            .iter()
            .copied()
            .max_by_key(|i| Version::parse(cs[*i].origin.crate_version)),
        BaselinePolicy::Exact {
            crate_name,
            crate_version,
        } => claimants.iter().copied().find(|i| {
            cs[*i].origin.crate_name == crate_name && cs[*i].origin.crate_version == crate_version
        }),
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
            Some(g) => ((g.type_id)(), g.type_name),
            None => (std::any::TypeId::of::<()>(), "()"),
        };
        let mut mismatched = false;
        for m in &members {
            if let Kind::Alt { input_type, .. } = &m.kind {
                if input_type() != gen_type {
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
    use crate::{ComparisonSet, Config, Stats, Suite, Token};
    use std::any::TypeId;

    // Shims that do nothing. Assembly never calls them - it decides what
    // *would* be run - so a plan can be checked without a machine claim, a
    // benchmark, or a linker.
    fn noop_flat(suite: &mut Suite<'_>, _: &Config, name: &str) -> Token<Stats> {
        suite.add(name, || ())
    }
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
                input_type: TypeId::of::<I>,
                input_type_name: type_name,
            },
        }
    }

    fn generator<I: 'static>(group: &'static str, type_name: &'static str) -> GenInputRegistration {
        GenInputRegistration {
            group,
            type_id: TypeId::of::<I>,
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

#[cfg(test)]
pub(crate) mod lane_tests {
    use super::*;
    use crate::registry::{ErasedInput, MatrixCandidate, MatrixInput};
    use crate::{ComparisonSet, Config, Stats, Suite, Token};
    use std::any::TypeId;

    fn noop_flat(
        suite: &mut Suite<'_>,
        _: &Config,
        name: &str,
        make: fn() -> ErasedInput,
    ) -> Token<Stats> {
        suite.add_gen_input(name, make, |_: &mut ErasedInput| ())
    }
    fn noop_alt<'a>(
        set: ComparisonSet<'a, ErasedInput>,
        _: &str,
    ) -> ComparisonSet<'a, ErasedInput> {
        set
    }

    pub(crate) fn cand<I: 'static>(
        matrix: &'static str,
        name: &'static str,
        ty: &'static str,
        is_baseline: bool,
    ) -> MatrixCandidate {
        cand_from::<I>(matrix, name, ty, is_baseline, "testcrate", "1.0.0")
    }

    pub(crate) fn cand_from<I: 'static>(
        matrix: &'static str,
        name: &'static str,
        ty: &'static str,
        is_baseline: bool,
        crate_name: &'static str,
        crate_version: &'static str,
    ) -> MatrixCandidate {
        MatrixCandidate {
            matrix,
            name,
            input_type: TypeId::of::<I>,
            input_type_name: ty,
            is_baseline,
            crate_name,
            crate_version,
            add_flat: noop_flat,
            add_alt: noop_alt,
        }
    }

    pub(crate) fn inp<I: 'static>(
        matrix: &'static str,
        name: &'static str,
        ty: &'static str,
    ) -> MatrixInput {
        inp_from::<I>(matrix, name, ty, "testcrate", "1.0.0")
    }

    pub(crate) fn inp_from<I: 'static>(
        matrix: &'static str,
        name: &'static str,
        ty: &'static str,
        crate_name: &'static str,
        crate_version: &'static str,
    ) -> MatrixInput {
        MatrixInput {
            matrix,
            name,
            crate_name,
            crate_version,
            type_id: TypeId::of::<I>,
            type_name: ty,
            make: || ErasedInput::new(()),
        }
    }

    pub(crate) fn leak_c(v: Vec<MatrixCandidate>) -> Vec<&'static MatrixCandidate> {
        v.into_iter().map(|x| &*Box::leak(Box::new(x))).collect()
    }
    pub(crate) fn leak_i(v: Vec<MatrixInput>) -> Vec<&'static MatrixInput> {
        v.into_iter().map(|x| &*Box::leak(Box::new(x))).collect()
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
        let (lanes, problems) = lanes(&cs, &is, RegistryOptions::default());
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
        let (lanes, problems) = lanes(&cs, &is, RegistryOptions::default());
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

    /// With nobody marked, the first by name is the baseline - a matrix
    /// should be writable without ceremony.
    #[test]
    fn an_unmarked_lane_takes_the_first_name_as_baseline() {
        let cs = leak_c(vec![
            cand::<u8>("m", "zulu", "u8", false),
            cand::<u8>("m", "alpha", "u8", false),
        ]);
        let is = leak_i(vec![inp::<u8>("m", "i", "u8")]);
        let (lanes, problems) = lanes(&cs, &is, RegistryOptions::default());
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
        assert_eq!(
            lanes(&before, &is, RegistryOptions::default()).0[0].candidates[0].name,
            "bravo"
        );

        let after = leak_c(vec![
            cand::<u8>("m", "bravo", "u8", false),
            cand::<u8>("m", "charlie", "u8", false),
            cand::<u8>("m", "alpha", "u8", false),
        ]);
        assert_eq!(
            lanes(&after, &is, RegistryOptions::default()).0[0].candidates[0].name,
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
        assert_eq!(
            lanes(&cs, &is, RegistryOptions::default()).0[0].candidates[0].name,
            "zulu"
        );
    }

    /// Two marked is a contradiction, unlike none.
    #[test]
    fn two_marked_baselines_are_rejected() {
        let cs = leak_c(vec![
            cand::<u8>("m", "a", "u8", true),
            cand::<u8>("m", "b", "u8", true),
        ]);
        let is = leak_i(vec![inp::<u8>("m", "i", "u8")]);
        let (lanes, problems) = lanes(&cs, &is, RegistryOptions::default());
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
        let (lanes, problems) = lanes(&cs, &is, RegistryOptions::default());
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

    /// A lone candidate has nothing to compare against. It is still measured,
    /// as a plain benchmark, rather than being dropped or panicking inside
    /// `add_comparison`.
    #[test]
    fn a_lane_with_one_candidate_is_kept_for_plain_measurement() {
        let cs = leak_c(vec![cand::<u8>("m", "only", "u8", false)]);
        let is = leak_i(vec![inp::<u8>("m", "i", "u8")]);
        let (lanes, problems) = lanes(&cs, &is, RegistryOptions::default());
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
        let (_, problems) = lanes(&cs, &is, RegistryOptions::default());
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
        let (lanes, problems) = lanes(&cs, &is, RegistryOptions::default());
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
        let (lanes, _) = lanes(&cs, &is, RegistryOptions::default());
        assert_eq!(
            lanes.iter().map(|l| l.matrix).collect::<Vec<_>>(),
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
        let (lanes, problems) = lanes(&cs, &is, RegistryOptions::default());
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
        let (lanes, _) = lanes(&cs, &is, RegistryOptions::default());
        assert_eq!(
            lanes[0].candidates[0].name, "sort@0.8.0",
            "the old version is what the new one is compared against",
        );
    }

    #[test]
    fn the_baseline_policy_can_pick_the_newest_instead() {
        let cs = leak_c(vec![
            cand_from::<u8>("m", "sort", "u8", true, "mycrate", "0.9.0"),
            cand_from::<u8>("m", "sort", "u8", true, "mycrate", "0.8.0"),
        ]);
        let is = leak_i(vec![inp::<u8>("m", "i", "u8")]);
        let opts = RegistryOptions::default().with_baseline(BaselinePolicy::Newest);
        let (lanes, _) = lanes(&cs, &is, opts);
        assert_eq!(lanes[0].candidates[0].name, "sort@0.9.0");
    }

    #[test]
    fn the_baseline_policy_can_name_one_exactly() {
        let cs = leak_c(vec![
            cand_from::<u8>("m", "sort", "u8", true, "mycrate", "0.9.0"),
            cand_from::<u8>("m", "sort", "u8", true, "mycrate", "0.8.0"),
            cand_from::<u8>("m", "sort", "u8", true, "mycrate", "0.7.0"),
        ]);
        let is = leak_i(vec![inp::<u8>("m", "i", "u8")]);
        let opts = RegistryOptions::default().with_baseline(BaselinePolicy::Exact {
            crate_name: "mycrate",
            crate_version: "0.8.0",
        });
        let (lanes, _) = lanes(&cs, &is, opts);
        assert_eq!(lanes[0].candidates[0].name, "sort@0.8.0");
    }

    /// Naming one that is not there is an error rather than a silent
    /// fallback: the caller asked for a specific comparison and did not get
    /// it.
    #[test]
    fn an_exact_baseline_that_is_absent_is_an_error() {
        let cs = leak_c(vec![
            cand_from::<u8>("m", "sort", "u8", true, "mycrate", "0.9.0"),
            cand_from::<u8>("m", "sort", "u8", true, "mycrate", "0.8.0"),
        ]);
        let is = leak_i(vec![inp::<u8>("m", "i", "u8")]);
        let opts = RegistryOptions::default().with_baseline(BaselinePolicy::Exact {
            crate_name: "mycrate",
            crate_version: "0.1.0",
        });
        let (lanes, problems) = lanes(&cs, &is, opts);
        assert!(lanes.is_empty());
        assert!(
            matches!(&problems[0], Diagnostic::NoSuchBaseline { .. }),
            "{problems:?}",
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
        let (lanes, problems) = lanes(&cs, &is, RegistryOptions::default());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(lanes[0].inputs.len(), 1, "one input, not one per version");
        assert_eq!(
            lanes[0].inputs[0].origin.crate_version, "0.9.0",
            "and it is the newest, not whichever happened to register first",
        );
    }

    /// `LatestPerCrate` keeps the newest of *each* crate.
    ///
    /// Per crate rather than overall, because the point of measuring against
    /// other crates is to measure against them: dropping a rival's
    /// implementation because your own version number is higher would be
    /// exactly wrong.
    #[test]
    fn latest_per_crate_keeps_every_crate_and_drops_only_old_copies() {
        let cs = leak_c(vec![
            cand_from::<u8>("m", "sort", "u8", true, "mine", "2.0.0"),
            cand_from::<u8>("m", "sort", "u8", true, "mine", "1.0.0"),
            // A rival, on a lower version number than mine.
            cand_from::<u8>("m", "sort", "u8", false, "theirs", "0.3.0"),
            cand_from::<u8>("m", "sort", "u8", false, "theirs", "0.2.0"),
        ]);
        let is = leak_i(vec![inp::<u8>("m", "i", "u8")]);
        let (lanes, problems) = lanes(&cs, &is, RegistryOptions::latest_per_crate());
        assert!(problems.is_empty(), "{problems:?}");
        let names: Vec<&str> = lanes[0]
            .candidates
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names.len(), 2, "one per crate: {names:?}");
        assert!(names.contains(&"sort@mine"), "{names:?}");
        assert!(
            names.contains(&"sort@theirs"),
            "the rival must survive its lower version number: {names:?}",
        );
        // Only the crate distinguishes them now, so the version is not in
        // the name - the least that tells them apart is what is used.
        assert!(!names.iter().any(|n| n.contains("2.0.0")), "{names:?}");
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
        let (lanes, _) = lanes(&cs, &is, RegistryOptions::default());
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
        let (lanes, problems) = lanes(&cs, &is, RegistryOptions::default());
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
        let (lanes, problems) = lanes(&cs, &is, RegistryOptions::default());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(lanes.len(), 2, "both matrices survive");
        for lane in &lanes {
            let names: Vec<&str> = lane.candidates.iter().map(|c| c.name.as_str()).collect();
            assert!(
                names.contains(&"sort"),
                "unrenamed in {}: {names:?}",
                lane.matrix
            );
        }
    }
}
