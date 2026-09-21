//! Attribute macros that register benchmarks for the `scaling` crate.
//!
//! These are re-exported by `scaling` itself and are not meant to be depended
//! on directly.
//!
//! # What these do, and what they deliberately do not
//!
//! Each macro writes two things beside the function it is put on: a shim `fn`
//! that knows how to add the benchmark to a suite, and an
//! `inventory::submit!` that registers the shim. That is all. In particular
//! they do **not** wrap the function in any `#[cfg]`: a `#[cfg]` written
//! above the attribute already strips the whole item before expansion, so a
//! caller who wants benchmarks kept out of ordinary builds writes
//!
//! ```ignore
//! #[cfg(feature = "my-benchmarks")]
//! #[scaling::bench]
//! fn something() { ... }
//! ```
//!
//! and picks their own feature name. Emitting a fixed `#[cfg(feature =
//! "scaling-bench")]` would mean a convention nobody asked for, and would
//! compile silently to nothing for anyone who had not defined that exact
//! feature.
//!
//! # Why generics never reach the registry
//!
//! A registry holds one type, and a benchmark is generic in its closure, its
//! input and its return type. None of that has to survive: the concrete types
//! are known *here*, at the expansion site, so the shim written here has a
//! fixed signature and what crosses into the registry is a plain function
//! pointer.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{parse_macro_input, Expr, FnArg, ItemFn, LitInt, LitStr, ReturnType, Type};

/// Register a benchmark, as `scaling::bench` would run it.
///
/// ```ignore
/// #[scaling::bench]
/// fn fib_200() -> usize { fib(200) }
///
/// #[scaling::bench(input = vec![0u8; 1024])]
/// fn hash(buf: &Vec<u8>) -> u64 { hash_of(buf) }
///
/// #[scaling::bench(make_input = || random_vec(1000))]
/// fn sort(v: &mut Vec<i32>) { v.sort() }
///
/// #[scaling::bench(make_input = || random_vec(1000))]
/// fn total(v: Vec<i32>) -> i32 { v.into_iter().sum() }
/// ```
///
/// The input argument may be `&I`, `&mut I`, or `I` by value - `&I` for one
/// like `hash` above that only reads it, plain `I` for one like `total`
/// that has to consume it (`into_iter()` needs ownership, and there is no
/// borrowed way to write it). The registry itself only ever hands out
/// `&mut I`; an ordinary reference downgrades it at the call site, the same
/// way any `&mut T` reborrows as `&T` when a function asks for less, and
/// `I` by value is taken from behind an `Option` the generated shim manages
/// for you - nothing to write by hand for it.
///
/// A zero-argument function returning `impl Fn() -> O` or `impl FnMut() ->
/// O` is a fourth shape, for state that has to persist and be mutated
/// across many calls rather than being rebuilt fresh each time - the one
/// pattern none of the above can express, because every input above is
/// prepared anew per call:
///
/// ```ignore
/// #[scaling::bench]
/// fn next_from_rng() -> impl FnMut() -> u64 {
///     let mut rng = StdRng::seed_from_u64(0);
///     move || rng.next_u64()
/// }
/// ```
///
/// The function itself runs once, to build the closure; every timed call
/// after that calls the closure it returned. This is not a smaller version
/// of `make_input` - it costs nothing extra to have. `impl Trait` in return
/// position is a concrete type the compiler already knows at the call
/// site, not a boxed one, so the generated shim holds it in a plain
/// `Option` and calls it directly: no allocation, no dynamic dispatch, on
/// any call, ever. Measured against the `thread_local!`-based workaround
/// this shape replaces, it comes out faster, not merely equivalent - the
/// workaround pays for a thread-local lookup on every call that this does
/// not.
///
/// `input = <value>` combines with this: the setup function takes `&I`,
/// `&mut I`, or `I` by value, exactly as it would without a setup shape, to
/// build its persistent state from.
///
/// ```ignore
/// #[scaling::bench(input = 0u64)]
/// fn next_from_seed(seed: &u64) -> impl FnMut() -> u64 {
///     let mut rng = StdRng::seed_from_u64(*seed);
///     move || rng.next_u64()
/// }
/// ```
///
/// `input` here is given to the setup function once, the same one time the
/// function itself runs - not cloned/regenerated per call the way a plain
/// `input = <value>` benchmark's is, since there is no per-call rebuild for
/// it to feed. Reading `seed` to configure `rng`, as above, is what setup
/// normally does with a reference and compiles as ordinary code; trying to
/// have the *returned closure itself* keep borrowing the input - `move ||
/// v.len()` for a `v: &mut Vec<u8>` parameter, say - does not, and is
/// rejected at the setup function's own definition (`error[E0700]: hidden
/// type ... captures lifetime that does not appear in bounds`): the
/// reference setup receives is only good for the one call that builds the
/// state, not for every timed call after. Take `I` by value instead when the
/// state genuinely needs to own what was borrowed.
///
/// `make_input` does not combine with this shape - a setup function whose
/// returned closure itself takes an argument is rejected outright, in
/// favor of a pattern that needs no special shape on the benchmark function
/// at all and, unlike this one, composes with comparisons: build the
/// expensive part once behind an `Arc` inside `make_input`'s own closure,
/// and pair it with a fresh per-call value as an ordinary tuple input.
///
/// ```ignore
/// #[scaling::bench(make_input = {
///     let sorted: std::sync::Arc<Vec<u64>> = std::sync::Arc::new((0..1_000_000).collect());
///     move || (sorted.clone(), rand::random::<u64>() % 1_000_000)
/// })]
/// fn search_in_sorted(input: &(std::sync::Arc<Vec<u64>>, u64)) -> bool {
///     input.0.binary_search(&input.1).is_ok()
/// }
/// ```
///
/// `sorted` is built once, the moment `make_input`'s own block runs, because
/// `make_input` accepts any expression - a block that builds something once
/// and returns a closure capturing it is ordinary Rust, nothing this crate
/// has to know about. `make_input` itself is still called fresh on every
/// timed call as it always is; cloning the `Arc` is cheap, and a
/// comparison's every alternative sees the *same* clone, which a setup
/// function's own returned closure - private to just that one benchmark -
/// could never guarantee. Reach for the setup-once shape above instead
/// when the thing that must vary per call is not an input at all but
/// mutation whose own cost is what you are measuring - advancing an RNG's
/// internal state, say: moving that into `make_input` would exclude the
/// very cost you wanted timed, since nothing `make_input` does is on the
/// clock.
///
/// Add `group = "name"` to make this one alternative of a comparison, and
/// `baseline` on exactly one member of each group to say which the others
/// are reported against - registrations have no order, so it cannot be the
/// first one added as it is for a hand-built `ComparisonSet`. `name = "..."`
/// overrides the reported name, which defaults to the module-qualified path
/// of the function.
///
/// A group's members share one input, declared once with
/// [`bench_input`](macro@bench_input) rather than with `input =` or
/// `make_input =` on each member - a group compiled with either of those is
/// rejected, because a per-alternative input would break the pairing that
/// makes a comparison's error bar narrower than two separate measurements:
///
/// ```ignore
/// #[scaling::bench_input(group = "sort")]
/// fn sort_data() -> Vec<i32> { random_vec(1000) }
///
/// #[scaling::bench(group = "sort", baseline)]
/// fn std_sort(v: &mut Vec<i32>) { v.sort() }
///
/// #[scaling::bench(group = "sort")]
/// fn unstable(v: &mut Vec<i32>) { v.sort_unstable() }
/// ```
///
/// A group's members must currently take `&I` or `&mut I`, not `I` by
/// value - not a fundamental restriction, just not yet taught to a group's
/// shared, cloned-per-round input.
///
/// A member may also use the setup-once shape above, returning `impl
/// Fn()/FnMut() -> O` instead of `O` directly: setup runs once for that
/// member, not once per timed call, exactly as it would outside a group.
/// The group's shared input is still regenerated and cloned every round
/// regardless - a comparison's pairing depends on every member seeing that
/// round's input, setup-once member included - so this saves the setup
/// function's own work, not the input machinery's.
#[proc_macro_attribute]
pub fn bench(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as Args);
    let func = parse_macro_input!(item as ItemFn);
    expand(args, func, Flavour::Flat)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Register a scaling benchmark, as `scaling::bench_scaling` would run it.
