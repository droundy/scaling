# Distributed benchmark registration

Working design for letting benchmarks be *defined anywhere* in a crate,
using [`inventory`], rather than hand-assembled into one `Suite` at one call
site.

[`inventory`]: https://docs.rs/inventory

## The problem

Every benchmark must currently be wired up in one place: build a `Config`,
call `cfg.suite()`, chain `.add()` / `.add_input()` / `.add_scaling()` /
`.add_comparison()`, call `suite.run()`, print the `Report`. There is no
trait to implement — a benchmark is just a closure — and no discovery
mechanism. Benchmarks therefore cannot live next to the code they measure.

`inventory` provides distributed static registration: any `'static` item can
be submitted from anywhere in a crate via `inventory::submit!` and collected
later via `inventory::iter`, with no central list.

## Two designs, and why the choice is deferred

**Design B ("hybrid")** — registration becomes *one more way* to fill a
`Suite`, via a new `Suite::add_registered()`. Everything existing keeps
working; you still write your own `main()` and print the `Report` yourself.

**Design A ("go all in")** — registration is the *only* way to define a
benchmark, and `scaling` owns `main()`, the CLI, and all output formatting.
The manual builder is demoted or removed.

The important structural point: **B is a strict waypoint on the way to A.**
Both need the same registry types, the same attribute macros, the same
assembly logic, the same matrix support. A adds a runner that owns output,
and removes the manual API. So there is no need to choose now — build the
shared substrate, and the choice becomes "do we also build the runner and
delete the builder?", answerable with real code in hand.

## Staging: additive first

Every stage below through stage 5 is **purely additive**. Nothing existing
changes behaviour, no public API is removed, and each stage is independently
revertible. Internal simplifications come afterwards, motivated by what the
registry actually turned out to need.

An earlier draft of this plan had it backwards, opening with two internal
refactors (removing the Bonferroni `Plan`/`Drop`, and adding per-benchmark
`Config` support to `Suite`) on the theory that the registry needed them.
**It does not.** Registry-discovered comparisons go through
`Suite::add_comparison`, which already counts them against the existing
plan, so the registry composes with the current machinery untouched. The
only thing that genuinely needs `Suite::add_*_with` is per-benchmark
accuracy *attributes*, which are a refinement — so they and the attributes
that drive them are deferred together to stage 6.

| stage | what | additive? |
| --- | --- | --- |
| 1 | `registry` module: descriptor types, `ErasedInput`, `collect!` | yes |
| 2 | assembly (`plan()`), validated with hand-written `submit!` | yes |
| 3 | `Suite::add_registered()` — this completes **Design B** | yes |
| 4 | `scaling-macros`: the attribute proc macros | yes |
| 5 | matrices: candidates × inputs | yes |
| 6 | internal simplification: remove `Plan`/`Drop`; `Suite::add_*_with` | no |
| 7 | runner, CLI, output formats — this completes **Design A** | mostly |
| 8 | migrate docs, README, `benches/` | no |

Stage 2 before 4 is the load-bearing order: it validates `inventory`,
`ErasedInput`, and the whole assembly path against real registrations
written by hand, *before* any proc-macro work is sunk into it. If the
approach is going to fail, it fails there, cheaply.

---

## Stage 1: the registry types

### The shim shape

`inventory` items must be `'static` with no captured environment, so a
registered benchmark body is a bare `fn` item. The crucial constraint is
what those `fn`s must *do*.

A shim of shape `fn(&Config) -> Stats` — one that runs the benchmark and
hands back a result — would be a bug. `Config::bench` is `block_on`-based,
so every benchmark would run to completion in sequence, discarding the
cooperative round-robin in `Scheduler::run` (`src/suite.rs`) that exists
precisely so no benchmark is measured in a machine state its neighbours
never saw.

Shims must therefore **add themselves into a suite** rather than run
themselves:

