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

/// Register a benchmark.
#[proc_macro_attribute]
pub fn bench(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as Args);
    let func = parse_macro_input!(item as ItemFn);
    expand(args, func, Flavour::Flat)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Register a scaling benchmark.
#[proc_macro_attribute]
pub fn bench_scaling(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as Args);
    let func = parse_macro_input!(item as ItemFn);
    expand(args, func, Flavour::Scaling)
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
    /// `group = "a"` or `group("a", "b", ...)`: every group this belongs
    /// to. Empty means none - a plain standalone registration.
    groups: Vec<LitStr>,
    baseline: bool,
    input: Option<Expr>,
    make_input: Option<Expr>,
    nmin: Option<LitInt>,
    /// `types(A, B)`: instantiate a generic candidate or input once per
    /// type.
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
                // Either `group = "a"` (the common, single-group case) or
                // `group("a", "b", ...)` (belongs to several at once) -
                // both accepted, so the common case stays as light as it
                // was before groups could be a list.
                "group" => {
                    if input.peek(syn::token::Paren) {
                        let inner;
                        syn::parenthesized!(inner in input);
                        let listed = syn::punctuated::Punctuated::<LitStr, syn::Token![,]>::parse_terminated(&inner)?;
                        if listed.is_empty() {
                            return Err(syn::Error::new(
                                key.span(),
                                "`group(..)` lists the groups this belongs to, so it needs at \
                                 least one",
                            ));
                        }
                        args.groups.extend(listed);
                    } else {
                        input.parse::<syn::Token![=]>()?;
                        args.groups.push(input.parse()?);
                    }
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
                             name, group, baseline, input, make_input, \
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

/// What a benchmark function's own signature declares its input to be.
///
/// `None` means it takes nothing, which the registry represents as `()` -
/// so a group whose candidates take nothing and a group whose candidates
/// take a generated input are one code path rather than two.
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

/// If `sig` is the `NoArg` setup-once shape - `impl Fn() -> O` or `impl
/// FnMut() -> O` - the `O` it produces, unwrapped from the closure bound
/// that names it. `None` otherwise, `OneArg` included: a caller wanting
/// [`Repeatable`] itself should ask [`returns_repeatable_closure`], this is
/// only for a caller that already knows it wants `NoArg` specifically and
/// needs the type inside it - [`expand_input`], where a non-generic input
/// function always takes no arguments, so there is no `OneArg` shape to
/// speak of.
fn repeatable_output(sig: &syn::Signature) -> Option<Type> {
    let syn::ReturnType::Type(_, ty) = &sig.output else {
        return None;
    };
    let Type::ImplTrait(imp) = &**ty else {
        return None;
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
        if !p.inputs.is_empty() {
            return None;
        }
        return match &p.output {
            syn::ReturnType::Type(_, ty) => Some((**ty).clone()),
            syn::ReturnType::Default => None,
        };
    }
    None
}

/// The rejection for a setup function whose returned closure itself takes an
/// argument (`Repeatable::OneArg`) - not supported anywhere it is checked,
/// so one message rather than one per call site.
fn one_arg_rejected(span: proc_macro2::Span) -> syn::Error {
    syn::Error::new(
        span,
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
    )
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

/// `|__v| #fname(__v.take().expect(...))`: the closure a shim uses when the
/// benchmark consumes its input by value. `Suite`/`InputGroup` only ever
/// hand out `&mut I`, so the stored value is wrapped in `Option<I>` and taken
/// out of it right before the call - exactly the trick this crate's own
/// documentation would otherwise have to tell a caller to write by hand.
/// `.take()` can only ever see `None` if something else already emptied this
/// slot, which nothing does: each slot is visited once per round, by this
/// closure alone.
fn take_and_call(fname: &syn::Ident) -> TokenStream2 {
    quote! {
        |__v| #fname(__v.take().expect(
            "scaling: input slot was already empty - please report this bug"
        ))
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
    // A scaling benchmark measures how one function grows with N, which is
    // not something a comparison compares - so it never takes a group,
    // regardless of `input`/`make_input`.
    if let (Flavour::Scaling, Some(g)) = (&flavour, args.groups.first()) {
        return Err(syn::Error::new(
            g.span(),
            "a scaling benchmark measures how one function grows with N, which is \
             not something a comparison compares",
        ));
    }
    if !args.groups.is_empty() && (args.input.is_some() || args.make_input.is_some()) {
        let span = args
            .input
            .as_ref()
            .map(|e| e.span())
            .or_else(|| args.make_input.as_ref().map(|e| e.span()))
            .unwrap_or_else(|| func.sig.span());
        return Err(syn::Error::new(
            span,
            "a comparison's candidates share their input with whatever \
             `#[scaling::input(group = \"...\")]` names the same group - not \
             `input =` or `make_input =` here, which would give this one its own \
             input and break the pairing a comparison's accuracy depends on",
        ));
    }
    if args.baseline && args.groups.is_empty() {
        return Err(syn::Error::new(
            func.sig.span(),
            "`baseline` says which candidate of a comparison the others are \
             reported against, so it needs a `group` to be the baseline of",
        ));
    }

    // `Flavour::Scaling` was already rejected above, so any group here
    // belongs to a `#[bench]`, which is a candidate rather than a
    // standalone registration.
    if !args.groups.is_empty() {
        return expand_candidate(args, func);
    }

    let name = reported_name(&args, &func);
    let fname = &func.sig.ident;
    let shim = format_ident!("__scaling_shim_{}", fname);

    let registration = match flavour {
        Flavour::Flat => {
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
                return Err(one_arg_rejected(func.sig.span()));
            }
            let body = match (&args.input, &args.make_input) {
                // The setup function itself runs once: `input` is given to
                // it there, not cloned/regenerated per call the way
                // `Suite::add_input`/`add_make_input` would. Routing this
                // through `Suite::add_input` instead would still type-check - the lazy
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
                            __adder.add(__name, move || {
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
                            __adder.add(__name, move || {
                                (__action.get_or_insert_with(|| #fname(&mut __input)))()
                            })
                        }
                    }
                }
                // `|__v| #fname(__v)`, not `#fname` passed directly: `Suite`
                // always hands out `&mut I`, and a named function's own type
                // does not satisfy a generic `FnMut(&mut I)` bound merely
                // because Rust would reborrow `&mut I` as `&I` at an
                // ordinary call site - that coercion only applies to an
                // actual call expression, which this closure body gives it.
                (Some(input), None) if !owned => {
                    quote!(__adder.add_input(__name, #input, |__v| #fname(__v)))
                }
                (None, Some(gen)) if !owned => {
                    quote!(__adder.add_make_input(__name, #gen, |__v| #fname(__v)))
                }
                // The function consumes its input, so `Suite` - which only
                // ever hands out `&mut I` - cannot call it directly. See
                // `take_and_call` for the `Option`/`.take()` trick that
                // works around that.
                (Some(input), None) => {
                    let take = take_and_call(fname);
                    quote! {
                        __adder.add_input(
                            __name,
                            ::core::option::Option::Some(#input),
                            #take,
                        )
                    }
                }
                (None, Some(gen)) => {
                    let take = take_and_call(fname);
                    quote! {
                        __adder.add_make_input(
                            __name,
                            {
                                let mut __gen = #gen;
                                move || ::core::option::Option::Some(__gen())
                            },
                            #take,
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
                        // `__action` stays a
                        // concrete (if unnameable) type the whole way
                        // through: `impl Trait` is not `Box<dyn Trait>`, so
                        // nothing here is a dynamic call - every call after
                        // the first is a direct call through the same
                        // monomorphized closure, exactly like any other
                        // benchmark's timed call.
                        quote! {
                            {
                                let mut __action = ::core::option::Option::None;
                                __adder.add(__name, move || {
                                    (__action.get_or_insert_with(#fname))()
                                })
                            }
                        }
                    } else {
                        quote!(__adder.add(__name, #fname))
                    }
                }
                (Some(_), Some(_)) => unreachable!("checked above"),
            };
            quote! {
                #[doc(hidden)]
                fn #shim(
                    __adder: &mut ::scaling::registry::Suite,
                    __name: &str,
                ) {
                    #body;
                }
                ::scaling::inventory::submit! {
                    ::scaling::registry::Registered {
                        name: #name,
                        crate_name: ::core::env!("CARGO_PKG_NAME"),
                        crate_version: ::core::env!("CARGO_PKG_VERSION"),
                        kind: ::scaling::registry::Kind::Flat(#shim),
                    }
                }
            }
        }
        Flavour::Scaling => {
            let nmin = args.nmin.as_ref().ok_or_else(|| {
                syn::Error::new(
                    func.sig.span(),
                    "a scaling benchmark needs `nmin = <literal>`, the size to start \
                     climbing from",
                )
            })?;
            let repeatable = returns_repeatable_closure(&func.sig);
            if repeatable == Repeatable::OneArg {
                return Err(one_arg_rejected(func.sig.span()));
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
                    quote!(__adder.add_scaling_gen(__name, #gen, |__v| #fname(__v), #nmin))
                }
                Some(gen) => {
                    let take = take_and_call(fname);
                    quote! {
                        __adder.add_scaling_gen(
                            __name,
                            {
                                let mut __gen = #gen;
                                move |__n: usize| ::core::option::Option::Some(__gen(__n))
                            },
                            #take,
                            #nmin,
                        )
                    }
                }
                None if repeatable == Repeatable::NoArg => {
                    quote! {
                        {
                            let mut __cache: ::std::collections::HashMap<usize, _> =
                                ::std::collections::HashMap::new();
                            __adder.add_scaling(__name, move |__n: usize| {
                                (__cache.entry(__n).or_insert_with(|| #fname(__n)))()
                            }, #nmin)
                        }
                    }
                }
                None => quote!(__adder.add_scaling(__name, #fname, #nmin)),
            };
            quote! {
                #[doc(hidden)]
                fn #shim(
                    __adder: &mut ::scaling::registry::Suite,
                    __name: &str,
                ) {
                    #body;
                }
                ::scaling::inventory::submit! {
                    ::scaling::registry::Registered {
                        name: #name,
                        crate_name: ::core::env!("CARGO_PKG_NAME"),
                        crate_version: ::core::env!("CARGO_PKG_VERSION"),
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

/// Register one input, shared by every candidate of a matching type in one
/// or more of the named groups.
#[proc_macro_attribute]
pub fn input(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as Args);
    let func = parse_macro_input!(item as ItemFn);
    expand_input(args, func)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// The `&'static [&'static str]` expression naming every group a
/// registration belongs to - `groups` is checked non-empty by every caller
/// before this is reached.
fn groups_array(groups: &[LitStr]) -> TokenStream2 {
    quote!(&[#(#groups),*])
}

/// A `#[bench]` naming one or more groups - see `expand`, which dispatches
/// here once it has confirmed as much.
fn expand_candidate(args: Args, func: ItemFn) -> syn::Result<TokenStream2> {
    let groups = groups_array(&args.groups);
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
        let alt = format_ident!("__scaling_malt_{}_{}", fname, n);
        let call = call_expr(fname, ty.as_ref());
        let alt_body = if returns_repeatable_closure(&func.sig) == Repeatable::NoArg {
            let repeatable = repeatable_call(call);
            quote! {
                let mut __action = ::core::option::Option::None;
                __set.add_input(__name, move |__e: &mut ::scaling::registry::ErasedInput| #repeatable)
            }
        } else {
            quote!(__set.add_input(__name, |__e: &mut ::scaling::registry::ErasedInput| #call))
        };
        out.extend(quote! {
            #[doc(hidden)]
            fn #alt<'__s>(
                __set: ::scaling::registry::InputGroup<'__s, ::scaling::registry::ErasedInput>,
                __name: &str,
            ) -> ::scaling::registry::InputGroup<'__s, ::scaling::registry::ErasedInput> {
                #alt_body
            }
            ::scaling::inventory::submit! {
                ::scaling::registry::Candidate {
                    groups: #groups,
                    name: #name,
                    input_type: #ty_id,
                    input_type_name: #ty_name,
                    is_baseline: #baseline,
                    crate_name: ::core::env!("CARGO_PKG_NAME"),
                    crate_version: ::core::env!("CARGO_PKG_VERSION"),
                    add_alt: #alt,
                }
            }
        });
    }
    Ok(out)
}

fn expand_input(args: Args, func: ItemFn) -> syn::Result<TokenStream2> {
    if args.groups.is_empty() {
        return Err(syn::Error::new(
            func.sig.span(),
            "`#[scaling::input]` needs `group = \"...\"` (or `group(\"a\", \"b\", ...)`) \
             to say which group or groups it feeds",
        ));
    }
    if !args.types.is_empty() && !args.sizes.is_empty() {
        return Err(syn::Error::new(
            func.sig.span(),
            "give either `types(..)` or `sizes(..)`, not both - one instantiates a \
             generic input once per type, the other registers a fixed input once per \
             size",
        ));
    }
    let repeatable = returns_repeatable_closure(&func.sig);
    if repeatable == Repeatable::OneArg {
        return Err(one_arg_rejected(func.sig.span()));
    }
    if !args.types.is_empty() && repeatable == Repeatable::NoArg {
        return Err(syn::Error::new(
            func.sig.span(),
            "the setup-once shape (`impl Fn()/FnMut() -> T`) is not yet supported \
             together with `types(..)` - write a plain `-> T` input, or register one \
             non-generic `#[scaling::input]` per type instead",
        ));
    }
    match (args.sizes.is_empty(), func.sig.inputs.len()) {
        (true, 0) => {}
        (true, _) => {
            return Err(syn::Error::new(
                func.sig.inputs.span(),
                "an input takes no arguments, unless it is registered at several \
                 `sizes(..)`, in which case it takes the size",
            ))
        }
        (false, 1) => {}
        (false, _) => {
            return Err(syn::Error::new(
                func.sig.span(),
                "an input registered at several `sizes(..)` takes the size as its one \
                 argument",
            ))
        }
    }

    let groups = groups_array(&args.groups);
    let fname = &func.sig.ident;
    // `name = "..."` overrides the bare function name, the same default
    // every registration derived from this crate uses.
    let bare = args
        .name
        .as_ref()
        .map(|lit| lit.value())
        .unwrap_or_else(|| fname.to_string());

    // The type a non-generic instantiation produces - unused by a
    // `types(..)` instantiation, which already has its own listed type and,
    // being generic, cannot be read off this function's own signature.
    let base_ty: Type = if repeatable == Repeatable::NoArg {
        repeatable_output(&func.sig).expect("just matched Repeatable::NoArg")
    } else {
        match &func.sig.output {
            ReturnType::Type(_, ty) => (**ty).clone(),
            ReturnType::Default => {
                return Err(syn::Error::new(
                    func.sig.span(),
                    "an input has to return the input it makes",
                ))
            }
        }
    };

    // What each instantiation is called, the type it produces, and the
    // expression that calls the underlying function - directly for a plain
    // input, or to build the setup-once closure once for the setup-once
    // shape (see below).
    struct Instantiation {
        name: TokenStream2,
        ty: Type,
        call: TokenStream2,
    }
    let instantiations: Vec<Instantiation> = if !args.types.is_empty() {
        // Turbofish, not argument-type inference: unlike a candidate, which
        // infers its type parameter from the erased input it is handed, an
        // input generator takes nothing to infer from.
        args.types
            .iter()
            .map(|ty| Instantiation {
                name: quote!(#bare),
                ty: ty.clone(),
                call: quote!(#fname::<#ty>()),
            })
            .collect()
    } else if !args.sizes.is_empty() {
        args.sizes
            .iter()
            .map(|size| {
                let size_txt = size.base10_digits().to_string();
                let name = format!("{bare}@{size_txt}");
                Instantiation {
                    name: quote!(#name),
                    ty: base_ty.clone(),
                    call: quote!(#fname(#size)),
                }
            })
            .collect()
    } else {
        vec![Instantiation {
            name: quote!(#bare),
            ty: base_ty.clone(),
            call: quote!(#fname()),
        }]
    };

    let mut out = quote! { #func };
    for (n, Instantiation { name, ty, call }) in instantiations.into_iter().enumerate() {
        let shim = format_ident!("__scaling_input_{}_{}", fname, n);
        let ty_name = type_name(&ty);
        // The setup-once shape: since a plain `fn` (what this generator is,
        // once registered) has no closure environment of its own to hold
        // state in - unlike `make_input = <closure>` on an ordinary
        // benchmark, which is user code free to capture whatever it wants -
        // the persistent state lives in a `thread_local!` inside the shim
        // instead, checked and populated the first time this particular
        // instantiation's generator runs and reused every round after.
        // Bounded, cheap bookkeeping: a `RefCell` check once per round, not
        // once per timed call - `#call` itself still only runs once, ever.
        let shim_body = if repeatable == Repeatable::NoArg {
            quote! {
                #[doc(hidden)]
                fn #shim() -> ::scaling::registry::ErasedInput {
                    ::std::thread_local! {
                        static __ACTION: ::core::cell::RefCell<
                            ::core::option::Option<::std::boxed::Box<dyn FnMut() -> #ty>>
                        > = ::core::cell::RefCell::new(::core::option::Option::None);
                    }
                    __ACTION.with(|__cell| {
                        let mut __guard = __cell.borrow_mut();
                        let __action = __guard.get_or_insert_with(|| ::std::boxed::Box::new(#call));
                        ::scaling::registry::ErasedInput::new(__action())
                    })
                }
            }
        } else {
            quote! {
                #[doc(hidden)]
                fn #shim() -> ::scaling::registry::ErasedInput {
                    ::scaling::registry::ErasedInput::new(#call)
                }
            }
        };
        out.extend(quote! {
            #shim_body
            ::scaling::inventory::submit! {
                ::scaling::registry::Input {
                    groups: #groups,
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