///
/// ```ignore
/// #[scaling::bench_scaling(nmin = 100)]
/// fn sort_n(n: usize) -> Vec<i32> { let mut v = random_n(n); v.sort(); v }
/// ```
///
/// `nmin` must be a literal: it is baked into the generated shim, whose
/// signature has nowhere to pass it.
///
/// # Holding size-dependent setup out of the timed call
///
/// The function above is timed in full, `random_n(n)` included - fine when
/// building the input *is* part of what you want measured, wrong when it
/// isn't: a scaling benchmark of "parse a buffer of size `n`" that also
/// allocates and fills that buffer inside the timed call is measuring
/// "allocate, fill, then parse", and the fitted power and constant answer
/// for that combination rather than for parsing alone.
///
/// `make_input = |n: usize| -> I { ... }` moves that cost out of the timed
/// region, the same idea as `#[bench(make_input = ...)]` but handed the size
/// so it can build an input of exactly that size:
///
/// ```ignore
/// #[scaling::bench_scaling(nmin = 8, make_input = |n: usize| random_n(n))]
/// fn sort_n(v: &mut Vec<i32>) -> usize {
///     v.sort();
///     v.len()
/// }
/// ```
///
/// Called once per sample, before timing starts; only the function body -
/// `v.sort()` above - is on the clock. `input = <value>` is rejected here
/// rather than accepted: a scaling benchmark sweeps `n`, so a fixed input
/// that does not vary with it would be measuring the same size at every
/// point on the curve, which is not a scaling law.
///
/// As with [`bench`], the function may take `&I`, `&mut I`, or `I` by
/// value.
///
/// # Setup that runs once per size, not once per timed call
///
/// `fn(n: usize) -> impl Fn()/FnMut() -> O` is [`bench`]'s setup-once shape,
/// applied to the size rather than an input: `n` builds something once, and
/// every timed call at that size reuses what it built instead of rebuilding
/// it. A scaling sweep revisits every size it discovers once per round -
/// `nmin`, the next one up, ..., back to `nmin`, and around again - rather
/// than advancing through sizes once each, so setup here is cached *per
/// size*: the first call at a given `n` runs it, every later call at that
/// same `n` reuses what it returned.
///
/// ```ignore
/// #[scaling::bench_scaling(nmin = 8)]
/// fn sort_n(n: usize) -> impl FnMut() -> usize {
///     let mut v: Vec<u64> = random_n(n);
///     move || {
///         v.sort();
///         v.len()
///     }
/// }
/// ```
///
/// `make_input` does not combine with this shape, same as [`bench`]'s own
/// setup-once shape: a setup function whose returned closure takes an
/// argument is rejected outright. `make_input` already accepts any
/// expression, including a stateful closure that caches its own expensive
/// part - keyed by `n`, since a scaling sweep asks for many different
/// sizes - behind an `Arc`, ordinary Rust with nothing new for this crate
/// to support:
///
/// ```ignore
/// #[scaling::bench_scaling(nmin = 1_000, make_input = {
///     let mut cache: HashMap<usize, std::sync::Arc<BigMap>> = HashMap::new();
///     move |n: usize| {
///         let map = cache.entry(n).or_insert_with(|| std::sync::Arc::new(build_big_map(n)));
///         (map.clone(), random_key(n))
///     }
/// })]
/// fn random_lookup(input: &(std::sync::Arc<BigMap>, u64)) -> u64 {
///     *input.0.get(&input.1).unwrap()
/// }
/// ```
///
/// The cache is bounded the same way [`bench_scaling`](macro@bench_scaling)'s own
/// per-size cache is: by however many distinct sizes the sweep visits, a
/// handful in practice. `make_input` itself is still called fresh on every
/// timed call, exactly as it always is; cloning the `Arc` is cheap, and -
/// unlike a setup function's own private, per-benchmark state - every
/// alternative in a comparison sharing this input sees the same clone.
/// Reach for the setup-once shape above instead only when what must vary
/// per call is not an input but mutation whose own cost is the
/// measurement: `make_input`'s own work is never on the clock, so moving
/// such a mutation there would exclude the very cost you meant to time.
#[proc_macro_attribute]
pub fn bench_scaling(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as Args);
    let func = parse_macro_input!(item as ItemFn);
    expand(args, func, Flavour::Scaling)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Declare the shared input of a comparison group.