```rust
pub struct Registered {
    pub name: &'static str,           // module-qualified
    pub crate_name: &'static str,     // env!("CARGO_PKG_NAME") at expansion site
    pub crate_version: &'static str,  // env!("CARGO_PKG_VERSION") at expansion site
    pub group: Option<&'static str>,
    pub is_baseline: bool,
    pub kind: Kind,
}

pub enum Kind {
    /// Calls suite.add / add_input / add_gen_input with the real closure,
    /// handing back the token so results stay reachable by name.
    Flat(fn(&mut Suite<'_>, &Config, &str) -> Token<Stats>),
    /// Calls suite.add_scaling / add_scaling_gen; nmin baked into the fn.
    Scaling(fn(&mut Suite<'_>, &Config, &str) -> Token<ScalingStats>),
    /// One alternative in a comparison group. `ComparisonSet` is a consuming
    /// builder, so this takes and returns it.
    Alt(for<'a> fn(ComparisonSet<'a, ErasedInput>, &str) -> ComparisonSet<'a, ErasedInput>),
}

inventory::collect!(Registered);
```

A `fn` item is `'static`, and `'static: 'a` for any `'a`, so these satisfy
`Suite<'a>`'s `F: FnMut() -> O + 'a` bounds without friction. The
higher-ranked `for<'a>` on `Alt` lets one static registration be used with
whatever `Config` borrow the runner establishes at runtime.

Generic parameters never reach the registry: the macro resolves each
benchmark's `F`/`I`/`O` once, at its own expansion site, so the registry
stores only bare function pointers of three fixed shapes. This works because
`Stats`, `ScalingStats` and `Comparisons` are already concrete non-generic
types.

### `ErasedInput`

`ComparisonSet<'a, I>` is generic over one shared input type and requires
`I: Clone`, because `comparison_gen_input` generates **one** `I` per round
and clones it for each alternative. That sharing is not incidental: it is
what makes `paired_std_error` meaningful, since pairing only cancels the
machine's drift when every alternative met the same realized input in the
same round. Independently drawn inputs would still print a number — just a
quietly worse one.

The registry cannot name a concrete `I`, so it needs an erased input that is
nonetheless `Clone`. `Box<dyn Any>` is not, but a box plus a
macro-monomorphized clone pointer is:

```rust
pub struct ErasedInput {
    value: Box<dyn Any>,
    clone_fn: fn(&dyn Any) -> Box<dyn Any>,
    type_id: TypeId,
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

impl ErasedInput {
    pub fn new<I: Any + Clone>(v: I) -> Self {
        ErasedInput {
            value: Box::new(v),
            // Monomorphized here: the macro knows I even though the registry
            // does not.
            clone_fn: |a| Box::new(a.downcast_ref::<I>().unwrap().clone()),
            type_id: TypeId::of::<I>(),
        }
    }
    pub fn get_mut<I: Any>(&mut self) -> &mut I {
        self.value.downcast_mut::<I>().expect("type checked during assembly")
    }
}
```

Every group becomes a `ComparisonSet<'a, ErasedInput>`, including groups with
no declared input — those use `ErasedInput::new(())`. That unifies the
`I = ()` and `I = T` cases into one path rather than leaving
`comparison_gen_input` groups unsupported.

The `expect` never fires in practice: assembly validates every member's
`TypeId` against its group's generator before anything runs (stage 2). It is
a backstop, not the error path.

### Generator and matrix registrations

```rust
pub struct GenInputRegistration {
    pub group: &'static str,
    /// A function returning the id, not the id itself: a registration is a
    /// `static`, so it is built in a `const` context, and `TypeId::of` only
    /// became usable in one in Rust 1.91. A `fn` pointer is
    /// const-constructible on every version.
    pub type_id: fn() -> TypeId,
    pub type_name: &'static str,
    pub make: fn() -> ErasedInput,
}

pub struct MatrixCandidate {
    pub matrix: &'static str,
    pub name: &'static str,
    pub input_type: fn() -> TypeId,  // from the fn signature; see above
    pub is_baseline: bool,
    pub crate_name: &'static str,
    pub crate_version: &'static str,
    /// A lone candidate has nothing to compare against: added as a plain
    /// benchmark, given a maker for the input it is paired with.
    pub add_flat: fn(&mut Suite<'_>, &Config, &str, fn() -> ErasedInput),
    /// The normal path: one alternative in this input's ComparisonSet.
    pub add_alt: for<'a> fn(ComparisonSet<'a, ErasedInput>, &str)
                     -> ComparisonSet<'a, ErasedInput>,
}

pub struct MatrixInput {
    pub matrix: &'static str,
    pub name: &'static str,
    pub type_id: TypeId,
    pub make: fn() -> ErasedInput,
}
```

