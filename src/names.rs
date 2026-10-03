//! What the things a run measured are called, and which shorter names stand
//! for them.
//!
//! The full names are made here, in one place: [`comparison_name`] and
//! [`candidate_name`] for what a group produces, and the benchmark's own name
//! for a standalone one. A [`Report`](crate::Report) then accepts, as well as
//! a full name, any shorter way of writing it that picks out exactly one thing,
//! and says why when a name does not.

use std::fmt::{self, Display, Formatter};

/// What an [`Address`] stands for, which is also what the accessor that looks
/// for it is asking after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// One timed thing: a standalone benchmark, or one candidate of a group.
    Single,
    /// A group's candidates on one input, measured together.
    Comparison,
    /// A scaling benchmark.
    Scaling,
}

impl Kind {
    /// What a name of this kind is, for a message.
    fn described(self) -> &'static str {
        match self {
            Kind::Single => "one timing",
            Kind::Comparison => "a comparison",
            Kind::Scaling => "a scaling benchmark",
        }
    }

    /// The `Report` method that gives a name of this kind.
    fn accessor(self) -> &'static str {
        match self {
            Kind::Single => "timing",
            Kind::Comparison => "comparison",
            Kind::Scaling => "scaling",
        }
    }
}

/// One name a report knows, and the shorter ways of writing it.
#[derive(Debug, Clone)]
pub(crate) struct Address {
    /// The name as it is listed.
    pub(crate) full: String,
    /// Other names that stand for it, when nothing else shares them.
    forms: Vec<String>,
    pub(crate) kind: Kind,
    /// Which entry of the report it is, and which of that entry's
    /// alternatives (always `0` unless it is one candidate of a comparison).
    pub(crate) entry: usize,
    pub(crate) alt: usize,
}

impl Address {
    pub(crate) fn new(
        full: String,
        forms: Vec<String>,
        kind: Kind,
        entry: usize,
        alt: usize,
    ) -> Self {
        Address {
            full,
            forms,
            kind,
            entry,
            alt,
        }
    }
}

/// The name of a comparison: `group@input`, or just `group` when the group has
/// no input. `type_name` is given when another input of the group, of another
/// type, has the same name, and is then added: `group@input (Vec<u8>)`.
pub(crate) fn comparison_name(group: &str, input: &str, type_name: Option<&str>) -> String {
    name_of(None, group, input, type_name)
}

/// The name of one candidate of a group: `group:candidate@input`, with the
/// input left off when the group has none and the type added as for
/// [`comparison_name`].
pub(crate) fn candidate_name(
    group: &str,
    candidate: &str,
    input: &str,
    type_name: Option<&str>,
) -> String {
    name_of(Some(group), candidate, input, type_name)
}

/// `scope:thing@input (type)`, each part present only if given.
fn name_of(scope: Option<&str>, thing: &str, input: &str, type_name: Option<&str>) -> String {
    let mut name = String::new();
    if let Some(scope) = scope {
        name.push_str(scope);
        name.push(':');
    }
    name.push_str(thing);
    if !input.is_empty() {
        name.push('@');
        name.push_str(input);
    }
    if let Some(type_name) = type_name {
        name.push_str(" (");
        name.push_str(type_name);
        name.push(')');
    }
    name
}

/// The name of one alternative of something that is not a registered group,
/// such as a group built by hand: `entry:alternative`.
pub(crate) fn alternative_name(entry: &str, alternative: &str) -> String {
    name_of(Some(entry), alternative, "", None)
}

/// The shorter ways to write [`alternative_name`]: the entry's name shortened as
/// [`path_forms`] does, or the alternative's alone.
pub(crate) fn alternative_forms(entry: &str, alternative: &str) -> Vec<String> {
    path_forms(entry)
        .iter()
        .map(|entry| alternative_name(entry, alternative))
        .chain([alternative.to_string()])
        .collect()
}

/// The shorter ways to write `a::b::f`: `b::f` and `f`.
pub(crate) fn path_forms(name: &str) -> Vec<String> {
    let parts: Vec<&str> = name.split("::").collect();
    (1..parts.len()).map(|k| parts[k..].join("::")).collect()
}

/// The shorter ways to write a comparison's name: without the type, without
/// the input, or without either.
pub(crate) fn comparison_forms(group: &str, input: &str, type_name: Option<&str>) -> Vec<String> {
    forms_of(None, group, input, type_name)
}

/// The shorter ways to write a candidate's name: without the group, the input,
/// or the type, in any combination.
pub(crate) fn candidate_forms(
    group: &str,
    candidate: &str,
    input: &str,
    type_name: Option<&str>,
) -> Vec<String> {
    forms_of(Some(group), candidate, input, type_name)
}