///
/// Named to pair with [`bench`]: this is the group-wide counterpart of the
/// per-benchmark `make_input = ...` argument `#[bench]` itself takes - the
/// same idea, "build a fresh input", but shared by every alternative in a
/// group rather than private to one benchmark.
///
/// ```ignore
/// #[scaling::bench_input(group = "sort")]
/// fn sort_data() -> Vec<i32> { random_vec(1000) }
/// ```
///
/// Called once per round, and the value cloned for each alternative, so that
/// all of them are measured on the same input - which is what makes their
/// differences paired. Exactly one per group.
///
/// A `group = "..."` on `#[bench]`/`#[bench_scaling]` itself cannot also
/// take `input = ...` or `make_input = ...`: an alternative's input always
/// comes from its group's `#[bench_input]`, and per-alternative inputs
/// would break the pairing the whole statistical model depends on. Use this
/// attribute instead.
#[proc_macro_attribute]
pub fn bench_input(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as Args);
    let func = parse_macro_input!(item as ItemFn);
    expand_bench_input(args, func)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Which of the registry's kinds is being written.
enum Flavour {
    Flat,
    Scaling,
}

/// The parsed contents of the attribute's parentheses.
#[derive(Default)]
struct Args {
    name: Option<LitStr>,
    group: Option<LitStr>,
    matrix: Option<LitStr>,
    baseline: bool,
    input: Option<Expr>,
    make_input: Option<Expr>,
    nmin: Option<LitInt>,
    /// `types(A, B)`: instantiate a generic candidate once per type.
    types: Vec<Type>,
    /// `sizes(1, 2)`: register an input once per size.
    sizes: Vec<LitInt>,
}

impl syn::parse::Parse for Args {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut args = Args::default();
        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            match key.to_string().as_str() {
                // A bare word, no value.
                "baseline" => args.baseline = true,
                "name" => {
                    input.parse::<syn::Token![=]>()?;
                    args.name = Some(input.parse()?);
                }
                "group" => {
                    input.parse::<syn::Token![=]>()?;
                    args.group = Some(input.parse()?);
                }
                "matrix" => {
                    input.parse::<syn::Token![=]>()?;
                    args.matrix = Some(input.parse()?);
                }
                // These two take a parenthesised list rather than a value,
                // since each stands for several registrations.
                "types" => {
                    let inner;
                    syn::parenthesized!(inner in input);
                    let listed =
                        syn::punctuated::Punctuated::<Type, syn::Token![,]>::parse_terminated(
                            &inner,
                        )?;
                    if listed.is_empty() {
                        return Err(syn::Error::new(
                            key.span(),
                            "`types(..)` lists the types to instantiate at, so it needs at \
                             least one",
                        ));
                    }
                    args.types = listed.into_iter().collect();
                }
                "sizes" => {
                    let inner;
                    syn::parenthesized!(inner in input);
                    let listed =
                        syn::punctuated::Punctuated::<LitInt, syn::Token![,]>::parse_terminated(
                            &inner,
                        )?;
                    if listed.is_empty() {
                        return Err(syn::Error::new(
                            key.span(),
                            "`sizes(..)` lists the sizes to register at, so it needs at \
                             least one",
                        ));
                    }
                    args.sizes = listed.into_iter().collect();
                }
                "input" => {
                    input.parse::<syn::Token![=]>()?;
                    args.input = Some(input.parse()?);
                }
                "make_input" => {
                    input.parse::<syn::Token![=]>()?;
                    args.make_input = Some(input.parse()?);
                }
                "nmin" => {
                    input.parse::<syn::Token![=]>()?;
                    args.nmin = Some(input.parse()?);
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "unknown option `{other}`; expected one of \
                             name, group, matrix, baseline, input, make_input, \
                             nmin, types(..), sizes(..)",
                        ),
                    ))
                }
            }
            if input.is_empty() {
                break;
            }
            input.parse::<syn::Token![,]>()?;
        }
        Ok(args)
    }
}

/// The input type a benchmark function takes, read off its own signature.
///
/// `None` means it takes nothing, which the registry represents as `()` -
/// so a group of no-input alternatives and a group of generated-input ones
/// are one code path rather than two.
/// What a benchmark function's own signature declares its input to be.
enum Input {
    /// No argument: the benchmark takes nothing.
    None,
    /// `&I` or `&mut I`. The registry only ever hands out `&mut I` - one
    /// erasure mechanism, not two - but the generated shim calls the
    /// benchmark as an ordinary function call rather than passing it as a
    /// value to satisfy some generic bound elsewhere, so an ordinary
    /// reborrow at that call site turns the `&mut I` into `&I` when that is
    /// what the signature asks for. Nothing downstream needs to know which
    /// was written.
    Ref(Type),
    /// `I`, by value: the function consumes its input. Only meaningful
    /// where the caller checks for it - see [`Input::owned_rejected`].
    Owned(Type),
}

impl Input {
    /// The type, regardless of whether it arrived by reference or by
    /// value - what type-identity code (matrix pairing, a group's shared
    /// input) cares about.
    fn ty(&self) -> Option<&Type> {
        match self {
            Input::None => None,
            Input::Ref(ty) | Input::Owned(ty) => Some(ty),
        }
    }

    /// An error for a context that has not been taught to accept an owned
    /// input, naming what to write instead.
    fn owned_rejected(&self, span: proc_macro2::Span) -> Option<syn::Error> {
        match self {
            Input::Owned(_) => Some(syn::Error::new(
                span,
                "this benchmark takes its input by value, which is not supported here yet - \
                 take it as `&I` or `&mut I` instead",
            )),
            _ => None,
        }
    }
}

fn input_kind(func: &ItemFn) -> syn::Result<Input> {
    let mut args = func.sig.inputs.iter();
    let first = match args.next() {
        None => return Ok(Input::None),
        Some(a) => a,
    };
    if args.next().is_some() {
        return Err(syn::Error::new(
            func.sig.inputs.span(),
            "a benchmark takes at most one argument, its input",
        ));
    }
    let pat = match first {
        FnArg::Receiver(r) => {
            return Err(syn::Error::new(
                r.span(),
                "a benchmark must be a free function, not a method",
            ))
        }
        FnArg::Typed(t) => t,
    };
    match &*pat.ty {
        Type::Reference(r) => Ok(Input::Ref((*r.elem).clone())),
        other => Ok(Input::Owned(other.clone())),
    }
}