"Candidate" rather than "row" because it is already the crate's word for one
side of a measured difference — `Comparison` holds a `baseline` and a
`candidate` — and every matrix cell ends up in exactly that role.

`add_flat` takes the input's maker as a *parameter* rather than capturing
it, which is what lets one registered candidate pair with any number of
separately registered inputs it knows nothing about.

### Cargo wiring

```toml
[dependencies]
inventory = { version = "0.3", optional = true }
[features]
registry = ["dep:inventory"]
```

Optional, so the default build keeps the crate's current "no dependencies at
all outside Linux CPU pinning" posture intact. Under Design A this
eventually becomes unconditional (see stage 7).

---

## Stage 2: assembly

`plan()` is deliberately a **pure function over slices**, so it can be
unit-tested exhaustively without running a single benchmark:

```rust
pub(crate) fn plan(
    regs: &[&'static Registered],
    gens: &[&'static GenInputRegistration],
    opts: &Options,
) -> Result<Plan, Vec<Diagnostic>>
```

### Deterministic ordering

`inventory` guarantees no ordering, so assembly **sorts by name**. Today
`Report` prints in declaration order; under registration "declaration order"
is meaningless, and sorted order is the only reproducible choice. Within a
group the baseline goes first (`ComparisonSet`'s first-added-is-baseline
rule) and the rest follow sorted.

This does not affect measurement — `Scheduler::run` reshuffles every round
regardless — but it is what makes two runs diffable.

### Validation, all before any measurement

| check | outcome |
| --- | --- |
| duplicate `name` | error listing every source crate/version/module |
| group with < 2 members | error naming the group |
| group with 0 baselines | error: "add `baseline` to one member" |
| group with > 1 baseline | `BaselinePolicy`, else error listing claimants |
| group with > 1 `gen_input` | error listing both declaration sites |
| member `TypeId` ≠ generator's | error naming the offending function and both types |
| group with members, no generator | fine — defaults to `ErasedInput::new(())` |

Every one is reported **before** `Machine::claim()` — before pinning CPUs or
blocking on the reserved-CPU lock — matching the reasoning already
documented in `ComparisonSet::run` for why it asserts before claiming.

### Verification for this stage

Exercise the whole path with **hand-written `inventory::submit!` calls**, no
macro. This is the stage that proves `inventory` links correctly, that
`ErasedInput` preserves pairing, and that assembly produces a working
`Suite`. Specifically worth asserting: a group whose input dominates runtime
reports a *smaller* `paired_std_error` than the individual `Stats` errors —
that is what shows the erased sharing did not silently degrade pairing.

---

## Stage 3: `Suite::add_registered()` — Design B complete

```rust
impl<'a> Suite<'a> {
    pub fn add_registered(&mut self, policy: BaselinePolicy) -> RegisteredTokens;
}
```

Runs `plan()`, then makes the same `self.add_*()` / `self.add_comparison()`
calls a human would, reusing `Suite`'s existing
`Arc<Mutex<Option<T>>>` → `Arc<dyn Reportable>` coercion verbatim. Returns a
map from name to `Token<Stats>` / `Token<ScalingStats>` /
`Token<Comparisons>` so specific results stay reachable after `run()`.

`add_registered()` runs at ordinary runtime, so it is free to build normal
capturing closures around the static shim pointers — the "no captured
environment" rule applies only to what is submitted at compile time.

Usage is unchanged in shape from today:

```rust
fn main() {
    let cfg = Config::relative(0.01).with_max_time(Duration::from_secs(5));
    let mut suite = cfg.suite();
    suite.add_registered(BaselinePolicy::NewestClaim);
    suite.add("one_off", || { /* still fine by hand */ });
    println!("{}", suite.run());
}
```

At this point Design B is done and usable, with the existing `Plan`/`Drop`
machinery untouched.

---

## Stage 4: the attribute proc macros