fn forms_of(scope: Option<&str>, thing: &str, input: &str, type_name: Option<&str>) -> Vec<String> {
    let full = name_of(scope, thing, input, type_name);
    let mut forms = Vec::new();
    for keep_scope in [true, false] {
        // Only a name with a scope has one to leave off.
        if !keep_scope && scope.is_none() {
            continue;
        }
        for keep_input in [true, false] {
            if !keep_input && input.is_empty() {
                continue;
            }
            for keep_type in [true, false] {
                if !keep_type && type_name.is_none() {
                    continue;
                }
                let form = name_of(
                    scope.filter(|_| keep_scope),
                    thing,
                    if keep_input { input } else { "" },
                    type_name.filter(|_| keep_type),
                );
                if form != full && !forms.contains(&form) {
                    forms.push(form);
                }
            }
        }
    }
    forms
}

/// Why a name found nothing.
pub(crate) enum Miss {
    /// Nothing is called that, in the kinds asked after.
    Nothing,
    /// More than one thing is: the full names of what fits.
    Several(Vec<String>),
}

/// The address of `kinds` that `query` names: one whose full name it is, or
/// else the only one that it is a shorter form of. Otherwise why not: nothing
/// is, or more than one is, since guessing between them would be worse than
/// saying so.
pub(crate) fn resolve<'a>(
    addresses: &'a [Address],
    query: &str,
    kinds: &[Kind],
) -> Result<&'a Address, Miss> {
    let wanted = || addresses.iter().filter(|a| kinds.contains(&a.kind));
    // A full name is never ambiguous, whatever else it also abbreviates.
    if let Some(exact) = wanted().find(|a| a.full == query) {
        return Ok(exact);
    }
    let mut claimed: Vec<&Address> = wanted()
        .filter(|a| a.forms.iter().any(|form| form == query))
        .collect();
    match claimed.len() {
        0 => Err(Miss::Nothing),
        1 => Ok(claimed.remove(0)),
        _ => Err(Miss::Several(
            claimed.into_iter().map(|a| a.full.clone()).collect(),
        )),
    }
}

/// Why a [`Report`](crate::Report) could not give what was asked for by name.
///
/// There are three reasons, and the message says which: nothing has that name;
/// a shortened name fits more than one thing, which are listed so that more of
/// the name can be given; or the name is the name of something else, a
/// comparison asked for as a single timing, say.
///
/// Printing it, with `{}` or `{:?}`, gives the message, so `.expect("..")` on a
/// failed lookup says what to fix. It is a [`std::error::Error`], so `?` works
/// in a `main` that returns `Result<(), Box<dyn Error>>`.
#[derive(Clone)]
pub struct NameError {
    message: String,
}

impl NameError {
    /// `name` found nothing among `wanted`, which is listed by `lister`.
    fn nothing(name: &str, wanted: Kind) -> Self {
        let lister = match wanted {
            Kind::Comparison => "`Report::all_comparisons`",
            Kind::Single | Kind::Scaling => "`Report::names`",
        };
        NameError {
            message: format!("nothing is called `{name}`; {lister} lists what ran"),
        }
    }

    /// `name` fits each of `full_names`.
    fn several(name: &str, full_names: &[String]) -> Self {
        // A name can fit a great many things; the first few say enough.
        const SHOWN: usize = 5;
        let mut listed: Vec<String> = full_names
            .iter()
            .take(SHOWN)
            .map(|full| format!("`{full}`"))
            .collect();
        if full_names.len() > SHOWN {
            listed.push(format!("and {} more", full_names.len() - SHOWN));
        }
        NameError {
            message: format!(
                "`{name}` fits more than one thing: {}; use more of the name",
                listed.join(", ")
            ),
        }
    }

    /// `name` is the name of a `found`, but a `wanted` was asked for.
    fn elsewhere(name: &str, found: Kind, wanted: Kind) -> Self {
        NameError {
            message: format!(
                "`{name}` is {}, not {}; ask for it with `Report::{}`",
                found.described(),
                wanted.described(),
                found.accessor(),
            ),
        }
    }

    /// Why `query` found nothing of kind `wanted`, given what else is there.
    pub(crate) fn because(addresses: &[Address], query: &str, wanted: Kind, miss: Miss) -> Self {
        match miss {
            Miss::Several(full_names) => NameError::several(query, &full_names),
            Miss::Nothing => [Kind::Single, Kind::Scaling, Kind::Comparison]
                .into_iter()
                .filter(|kind| *kind != wanted)
                .find(|kind| resolve(addresses, query, &[*kind]).is_ok())
                .map_or_else(
                    || NameError::nothing(query, wanted),
                    |found| NameError::elsewhere(query, found, wanted),
                ),
        }
    }
}

impl Display for NameError {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// The same text as [`Display`], so that `unwrap` and `expect` show it as it
/// reads rather than as a quoted string.
impl fmt::Debug for NameError {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        Display::fmt(self, f)
    }
}

impl std::error::Error for NameError {}
