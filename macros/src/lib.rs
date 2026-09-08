//! Attribute macros that register benchmarks for the `scaling` crate.
//!
//! These are re-exported by `scaling` itself and are not meant to be depended
//! on directly. See `REGISTRATION.md` in that crate for the design.
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

/// Register a benchmark, as [`scaling::bench`] would run it.
///
/// ```ignore
/// #[scaling::bench]
/// fn fib_200() -> usize { fib(200) }
///
/// #[scaling::bench(input = vec![0u8; 1024])]
/// fn hash(buf: &mut Vec<u8>) -> u64 { hash_of(buf) }
///
/// #[scaling::bench(gen_input = || random_vec(1000))]
/// fn sort(v: &mut Vec<i32>) { v.sort() }
/// ```
///
/// Add `group = "name"` to make this one alternative of a comparison, and
/// `baseline` on exactly one member of each group to say which the others
/// are reported against - registrations have no order, so it cannot be the
/// first one added as it is for a hand-built `ComparisonSet`. `name = "..."`
/// overrides the reported name, which defaults to the module-qualified path
/// of the function.
#[proc_macro_attribute]
pub fn bench(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as Args);
    let func = parse_macro_input!(item as ItemFn);
    expand(args, func, Flavour::Flat)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Register a scaling benchmark, as [`scaling::bench_scaling`] would run it.
///
/// ```ignore
/// #[scaling::bench_scaling(nmin = 100)]
/// fn sort_n(n: usize) -> Vec<i32> { let mut v = random_n(n); v.sort(); v }
/// ```
///
/// `nmin` must be a literal: it is baked into the generated shim, whose
/// signature has nowhere to pass it.
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
/// ```ignore
/// #[scaling::gen_input(group = "sort")]
/// fn sort_data() -> Vec<i32> { random_vec(1000) }
/// ```
///
/// Called once per round, and the value cloned for each alternative, so that
/// all of them are measured on the same input - which is what makes their
/// differences paired. Exactly one per group.
#[proc_macro_attribute]
pub fn gen_input(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as Args);
    let func = parse_macro_input!(item as ItemFn);
    expand_gen_input(args, func)
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
    baseline: bool,
    input: Option<Expr>,
    gen_input: Option<Expr>,
    nmin: Option<LitInt>,
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
                             name, group, baseline, input, gen_input, nmin",
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
fn input_type(func: &ItemFn) -> syn::Result<Option<Type>> {
    let mut args = func.sig.inputs.iter();
    let first = match args.next() {
        None => return Ok(None),
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
        // The input is passed as `&mut I`, matching `bench_input`.
        Type::Reference(r) if r.mutability.is_some() => Ok(Some((*r.elem).clone())),
        other => Err(syn::Error::new(
            other.span(),
            "a benchmark's input argument must be `&mut I`, as `bench_input` takes it",
        )),
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
    if args.input.is_some() && args.gen_input.is_some() {
        return Err(syn::Error::new(
            func.sig.span(),
            "give either `input` or `gen_input`, not both: one clones a value per \
             iteration, the other builds a fresh one",
        ));
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
            let ity = input_type(&func)?;
            let baseline = args.baseline;
            // The call, and the type the group's input must have.
            let (call, ty_id, ty_name) = match &ity {
                Some(ty) => (
                    quote!(#fname(__e.get_mut::<#ty>())),
                    quote!(::core::any::TypeId::of::<#ty>),
                    quote!(::core::stringify!(#ty)),
                ),
                None => (
                    quote!(#fname()),
                    quote!(::core::any::TypeId::of::<()>),
                    quote!("()"),
                ),
            };
            quote! {
                #[doc(hidden)]
                fn #shim<'__s>(
                    __set: ::scaling::ComparisonSet<'__s, ::scaling::registry::ErasedInput>,
                    __name: &str,
                ) -> ::scaling::ComparisonSet<'__s, ::scaling::registry::ErasedInput> {
                    __set.add_input(__name, |__e: &mut ::scaling::registry::ErasedInput| #call)
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
            let body = match (&args.input, &args.gen_input) {
                (Some(input), None) => quote!(__suite.add_input(__name, #input, #fname)),
                (None, Some(gen)) => quote!(__suite.add_gen_input(__name, #gen, #fname)),
                (None, None) => {
                    // No input declared, so the function must take none.
                    if input_type(&func)?.is_some() {
                        return Err(syn::Error::new(
                            func.sig.inputs.span(),
                            "this benchmark takes an input, so say where it comes from: \
                             `input = <value>` clones one per iteration, `gen_input = \
                             <closure>` builds a fresh one",
                        ));
                    }
                    quote!(__suite.add(__name, #fname))
                }
                (Some(_), Some(_)) => unreachable!("checked above"),
            };
            quote! {
                #[doc(hidden)]
                fn #shim(
                    __suite: &mut ::scaling::Suite<'_>,
                    __cfg: &::scaling::Config,
                    __name: &str,
                ) -> ::scaling::Token<::scaling::Stats> {
                    let _ = __cfg;
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
                Some(gen) => quote!(__suite.add_scaling_gen(__name, #gen, #fname, #nmin)),
                None => quote!(__suite.add_scaling(__name, #fname, #nmin)),
            };
            quote! {
                #[doc(hidden)]
                fn #shim(
                    __suite: &mut ::scaling::Suite<'_>,
                    __cfg: &::scaling::Config,
                    __name: &str,
                ) -> ::scaling::Token<::scaling::ScalingStats> {
                    let _ = __cfg;
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

fn expand_gen_input(args: Args, func: ItemFn) -> syn::Result<TokenStream2> {
    let group = args.group.as_ref().ok_or_else(|| {
        syn::Error::new(
            func.sig.span(),
            "`#[scaling::gen_input]` declares the shared input of a comparison group, \
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
    Ok(quote! {
        #func
        #[doc(hidden)]
        fn #shim() -> ::scaling::registry::ErasedInput {
            ::scaling::registry::ErasedInput::new(#fname())
        }
        ::scaling::inventory::submit! {
            ::scaling::registry::GenInputRegistration {
                group: #group,
                type_id: ::core::any::TypeId::of::<#ty>,
                type_name: ::core::stringify!(#ty),
                make: #shim,
            }
        }
    })
}