These are **attribute** proc macros (`#[scaling::bench]` on an `fn`), not
derive macros — a derive can only attach to a `struct` or `enum`, and what
is being annotated here is a function. This requires a new
`proc-macro = true` crate, `scaling-macros`, and realistically
`syn` + `quote` + `proc-macro2`.

`macro_rules!` cannot attach as an item attribute on stable. The only
macro_rules alternative wraps the whole item —
`scaling::register!{ fn f() {...} }` — which is fragile across generics,
visibility and other attributes. Since stage 3 already works without any
macro, this stage is a pure ergonomics upgrade and its dependency cost can
be weighed on its own.

### Grammar

| attribute | wraps |
| --- | --- |
| `#[scaling::bench]` | `Suite::add` |
| `#[scaling::bench(input = PATH)]` | `Suite::add_input` |
| `#[scaling::bench(gen_input = PATH)]` | `Suite::add_gen_input` |
| `#[scaling::bench_scaling(nmin = N)]` | `Suite::add_scaling` |
| `#[scaling::bench_scaling(gen_input = PATH, nmin = N)]` | `Suite::add_scaling_gen` |
| `#[scaling::gen_input(group = "G")]` | declares group `G`'s shared input |
| `#[scaling::candidate(matrix = "M")]` | one candidate of matrix `M` (stage 5) |
| `#[scaling::input(matrix = "M", name = "N")]` | one input of matrix `M` (stage 5) |

Modifiers on `bench` / `bench_scaling` / `candidate`: `group = "G"`,
`baseline`, `name = "..."`. Per-benchmark accuracy attributes
(`target_rel_error`, `max_time`) are **deferred to stage 6**, since they
need `Suite::add_*_with` to exist first.

`nmin` must be a literal, since it is baked into the generated shim —
`Kind::Scaling` carries no per-call arguments.

### Expansion

```rust
// #[scaling::bench] fn fib_200() -> usize { fib(200) }
#[cfg(feature = "scaling-bench")]
fn fib_200() -> usize { fib(200) }

#[cfg(feature = "scaling-bench")]
#[doc(hidden)]
mod __scaling_fib_200 {
    use super::*;
    fn add(suite: &mut ::scaling::Suite<'_>, cfg: &::scaling::Config, name: &str) {
        suite.add(name, fib_200);
    }
    ::scaling::inventory::submit! {
        ::scaling::registry::Registered {
            name: concat!(module_path!(), "::fib_200"),
            crate_name: env!("CARGO_PKG_NAME"),
            crate_version: env!("CARGO_PKG_VERSION"),
            group: None, is_baseline: false,
            kind: ::scaling::registry::Kind::Flat(add),
        }
    }
}
```

A group member expands to an `Alt` shim instead, threading the consuming
builder and downcasting the erased input:

```rust
fn add<'a>(
    set: ComparisonSet<'a, ErasedInput>,
    name: &str,
) -> ComparisonSet<'a, ErasedInput> {
    set.add_input(name, |e: &mut ErasedInput| sort_std(e.get_mut::<Vec<i32>>()))
}
```

### Names

Default to `concat!(module_path!(), "::", fn_name)` so two `fib` benchmarks
in different modules do not collide; `name = "..."` overrides for display.
Duplicate final names are a startup error naming both sources, never a
silent overwrite.

### Compiling out of normal builds

An earlier draft had the macro emit `#[cfg(feature = "scaling-bench")]`
around its whole expansion. **Don't.** That invents a feature name the
caller never chose, and compiles silently to nothing for anyone who has not
defined that exact feature.

It is also unnecessary. A `#[cfg]` written *above* the attribute strips the
item before the macro ever runs — verified: a proc macro placed under a false
`#[cfg]` never executes. So the caller picks their own name and nothing has
to be built in:

```rust
#[cfg(feature = "my-benchmarks")]
#[scaling::bench]
fn something() { ... }
```

With that feature off, none of it is compiled, type-checked or linked, so
annotated benchmarks can live in `src/` next to what they measure at zero
cost to ordinary builds and to published-crate consumers. It also works with
any `cfg`, not only features.

Two consequences:

- The `[[bench]]` target needs `required-features = ["scaling-bench"]`, so
  `cargo bench` without it skips rather than failing to link. A
  `.cargo/config.toml` alias saves typing.