/// What kind of "run setup once, then call the result repeatedly" shape a
/// function's return type promises, if any - a benchmark, a group member, a
/// matrix candidate, or a scaling sweep may all return this instead of `O`
/// directly. Only the return type says which; the function's own arguments
/// (none, an input, or the size a scaling sweep passes) are a separate
/// question every call site decides for itself.
#[derive(PartialEq, Eq)]
enum Repeatable {
    /// An ordinary return type - not this shape.
    No,
    /// `impl Fn() -> O` / `impl FnMut() -> O`: setup takes no further
    /// input, ever, after the one call that builds it.
    NoArg,
    /// `impl Fn(K) -> O` / `impl FnMut(K) -> O`: not a supported shape -
    /// detected only to give a clear rejection rather than silently
    /// mistreating the returned closure as the benchmark's own output. See
    /// the rejection message at each call site for what to write instead:
    /// an ordinary input built from state cached behind an `Arc` inside
    /// `make_input`'s own closure.
    OneArg,
}

/// Not `impl FnOnce() -> O`, and not more than one argument: a `FnOnce`
/// cannot be called more than the one time setup-once's whole point is to
/// avoid, and nothing here has a second argument to feed.
fn returns_repeatable_closure(sig: &syn::Signature) -> Repeatable {
    let syn::ReturnType::Type(_, ty) = &sig.output else {
        return Repeatable::No;
    };
    let Type::ImplTrait(imp) = &**ty else {
        return Repeatable::No;
    };
    for bound in &imp.bounds {
        let syn::TypeParamBound::Trait(trait_bound) = bound else {
            continue;
        };
        let Some(last) = trait_bound.path.segments.last() else {
            continue;
        };
        if last.ident != "Fn" && last.ident != "FnMut" {
            continue;
        }
        let syn::PathArguments::Parenthesized(p) = &last.arguments else {
            continue;
        };
        return match p.inputs.len() {
            0 => Repeatable::NoArg,
            1 => Repeatable::OneArg,
            _ => Repeatable::No,
        };
    }
    Repeatable::No
}

