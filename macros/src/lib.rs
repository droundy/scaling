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
/// #[scaling::bench(gen_input = || random_vec(1000))]
/// fn sort(v: &mut Vec<i32>) { v.sort() }
///
/// #[scaling::bench(gen_input = || random_vec(1000))]
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
/// of `gen_input` - it costs nothing extra to have. `impl Trait` in return
/// position is a concrete type the compiler already knows at the call
/// site, not a boxed one, so the generated shim holds it in a plain
/// `Option` and calls it directly: no allocation, no dynamic dispatch, on
/// any call, ever. Measured against the `thread_local!`-based workaround
/// this shape replaces, it comes out faster, not merely equivalent - the
/// workaround pays for a thread-local lookup on every call that this does
/// not.
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
/// `gen_input =` on each member - a group compiled with either of those is
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
/// `gen_input = |n: usize| -> I { ... }` moves that cost out of the timed
/// region, the same idea as `#[bench(gen_input = ...)]` but handed the size
/// so it can build an input of exactly that size:
///
/// ```ignore
/// #[scaling::bench_scaling(nmin = 8, gen_input = |n: usize| random_n(n))]
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
/// per-benchmark `gen_input = ...` argument `#[bench]` itself takes - the
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
/// take `input = ...` or `gen_input = ...`: an alternative's input always
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
    gen_input: Option<Expr>,
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
                "gen_input" => {
                    input.parse::<syn::Token![=]>()?;
                    args.gen_input = Some(input.parse()?);
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
                             name, group, matrix, baseline, input, gen_input, \
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

/// Whether a zero-argument function's return type is `impl Fn() -> O` or
/// `impl FnMut() -> O` - the "run setup once, then call the result
/// repeatedly" shape `#[bench]` recognizes there.
///
/// Deliberately narrow: only a zero-argument `Fn`/`FnMut` bound counts,
/// not `impl Fn(X) -> O` (a different shape entirely, not handled here)
/// and not `impl FnOnce() -> O` (which cannot be called more than the one
/// time a benchmark's whole point is to avoid).
fn returns_repeatable_closure(sig: &syn::Signature) -> bool {
    let syn::ReturnType::Type(_, ty) = &sig.output else {
        return false;
    };
    let Type::ImplTrait(imp) = &**ty else {
        return false;
    };
    imp.bounds.iter().any(|bound| {
        let syn::TypeParamBound::Trait(trait_bound) = bound else {
            return false;
        };
        let Some(last) = trait_bound.path.segments.last() else {
            return false;
        };
        (last.ident == "Fn" || last.ident == "FnMut")
            && matches!(
                &last.arguments,
                syn::PathArguments::Parenthesized(p) if p.inputs.is_empty()
            )
    })
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
             sweeps `n` - use `gen_input = |n: usize| ...` to build a size-dependent \
             input instead",
        ));
    }
    if args.input.is_some() && args.gen_input.is_some() {
        return Err(syn::Error::new(
            func.sig.span(),
            "give either `input` or `gen_input`, not both: one clones a value per \
             iteration, the other builds a fresh one",
        ));
    }
    // `Flavour::Flat` only: a scaling benchmark in a group is rejected below,
    // for its own, more specific reason, regardless of `input`/`gen_input`.
    if let Flavour::Flat = flavour {
        if args.group.is_some() && (args.input.is_some() || args.gen_input.is_some()) {
            let span = args
                .input
                .as_ref()
                .map(|e| e.span())
                .or_else(|| args.gen_input.as_ref().map(|e| e.span()))
                .unwrap_or_else(|| func.sig.span());
            return Err(syn::Error::new(
                span,
                "a comparison group's alternatives share one input, declared once with \
                 `#[scaling::bench_input(group = \"...\")]` - not `input =` or \
                 `gen_input =` on each member, which would give every alternative its \
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
            quote! {
                #[doc(hidden)]
                fn #shim<'__s>(
                    __set: ::scaling::registry::Alternative<'__s>,
                    __name: &str,
                ) -> ::scaling::registry::Alternative<'__s> {
                    __set.add(__name, |__e: &mut ::scaling::registry::ErasedInput| #call)
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
            let body = match (&args.input, &args.gen_input) {
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
                    quote!(__adder.gen_input(__name, #gen, |__v| #fname(__v)))
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
                        __adder.gen_input(
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
                             `input = <value>` clones one per iteration, `gen_input = \
                             <closure>` builds a fresh one",
                        ));
                    }
                    if returns_repeatable_closure(&func.sig) {
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
            let body = match &args.gen_input {
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
        out.extend(quote! {
            #[doc(hidden)]
            fn #flat(
                __adder: &mut ::scaling::registry::Adder<'_, '_>,
                __name: &str,
                __make: fn() -> ::scaling::registry::ErasedInput,
            ) -> ::scaling::registry::Handle<::scaling::Stats> {
                __adder.gen_input(
                    __name,
                    __make,
                    |__e: &mut ::scaling::registry::ErasedInput| #call,
                )
            }
            #[doc(hidden)]
            fn #alt<'__s>(
                __set: ::scaling::registry::Alternative<'__s>,
                __name: &str,
            ) -> ::scaling::registry::Alternative<'__s> {
                __set.add(__name, |__e: &mut ::scaling::registry::ErasedInput| #call)
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