- Cargo **feature unification**: inside a workspace, another member enabling
  `scaling-bench` (even transitively via dev-dependencies) turns it on for
  every build of this crate in that graph. Worth a doc warning.

Because `inventory` collects everything linked into one binary, a second
`[[bench]]` target linking the same library rediscovers and re-runs the same
registrations. Decide deliberately that registry benchmarks live in exactly
one binary rather than meeting this by surprise.

---

## Stage 5: matrices

The natural way to want *N candidates × M inputs* is to define each
candidate and each input **separately** and have the cross-product formed
for you. Writing N×M wrappers by hand, or generating them from one central
`bench_matrix!` invocation, reintroduces exactly the central assembly site
this whole design removes.

```rust
// candidates, defined anywhere
#[scaling::candidate(matrix = "sort", baseline)]
fn std_sort(v: &mut Vec<i32>) { v.sort(); }

#[scaling::candidate(matrix = "sort")]
fn unstable_sort(v: &mut Vec<i32>) { v.sort_unstable(); }

// inputs, defined anywhere else
#[scaling::input(matrix = "sort", name = "reversed")]
fn reversed() -> Vec<i32> { (0..10_000).rev().collect() }

#[scaling::input(matrix = "sort", name = "random")]
fn random() -> Vec<i32> { random_vec(10_000) }
```

Adding an input is one new function in whatever file makes sense; every
existing candidate picks it up automatically, and vice versa.

### Type lanes

Pairing is by `TypeId`: a candidate pairs with exactly those inputs whose
`type_id` matches its `input_type`. A matrix therefore partitions into
**lanes**, one per input type, and cells form only within a lane — so a
matrix mixing unrelated type families works correctly and for free rather
than being a type error.

An **orphan** (a candidate whose lane has no inputs, or an input whose lane
has no candidates) is almost always a typo or a type mismatch, so it is a
startup warning naming the item and its type.

### Generic candidates and sized inputs

```rust
#[scaling::candidate(matrix = "hash", types(String, Vec<u8>, [u8; 32]))]
fn fnv<T: AsRef<[u8]>>(x: &mut T) { fnv_bytes(x.as_ref()); }

#[scaling::input(matrix = "sort", sizes(100, 10_000, 1_000_000))]
fn random_vec(n: usize) -> Vec<i32> { (0..n).map(|_| rand()).collect() }
```

`types(...)` emits one `MatrixCandidate` per listed type, each with its own
monomorphized shim and `TypeId`. This is the one place monomorphization must
be spelled out — Rust offers no way around naming the instantiations — but
it is one annotation, not one wrapper per pair. `sizes(...)` emits one
`MatrixInput` per size, named `random_vec@100` and so on, which covers the
"large strings, small strings" pattern directly.

### Every multi-candidate lane is a comparison

There is one mode, not two. For each *input*, assembly builds one
`ComparisonSet<ErasedInput>` over every candidate in that lane, baseline
first, and calls `suite.add_comparison("sort@random", set)`. A 3×3 matrix
produces 3 comparisons of 3 alternatives each.

The only exception is arithmetic rather than policy: a lane holding exactly
one candidate has nothing to compare against, so its cells are added as
plain benchmarks via `add_flat`.

This loses nothing. `Comparisons` carries the baseline's `Stats` *and* each
candidate's, so absolute per-cell timings remain available for the table — a
comparison is a strict superset of a bare timing grid, not an alternative
view of it. That is what makes "always compare" the right rule rather than a
tradeoff, and it means there is no mode flag to design.

### Choosing the baseline

Requiring an explicit `baseline` everywhere would be friction exactly where
this design removes it — you would have to pick a privileged candidate
before knowing which is fastest. So:

1. exactly one candidate marked `baseline` → it wins;
2. several marked (the multi-version case) → `BaselinePolicy` resolves;
3. **none marked → the lexicographically first name in the lane**,
   deterministically.

Rule 3 makes the common case zero-annotation. Its cost is worth stating:
adding a candidate that sorts earlier silently moves the baseline, so every
reported percentage re-bases at once and a CI diff lights up entirely. The
measurements are unaffected. Mitigations: the output **names the baseline**
(`sort@random (baseline: radix_sort)`), and the docs should recommend an
explicit mark for anything CI-tracked.