/// The input type, spelled the way a person would.
///
/// Not `stringify!`: that keeps the spacing of the tokens it was handed, and
/// tokens re-emitted by a proc macro have lost theirs - so `Vec<u64>` comes
/// out as `Vec < u64 >`, which then appears in a matrix heading and in every
/// diagnostic that names a type. Rebuilding it here rather than tidying it
/// up at each place it is printed keeps one spelling, which matters because
/// this string is an *identity*: assembly pairs candidates with inputs by
/// comparing it.
fn type_name(ty: &Type) -> TokenStream2 {
    let spaced = quote!(#ty).to_string();
    let hugs = |c: char| "<>()[]&:;,".contains(c);
    let mut out = String::with_capacity(spaced.len());
    let chars: Vec<char> = spaced.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c != ' ' {
            out.push(*c);
            continue;
        }
        // A space survives only between two things that would otherwise run
        // together - `&mut u8`, `dyn Any` - and after a separator, where
        // dropping it would give `HashMap<String,u8>`.
        let previous = out.chars().last();
        let next = chars.get(i + 1).copied();
        if let (Some(p), Some(n)) = (previous, next) {
            if (!hugs(p) && !hugs(n)) || p == ',' || p == ';' {
                out.push(' ');
            }
        }
    }
    quote!(#out)
}

/// The `TypeId::of` expression and the printable name for `ty`, or `()`'s
/// when there is none.
fn ty_id_and_name(ty: Option<&Type>) -> (TokenStream2, TokenStream2) {
    match ty {
        Some(ty) => (quote!(::core::any::TypeId::of::<#ty>), type_name(ty)),
        None => (quote!(::core::any::TypeId::of::<()>), quote!("()")),
    }
}

/// The expression a shim uses to call `fname` on its input, or with none.
fn call_expr(fname: &syn::Ident, ty: Option<&Type>) -> TokenStream2 {
    match ty {
        Some(ty) => quote!(#fname(__e.get_mut::<#ty>())),
        None => quote!(#fname()),
    }
}

/// Wraps `call` - an expression that runs a setup function once - in Design
/// A's lazy-build-once pattern: the first call runs `call` and keeps its
/// result (the closure the setup function returned); every call after reuses
/// what is stored rather than running `call` again. The caller declares `let
/// mut __action = ::core::option::Option::None;` ahead of the closure this
/// is spliced into and captures it there - where that lives (a group's shim
/// function, a matrix candidate's) differs by call site, so this only
/// builds the part that is the same everywhere.
fn repeatable_call(call: TokenStream2) -> TokenStream2 {
    quote! {
        (__action.get_or_insert_with(|| #call))()
    }
}

/// The reported name: the override if given, else the bare function name.
fn name_or_bare(name: &Option<LitStr>, fname: &syn::Ident) -> TokenStream2 {
    match name {
        Some(lit) => quote!(#lit),
        None => {
            let bare = fname.to_string();
            quote!(#bare)
        }
    }
}

/// `concat!(module_path!(), "::", "fn_name")`, or the override.
fn reported_name(args: &Args, func: &ItemFn) -> TokenStream2 {
    match &args.name {
        Some(lit) => quote!(#lit),
        None => {
            let bare = func.sig.ident.to_string();
            quote!(::core::concat!(::core::module_path!(), "::", #bare))
        }
    }
}

fn expand(args: Args, func: ItemFn, flavour: Flavour) -> syn::Result<TokenStream2> {
    if let (Flavour::Flat, Some(n)) = (&flavour, &args.nmin) {
        return Err(syn::Error::new(
            n.span(),
            "`nmin` belongs to `#[scaling::bench_scaling]`, which measures how a \
             benchmark grows with N",
        ));
    }
    if let (Flavour::Scaling, Some(input)) = (&flavour, &args.input) {
        return Err(syn::Error::new(
            input.span(),
            "`input` fixes one value regardless of size, but a scaling benchmark \
             sweeps `n` - use `make_input = |n: usize| ...` to build a size-dependent \
             input instead",
        ));
    }
    if args.input.is_some() && args.make_input.is_some() {
        return Err(syn::Error::new(
            func.sig.span(),
            "give either `input` or `make_input`, not both: one clones a value per \
             iteration, the other builds a fresh one",
        ));
    }
    // `Flavour::Flat` only: a scaling benchmark in a group is rejected below,
    // for its own, more specific reason, regardless of `input`/`make_input`.
    if let Flavour::Flat = flavour {
        if args.group.is_some() && (args.input.is_some() || args.make_input.is_some()) {
            let span = args
                .input
                .as_ref()
                .map(|e| e.span())
                .or_else(|| args.make_input.as_ref().map(|e| e.span()))
                .unwrap_or_else(|| func.sig.span());
            return Err(syn::Error::new(
                span,
                "a comparison group's alternatives share one input, declared once with \
                 `#[scaling::bench_input(group = \"...\")]` - not `input =` or \
                 `make_input =` on each member, which would give every alternative its \
                 own input and break the pairing a comparison's accuracy depends on",
            ));
        }
    }
    if args.baseline && args.group.is_none() {
        return Err(syn::Error::new(
            func.sig.span(),
            "`baseline` says which alternative of a comparison the others are \
             reported against, so it needs a `group` to be the baseline of",
        ));
    }

    let name = reported_name(&args, &func);
    let fname = &func.sig.ident;
    let shim = format_ident!("__scaling_shim_{}", fname);

    let registration = match (&args.group, &flavour) {
        // An alternative in a comparison group.
        (Some(group), Flavour::Flat) => {
            let kind = input_kind(&func)?;
            if let Some(e) = kind.owned_rejected(func.sig.inputs.span()) {
                return Err(e);
            }
            let baseline = args.baseline;
            // The call, and the type the group's input must have.
            let call = call_expr(fname, kind.ty());
            let (ty_id, ty_name) = ty_id_and_name(kind.ty());
            // A setup function runs once, lazily, on the first of the many
            // calls a comparison makes to this alternative - see
            // `repeatable_call`. The group's shared input still gets
            // regenerated and cloned every round regardless, same as for
            // any other alternative; only the setup function's own work is
            // saved.
            let add = if returns_repeatable_closure(&func.sig) == Repeatable::NoArg {
                let call = repeatable_call(call);
                quote! {
                    let mut __action = ::core::option::Option::None;
                    __set.add(__name, move |__e: &mut ::scaling::registry::ErasedInput| #call)
                }
            } else {
                quote! {
                    __set.add(__name, |__e: &mut ::scaling::registry::ErasedInput| #call)
                }
            };
            quote! {
                #[doc(hidden)]
                fn #shim<'__s>(
                    __set: ::scaling::registry::Alternative<'__s>,
                    __name: &str,
                ) -> ::scaling::registry::Alternative<'__s> {
                    #add
                }
                ::scaling::inventory::submit! {
                    ::scaling::registry::Registered {
                        name: #name,
                        crate_name: ::core::env!("CARGO_PKG_NAME"),
                        crate_version: ::core::env!("CARGO_PKG_VERSION"),
                        group: ::core::option::Option::Some(#group),
                        is_baseline: #baseline,
                        kind: ::scaling::registry::Kind::Alt {
                            add: #shim,
                            input_type: #ty_id,
                            input_type_name: #ty_name,
                        },
                    }
                }
            }
        }
        (Some(g), Flavour::Scaling) => {
            return Err(syn::Error::new(
                g.span(),
                "a scaling benchmark measures how one function grows with N, which is \
                 not something a comparison group compares",
            ))
        }
        // A standalone benchmark.
        (None, Flavour::Flat) => {
            let kind = input_kind(&func)?;
            let owned = matches!(kind, Input::Owned(_));
            let repeatable_kind = returns_repeatable_closure(&func.sig);
            let repeatable = repeatable_kind == Repeatable::NoArg;
            if repeatable && args.make_input.is_some() {
                return Err(syn::Error::new(
                    func.sig.span(),
                    "`make_input` rebuilds its value for every timed call, but a setup \
                     function that returns `impl Fn()/FnMut() -> O` only ever runs \
                     once - use `input = <value>` instead, which this evaluates once, \
                     for exactly that reason",
                ));
            }
            if repeatable_kind == Repeatable::OneArg {
                return Err(syn::Error::new(
                    func.sig.span(),
                    "a setup function whose returned closure takes an argument isn't \
                     supported - whatever that argument's own cost of generating a \
                     fresh value doesn't matter, so build the state once behind an \
                     `Arc`, clone it inside a `make_input` closure alongside a fresh \
                     per-call value as an ordinary tuple, and let this benchmark take \
                     that tuple like any other input; if instead the argument's own \
                     generation *is* part of what you want measured, keep it out of \
                     the closure's signature and mutate captured state inside the \
                     closure body instead - `impl Fn()/FnMut() -> O`, not `impl \
                     Fn(K)/FnMut(K) -> O`",
                ));
            }
            let body = match (&args.input, &args.make_input) {
                // The setup function itself runs once: `input` is given to
                // it there, not cloned/regenerated per call the way
                // `Adder::input`/`make_input` would. Routing this through
                // `Adder::input` instead would still type-check - the lazy
                // `get_or_insert_with` below only ever uses the first of the
                // many clones it would hand out - but it would pay to build
                // every one of those unused clones first, for nothing.
                //
                // The `!owned` arm below always passes `&mut __input`, same
                // reasoning as the ordinary `&I`/`&mut I` arms further down:
                // an ordinary reference downgrades from `&mut` at the call
                // site. Setup reading `v` to configure what it returns (`*v`,
                // `v.clone()`, …) compiles as ordinary safe code; a setup
                // that instead tries to have the returned closure keep
                // borrowing `v` itself is rejected by the compiler right
                // there, at its own definition (E0700, "hidden type captures
                // lifetime that does not appear in bounds") - the borrow
                // only lasts this one call, and nothing here needs it to
                // last longer.
                (Some(input), None) if repeatable && owned => {
                    quote! {
                        {
                            let mut __action = ::core::option::Option::None;
                            __adder.flat(__name, move || {
                                (__action.get_or_insert_with(|| #fname(#input)))()
                            })
                        }
                    }
                }
                (Some(input), None) if repeatable => {
                    quote! {
                        {
                            let mut __input = #input;
                            let mut __action = ::core::option::Option::None;
                            __adder.flat(__name, move || {
                                (__action.get_or_insert_with(|| #fname(&mut __input)))()
                            })
                        }
                    }
                }
                // `|__v| #fname(__v)`, not `#fname` passed directly: `Adder`
                // always hands out `&mut I`, and a named function's own type
                // does not satisfy a generic `FnMut(&mut I)` bound merely
                // because Rust would reborrow `&mut I` as `&I` at an
                // ordinary call site - that coercion only applies to an
                // actual call expression, which this closure body gives it.
                (Some(input), None) if !owned => {
                    quote!(__adder.input(__name, #input, |__v| #fname(__v)))
                }
                (None, Some(gen)) if !owned => {
                    quote!(__adder.make_input(__name, #gen, |__v| #fname(__v)))
                }
                // The function consumes its input, so `Adder` - which only
                // ever hands out `&mut I` - cannot call it directly. Wrap
                // the stored value in `Option`, hand out `&mut Option<I>`
                // as always, and `.take()` the real value out of it right
                // before the call: exactly the trick this crate's own
                // documentation would otherwise have to tell a caller to
                // write by hand. `.take()` can only ever see `None` if
                // something else already emptied this slot, which nothing
                // does - each slot is visited once per round, by this
                // closure alone.
                (Some(input), None) => {
                    quote! {
                        __adder.input(
                            __name,
                            ::core::option::Option::Some(#input),
                            |__v| #fname(__v.take().expect(
                                "scaling: input slot was already empty - please report this bug"
                            )),
                        )
                    }
                }
                (None, Some(gen)) => {
                    quote! {
                        __adder.make_input(
                            __name,
                            {
                                let mut __gen = #gen;
                                move || ::core::option::Option::Some(__gen())
                            },
                            |__v| #fname(__v.take().expect(
                                "scaling: input slot was already empty - please report this bug"
                            )),
                        )
                    }
                }
                (None, None) => {
                    // No input declared, so the function must take none.
                    if !matches!(kind, Input::None) {
                        return Err(syn::Error::new(
                            func.sig.inputs.span(),
                            "this benchmark takes an input, so say where it comes from: \
                             `input = <value>` clones one per iteration, `make_input = \
                             <closure>` builds a fresh one",
                        ));
                    }
                    if repeatable {
                        // Setup runs once, lazily - on first call, which is
                        // also the first time `Adder::flat`'s own filter
                        // check has already passed, so a filtered-out
                        // benchmark never pays for it. `__action` stays a
                        // concrete (if unnameable) type the whole way
                        // through: `impl Trait` is not `Box<dyn Trait>`, so
                        // nothing here is a dynamic call - every call after
                        // the first is a direct call through the same
                        // monomorphized closure, exactly like any other
                        // benchmark's timed call.
                        quote! {
                            {
                                let mut __action = ::core::option::Option::None;
                                __adder.flat(__name, move || {
                                    (__action.get_or_insert_with(#fname))()
                                })
                            }
                        }
                    } else {
                        quote!(__adder.flat(__name, #fname))
                    }
                }
                (Some(_), Some(_)) => unreachable!("checked above"),
            };
            quote! {
                #[doc(hidden)]
                fn #shim(
                    __adder: &mut ::scaling::registry::Adder<'_, '_>,
                    __name: &str,
                ) -> ::scaling::registry::Handle<::scaling::Stats> {
                    #body
                }
                ::scaling::inventory::submit! {
                    ::scaling::registry::Registered {
                        name: #name,
                        crate_name: ::core::env!("CARGO_PKG_NAME"),
                        crate_version: ::core::env!("CARGO_PKG_VERSION"),
                        group: ::core::option::Option::None,
                        is_baseline: false,
                        kind: ::scaling::registry::Kind::Flat(#shim),
                    }
                }
            }
        }
        (None, Flavour::Scaling) => {
            let nmin = args.nmin.as_ref().ok_or_else(|| {
                syn::Error::new(
                    func.sig.span(),
                    "a scaling benchmark needs `nmin = <literal>`, the size to start \
                     climbing from",
                )
            })?;
            let repeatable = returns_repeatable_closure(&func.sig);
            if repeatable == Repeatable::OneArg {
                return Err(syn::Error::new(
                    func.sig.span(),
                    "a setup function whose returned closure takes an argument isn't \
                     supported - whatever that argument's own cost of generating a \
                     fresh value doesn't matter, so build the state once behind an \
                     `Arc`, clone it inside a `make_input` closure alongside a fresh \
                     per-call value as an ordinary tuple, and let this benchmark take \
                     that tuple like any other input; if instead the argument's own \
                     generation *is* part of what you want measured, keep it out of \
                     the closure's signature and mutate captured state inside the \
                     closure body instead - `impl Fn()/FnMut() -> O`, not `impl \
                     Fn(K)/FnMut(K) -> O`",
                ));
            }
            if repeatable == Repeatable::NoArg && args.make_input.is_some() {
                return Err(syn::Error::new(
                    func.sig.span(),
                    "this setup function's returned closure takes no argument, so \
                     `make_input`'s value - rebuilt fresh on every timed call, unlike \
                     setup itself - has nowhere to go. Drop `make_input` if the setup \
                     function alone (built from `n`) is everything the benchmark \
                     needs; if the input it builds needs to persist across calls at \
                     the same size, cache it inside `make_input`'s own closure - see \
                     the module docs for the pattern",
                ));
            }
            let body = match &args.make_input {
                // Wrapped for the same reason as the flat case above.
                Some(gen) if !matches!(input_kind(&func)?, Input::Owned(_)) => {
                    quote!(__adder.scaling_gen(__name, #gen, |__v| #fname(__v), #nmin))
                }
                Some(gen) => {
                    quote! {
                        __adder.scaling_gen(
                            __name,
                            {
                                let mut __gen = #gen;
                                move |__n: usize| ::core::option::Option::Some(__gen(__n))
                            },
                            |__v| #fname(__v.take().expect(
                                "scaling: input slot was already empty - please report this bug"
                            )),
                            #nmin,
                        )
                    }
                }
                None if repeatable == Repeatable::NoArg => {
                    quote! {
                        {
                            let mut __cache: ::std::collections::HashMap<usize, _> =
                                ::std::collections::HashMap::new();
                            __adder.scaling(__name, move |__n: usize| {
                                (__cache.entry(__n).or_insert_with(|| #fname(__n)))()
                            }, #nmin)
                        }
                    }
                }
                None => quote!(__adder.scaling(__name, #fname, #nmin)),
            };
            quote! {
                #[doc(hidden)]
                fn #shim(
                    __adder: &mut ::scaling::registry::Adder<'_, '_>,
                    __name: &str,
                ) -> ::scaling::registry::Handle<::scaling::ScalingStats> {
                    #body
                }
                ::scaling::inventory::submit! {
                    ::scaling::registry::Registered {
                        name: #name,
                        crate_name: ::core::env!("CARGO_PKG_NAME"),
                        crate_version: ::core::env!("CARGO_PKG_VERSION"),
                        group: ::core::option::Option::None,
                        is_baseline: false,
                        kind: ::scaling::registry::Kind::Scaling(#shim),
                    }
                }
            }
        }
    };

    Ok(quote! {
        // The input argument's type is not a borrow chosen for convenience,
        // it is the declared input type the registry keys the benchmark on -
        // so `clippy::ptr_arg`'s advice to take `&mut [T]` instead of
        // `&mut Vec<T>` would change what is being registered. Every caller
        // writing an input benchmark would otherwise meet that warning on
        // correct code.
        #[allow(clippy::ptr_arg)]
        #func
        #registration
    })
}

fn expand_bench_input(args: Args, func: ItemFn) -> syn::Result<TokenStream2> {
    let group = args.group.as_ref().ok_or_else(|| {
        syn::Error::new(
            func.sig.span(),
            "`#[scaling::bench_input]` declares the shared input of a comparison group, \
             so it needs `group = \"...\"` to say which",
        )
    })?;
    let ty = match &func.sig.output {
        ReturnType::Type(_, ty) => (**ty).clone(),
        ReturnType::Default => {
            return Err(syn::Error::new(
                func.sig.span(),
                "an input generator has to return the input it generates",
            ))
        }
    };
    if !func.sig.inputs.is_empty() {
        return Err(syn::Error::new(
            func.sig.inputs.span(),
            "an input generator takes no arguments: it is called once per round to \
             build the input the group shares",
        ));
    }
    let fname = &func.sig.ident;
    let shim = format_ident!("__scaling_gen_{}", fname);
    let ty_name = type_name(&ty);
    Ok(quote! {
        #func
        #[doc(hidden)]
        fn #shim() -> ::scaling::registry::ErasedInput {
            ::scaling::registry::ErasedInput::new(#fname())
        }
        ::scaling::inventory::submit! {
            ::scaling::registry::BenchInputRegistration {
                group: #group,
                crate_name: ::core::env!("CARGO_PKG_NAME"),
                crate_version: ::core::env!("CARGO_PKG_VERSION"),
                type_id: ::core::any::TypeId::of::<#ty>,
                type_name: #ty_name,
                make: #shim,
            }
        }
    })
}

/// Register one implementation of a matrix.
///
/// ```ignore
/// #[scaling::candidate(matrix = "sort", baseline)]
/// fn std_sort(v: &mut Vec<i32>) { v.sort() }
///
/// #[scaling::candidate(matrix = "sort")]
/// fn unstable(v: &mut Vec<i32>) { v.sort_unstable() }
/// ```
///
/// Candidates and inputs are registered independently and neither names the
/// other: a candidate says what type it takes, an input says what type it
/// makes, and every pairing of the two is measured. Adding an input is one
/// new function, and every candidate picks it up.
///
/// A matrix with two or more candidates of a type becomes one comparison per
/// input of that type - a comparison carries each candidate's own timing as
/// well as its difference from the baseline, so nothing is lost by always
/// comparing. `baseline` says which one the others are reported against; with
/// none marked the first by name is used, and the report says which it was.
///
/// `types(A, B, ...)` registers the same generic function once per listed
/// type, which is the one place monomorphisation has to be spelled out.
///
/// A candidate may also return `impl Fn()/FnMut() -> O` instead of `O`
/// directly - the same setup-once shape [`bench`] documents. Setup runs once
/// per (candidate, input) pairing this matrix measures, not once per timed
/// call. The matrix's own input machinery still regenerates - and, in a
/// comparison of two or more candidates, clones - a fresh input every round
/// regardless, same as for any other candidate; this saves the setup
/// function's own work on top of that, not the input machinery's.
#[proc_macro_attribute]
pub fn candidate(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as Args);
    let func = parse_macro_input!(item as ItemFn);
    expand_candidate(args, func)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Register one input of a matrix.
///
/// ```ignore
/// #[scaling::input(matrix = "sort", name = "reversed")]
/// fn reversed() -> Vec<i32> { (0..10_000).rev().collect() }
///
/// #[scaling::input(matrix = "sort", sizes(100, 10_000))]
/// fn random(n: usize) -> Vec<i32> { random_of_len(n) }
/// ```
///
/// With `sizes(..)` the function takes the size and is registered once per
/// listed size, named `fn_name@size` - which covers wanting the same input
/// large and small without writing it twice.
#[proc_macro_attribute]
pub fn input(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as Args);
    let func = parse_macro_input!(item as ItemFn);
    expand_input(args, func)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_candidate(args: Args, func: ItemFn) -> syn::Result<TokenStream2> {
    let matrix = args.matrix.as_ref().ok_or_else(|| {
        syn::Error::new(
            func.sig.span(),
            "`#[scaling::candidate]` needs `matrix = \"...\"` to say which matrix it is \
             one implementation of",
        )
    })?;
    let fname = &func.sig.ident;
    let reported = name_or_bare(&args.name, fname);
    let baseline = args.baseline;
    let kind = input_kind(&func)?;
    if let Some(e) = kind.owned_rejected(func.sig.inputs.span()) {
        return Err(e);
    }
    let declared = kind.ty().cloned();

    // One registration per listed type, or a single one at whatever the
    // signature says.
    struct Instantiation {
        name: TokenStream2,
        ty_id: TokenStream2,
        ty_name: TokenStream2,
        ty: Option<Type>,
    }
    let instantiations: Vec<Instantiation> = if args.types.is_empty() {
        let (ty_id, ty_name) = ty_id_and_name(declared.as_ref());
        vec![Instantiation {
            name: reported.clone(),
            ty_id,
            ty_name,
            ty: declared.clone(),
        }]
    } else {
        if declared.is_none() {
            return Err(syn::Error::new(
                func.sig.span(),
                "`types(..)` lists the types to instantiate this at, so it has to take \
                     an input of the type being varied",
            ));
        }
        args.types
            .iter()
            .map(|ty| {
                let (ty_id, ty_name) = ty_id_and_name(Some(ty));
                Instantiation {
                    name: reported.clone(),
                    ty_id,
                    ty_name,
                    ty: Some(ty.clone()),
                }
            })
            .collect()
    };

    let mut out = quote! {
        #[allow(clippy::ptr_arg)]
        #func
    };
    for (
        n,
        Instantiation {
            name,
            ty_id,
            ty_name,
            ty,
        },
    ) in instantiations.into_iter().enumerate()
    {
        let flat = format_ident!("__scaling_mflat_{}_{}", fname, n);
        let alt = format_ident!("__scaling_malt_{}_{}", fname, n);
        let call = call_expr(fname, ty.as_ref());
        // Same setup-once shape as an ordinary benchmark or a group member -
        // see `repeatable_call`. `#flat` and `#alt` are each called once per
        // (candidate, input) pairing (assembly builds a fresh one per lane),
        // so `__action`'s scope here is exactly one pairing's whole run, not
        // shared across others. The matrix's input is still regenerated (for
        // `#flat`) or regenerated-and-cloned (for `#alt`, one comparison's
        // shared input per round) every call regardless - only the setup
        // function's own work, not the matrix's own input machinery, is
        // what this saves.
        let (flat_body, alt_body) = if returns_repeatable_closure(&func.sig) == Repeatable::NoArg {
            let repeatable = repeatable_call(call);
            (
                quote! {
                    let mut __action = ::core::option::Option::None;
                    __adder.make_input(
                        __name,
                        __make,
                        move |__e: &mut ::scaling::registry::ErasedInput| #repeatable,
                    )
                },
                quote! {
                    let mut __action = ::core::option::Option::None;
                    __set.add(__name, move |__e: &mut ::scaling::registry::ErasedInput| #repeatable)
                },
            )
        } else {
            (
                quote! {
                    __adder.make_input(
                        __name,
                        __make,
                        |__e: &mut ::scaling::registry::ErasedInput| #call,
                    )
                },
                quote! {
                    __set.add(__name, |__e: &mut ::scaling::registry::ErasedInput| #call)
                },
            )
        };
        out.extend(quote! {
            #[doc(hidden)]
            fn #flat(
                __adder: &mut ::scaling::registry::Adder<'_, '_>,
                __name: &str,
                __make: fn() -> ::scaling::registry::ErasedInput,
            ) -> ::scaling::registry::Handle<::scaling::Stats> {
                #flat_body
            }
            #[doc(hidden)]
            fn #alt<'__s>(
                __set: ::scaling::registry::Alternative<'__s>,
                __name: &str,
            ) -> ::scaling::registry::Alternative<'__s> {
                #alt_body
            }
            ::scaling::inventory::submit! {
                ::scaling::registry::MatrixCandidate {
                    matrix: #matrix,
                    name: #name,
                    input_type: #ty_id,
                    input_type_name: #ty_name,
                    is_baseline: #baseline,
                    crate_name: ::core::env!("CARGO_PKG_NAME"),
                    crate_version: ::core::env!("CARGO_PKG_VERSION"),
                    add_flat: #flat,
                    add_alt: #alt,
                }
            }
        });
    }
    Ok(out)
}

fn expand_input(args: Args, func: ItemFn) -> syn::Result<TokenStream2> {
    let matrix = args.matrix.as_ref().ok_or_else(|| {
        syn::Error::new(
            func.sig.span(),
            "`#[scaling::input]` needs `matrix = \"...\"` to say which matrix it is an \
             input of",
        )
    })?;
    let ty = match &func.sig.output {
        ReturnType::Type(_, ty) => (**ty).clone(),
        ReturnType::Default => {
            return Err(syn::Error::new(
                func.sig.span(),
                "an input has to return the input it makes",
            ))
        }
    };
    let fname = &func.sig.ident;
    let ty_name = type_name(&ty);

    if args.sizes.is_empty() {
        if !func.sig.inputs.is_empty() {
            return Err(syn::Error::new(
                func.sig.inputs.span(),
                "an input takes no arguments unless it is registered at several `sizes(..)`, \
                 in which case it takes the size",
            ));
        }
        let name = name_or_bare(&args.name, fname);
        let shim = format_ident!("__scaling_minput_{}", fname);
        return Ok(quote! {
            #func
            #[doc(hidden)]
            fn #shim() -> ::scaling::registry::ErasedInput {
                ::scaling::registry::ErasedInput::new(#fname())
            }
            ::scaling::inventory::submit! {
                ::scaling::registry::MatrixInput {
                    matrix: #matrix,
                    name: #name,
                    crate_name: ::core::env!("CARGO_PKG_NAME"),
                    crate_version: ::core::env!("CARGO_PKG_VERSION"),
                    type_id: ::core::any::TypeId::of::<#ty>,
                    type_name: #ty_name,
                    make: #shim,
                }
            }
        });
    }

    if func.sig.inputs.len() != 1 {
        return Err(syn::Error::new(
            func.sig.span(),
            "an input registered at several `sizes(..)` takes the size as its one argument",
        ));
    }
    // `name = "..."` overrides the bare function name here exactly as it
    // does in the no-`sizes` branch above - previously only that branch
    // honored it, so `#[scaling::input(name = "...", sizes(..))]` silently
    // registered under the function's own name instead.
    let bare = args
        .name
        .as_ref()
        .map(|lit| lit.value())
        .unwrap_or_else(|| fname.to_string());
    let mut out = quote! { #func };
    for (n, size) in args.sizes.iter().enumerate() {
        let shim = format_ident!("__scaling_minput_{}_{}", fname, n);
        let size_txt = size.base10_digits().to_string();
        let name = format!("{bare}@{size_txt}");
        out.extend(quote! {
            #[doc(hidden)]
            fn #shim() -> ::scaling::registry::ErasedInput {
                ::scaling::registry::ErasedInput::new(#fname(#size))
            }
            ::scaling::inventory::submit! {
                ::scaling::registry::MatrixInput {
                    matrix: #matrix,
                    name: #name,
                    crate_name: ::core::env!("CARGO_PKG_NAME"),
                    crate_version: ::core::env!("CARGO_PKG_VERSION"),
                    type_id: ::core::any::TypeId::of::<#ty>,
                    type_name: #ty_name,
                    make: #shim,
                }
            }
        });
    }
    Ok(out)
}