### Budget

`Suite` gives each entry its own `max_time`, and `add_comparison` multiplies
an input's budget by its `k` — so a 3×3 matrix at a 5s default is 3 × 3 × 5s
≈ 45 seconds, growing with the product. Print the discovered cell count and
a worst-case estimate before starting.

---

## Stage 6: internal simplification

Only now, with the registry working, do the internal changes become
motivated rather than speculative.

### Removing `Plan` / `Drop`

`Config` carries a `Plan` behind an `Arc` — promised count, cached
`z_alpha`, count made — plus a `Drop` asserting the counts match, plus a
`by_caller` flag and a detach-on-plan rule for template `Config`s. All of it
exists because the threshold is computed somewhere other than where the
comparisons are made, so the count has to travel.

Mostly it need not. Each entry point knows its own family: `compare*` makes
one, `ComparisonSet::run` makes `k - 1`, and a `Suite` knows its total once
its last entry is added, before it runs anything. So each computes its own
limit and passes it down.

**One caveat that must not be glossed:** this is only fully correct for
`Suite`. Calling `cfg.compare()` `n` times judges each result as a family of
one, so the chance of some false positive among them grows with `n`,
uncorrected — and forcing the caller to declare that total is precisely what
`with_comparisons_planned` + `Drop` bought. Removing them removes a real
guarantee for the standalone path. That is acceptable only because the
standalone comparison functions are expected to give way to `Suite`, which
is the only path that sees a whole family. The docs must say so plainly
rather than describing the result as a clean win.

Two implementation notes found by building this once already:

- A suite's comparisons are boxed as they are added, before the total is
  known, so the limit must reach them through a shared cell that `run` fills
  in first — and **that read must stay lazy**. An eager read hands every
  comparison `NaN`, which fails *silently*: nothing is ever significant,
  each comparison burns its full budget, and the report reads as a pile of
  honest "unchanged" results. Test for it explicitly.
- The plan's counter was doing double duty: besides counting, it seeded
  which order alternatives are timed in. Removing it leaves every standalone
  comparison on seed 0, so consecutive comparisons draw identical sequences
  and a loop of them correlates with itself. Replace it with a
  process-global seed counter. The tests that would catch this are gated on
  a quiesced machine and **skip silently on most**, reporting `ok` in 0.00s.

### Per-benchmark `Config`

Add `add_with`, `add_input_with`, `add_gen_input_with`, `add_scaling_with`,
`add_scaling_gen_with`, each taking the `Config` that one benchmark is
measured against; existing methods forward with the suite's own. This is
what stage 4's deferred accuracy attributes need. The `Config` must outlive
the suite.

The threshold stays suite-wide: it belongs to the family, so a benchmark
cannot opt out of the family it is in by bringing its own `Config`.

While here: `add_comparison` sizes its clock from the *suite's* `max_time`
while `run_async` takes the accuracy goal from the *set's* `Config`, so a
set built from a different `Config` chases one target on the other's budget.
Use the set's for both. Note `hit_limit` cannot detect this — an unreachable
goal ends at the budget either way — so test elapsed time instead.

---

## Stage 7: runner and output — Design A

Everything above is Design B plus shared substrate. Design A adds:

- `scaling::main!()` expanding to a runner that discovers, assembles, runs
  and prints — turning `benches/bench.rs` into one line.
- Hand-rolled CLI (~100 lines, no `clap`): name filter, `--exact`,
  `--list`, `--format table|json|matrix`, accuracy and budget flags,
  `--baseline`.
- Output formats: the existing `Report` `Display` for tables, a 2-D matrix
  layout per lane, and a hand-rolled JSON emitter (~40 lines — every field
  is a number, bool or string, so `serde` would earn nothing).
- Non-zero exit on `untrustworthy`, or with `--fail-on-regression` on a
  detected slowdown — which is what makes the runner usable as a CI gate,
  and is only possible because `scaling` owns the top level.
- `Config` becomes implicit, built from flags rather than written in a
  `main()` you control. This is the real ergonomic cost of A.
- `inventory` and `scaling-macros` become unconditional dependencies, since
  there is no "without it" mode left to preserve. A permanent departure from
  the crate's zero-dependency posture.

`--list` deserves emphasis: it makes the registry inspectable without
running anything, which is the practical answer to "did my benchmark
actually get linked in?" — the question `inventory`'s failure modes most
often provoke.

---

## Two constraints found while building this

**A registration is a `static`, so everything in it must be const.** That
rules out `TypeId::of::<I>()` as a field value: it only became usable in a
`const` context in Rust 1.91, far above this crate's 1.66. Registrations
therefore store `fn() -> TypeId` and assembly calls it. Worth knowing before
adding any other field - `stringify!` output and `env!` are fine, most things
that look like values are not. Clippy's `incompatible_msrv` lint catches
this; a modern toolchain alone does not, since it compiles happily.

**`inventory` wants Rust 1.68**, above this crate's 1.66. Being optional it
only raises the floor for crates enabling `registry`; the default build is
unaffected.

## `inventory` risk

Collection is via linker sections and `ctor`-style registration. Known edge
cases: dylib/cdylib boundaries get separate registries; no `no_std`, no
`wasm32-unknown-unknown`; and aggressive LTO / `--gc-sections` / `strip`
configurations have historically stripped address-only-referenced items
(largely mitigated now, but a linker-behaviour dependency rather than a
language guarantee).

Under Design B this is contained: hit one and you simply do not call
`add_registered()` there, and the manual builder keeps working. Under Design
A there is no fallback and any of these is a hard blocker.

## Comparing against past versions

A self-referential renamed dev-dependency:

```toml
[dev-dependencies]
my_crate_baseline = { package = "my_crate", git = "...", tag = "v0.8.0" }
```

**Hard constraint:** dev-dependencies are visible only to
`benches/`/`tests/`/`examples/`, never to `src/`. So the old-version arm can
only live in `benches/*.rs`, never in the library behind the
`scaling-bench` gate. The new-version arm may live in either; both land in
the same binary's registry.

**Automatic pickup** works if the old tag already carried annotations:
enable the feature on that one dependency edge
(`features = ["scaling-bench"]`) and its `submit!` registers into the same
process-wide registry. Three limits, worst first:

1. **Only works going forward** — the old tag must already be annotated.
2. **Both crates must resolve to the same `scaling` type instance.**
   `inventory` keys on the literal monomorphized `Registered`; two `scaling`
   versions in the graph means the old registrations land in a different,
   invisible registry — compiles fine, silently missing. Sharp risk
   precisely because `scaling` is pre-1.0 and still churning.
3. **`is_baseline` gets frozen into historical source** rather than chosen
   at comparison time.

Given (2), manual wrappers stay the recommended default.

### `BaselinePolicy`

`env!("CARGO_PKG_VERSION")` / `env!("CARGO_PKG_NAME")` evaluate in whichever
crate the macro expands in, so every registration is stamped with its true
origin for free — exactly the signal needed when several versions each
self-declare `baseline`:

```rust
pub enum BaselinePolicy {
    NewestClaim,   // default: highest crate_version among claimants
    OldestClaim,
    Exact { crate_name: &'static str, crate_version: &'static str },
    Custom(fn(&[&Registered]) -> usize),
}
```

Two notes:

- Version comparison must be **numeric, not lexical** (`"0.10.0"` sorts
  *after* `"0.9.0"`, which string comparison gets backwards). A ~15-line
  major/minor/patch parser suffices; `semver` would be a dependency for
  three integers.
- Only ever choose among entries that opted in with `is_baseline`, and error
  when the winner's `crate_version` equals the current crate's own — the
  in-development version can outrank every published tag, so `NewestClaim`
  would otherwise pick exactly backwards.

Not on `Config`, which stage 6 strips to timing parameters only. Under B it
is a parameter to `add_registered`; under A, a CLI flag.

## Known limitation, both designs

`comparison_gen_input` groups need runtime `Any`/`TypeId` typing
(`ErasedInput`), trading `ComparisonSet<I>`'s compile-time type safety for a
startup check. The check is exhaustive and reported before measuring, but it
is a check rather than a proof, and it is unavoidable if separately
registered functions are to share one generated input.
