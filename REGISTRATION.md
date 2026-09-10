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
| 6 | internal simplification: remove `Plan`/`Drop`; `Suite::add_*_with` | **done** |
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

Warnings and errors are separated deliberately, and reach the caller by
different routes. A contradiction — two baselines, two candidates of a name,
a lane whose types do not really match — makes that lane untrustworthy, so it
is skipped, while an orphan leaves everything else perfectly good. Both come
back in `RegisteredTokens::warnings` rather than stopping the run; only the
whole-registry errors of stage 2 come back as `Err`.

One subtlety worth knowing: a lane is keyed on the type's *spelled name*,
since `TypeId` is not `Ord` and cannot bucket. The real `TypeId`s are then
checked within each lane, so two different types that happen to spell
themselves alike are caught rather than paired — which would otherwise be a
downcast panic later.

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

### Prerequisite: results reachable by name — **done**

**Before any of the direct API is removed**, a `Stats` or a `Comparison` has
to be gettable out of a finished suite *by name* — looked up by its group or
benchmark name — and not only through the `Token` returned when it was
added.

This is now in place: `Report::names`, `Report::contains`, `Report::stats`,
`Report::scaling`, `Report::comparison`, the generic `Report::get`, and
`Report::all_stats`/`all_comparisons` for iterating one kind out of a mixed
report. Asking for the wrong type gives `None` rather than the wrong value,
so a caller that does not know what a name refers to can simply ask.

`Reportable` grew an `as_any` to make it possible. Rendering was once all it
had to do, because anyone wanting the measurement rather than its text held
a `Token`; that stops being true the moment the caller did not write the
`add` call. The original design note — "neither an enum of result kinds nor
any downcasting is needed" — was true of the problem as it stood then and is
not true of this one.

Benchmarks get driven by scripts, not only read by people: checking whether
the best version of a function is really the one being used under some
circumstance, say. A script like that discovers what it wants at runtime and
cannot hold a token that was returned when the benchmark was registered —
and under `Suite::add_registered` nobody holds those tokens at all, because
nothing wrote the `add` call.

That scenario is what the tests exercise, rather than only the mechanism:
they throw the tokens away, find a benchmark by searching `names()`, and ask
which alternative of a comparison actually measured fastest.

### Removing `Plan` / `Drop` — **done**

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

### Per-benchmark `Config` — **done**

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

### Registrations from more than one crate or version

Once an old copy of a crate, or a rival crate, registers alongside the
current one, several registrations arrive under the same name — they *are*
the same source, a version apart. Before this was handled, that was not
merely unsupported: the duplicate-name check rejected the whole matrix, so
the automatic-pickup path above produced zero lanes.

Candidates and inputs need opposite treatment, which is the crux:

- **Candidates are the point.** Two versions of one implementation are
  exactly the comparison being asked for, so both are kept and told apart.
- **Inputs are redundant.** Two versions of one generator are meant to build
  the same data, so measuring on both doubles the work for nothing — and
  worse, an old generator paired with new implementations quietly changes
  what is being measured if the generator itself has changed since. One is
  kept: the newest.

**Names carry only what distinguishes them.** Two versions of one crate give
`sort@0.8.0` and `sort@0.9.0`; two different crates give `sort@mine` and
`sort@theirs`; both differing gives `sort@mine-2.0.0`. Asking merely whether
crates *differ* is the wrong question — with one implementation per crate
the versions differ too, and `sort@mine-2.0.0` says nothing `sort@mine`
does not.

**`VersionPolicy::LatestPerCrate` keeps the newest of *each crate*.** Per
crate, not overall: when the point is measuring against other crates,
dropping a rival's implementation because your own version number happens to
be higher would be exactly wrong. `RegistryOptions::latest_per_crate()` is
the way to ask for it, and it is what you want when several rivals are
present and only their current releases are interesting.

**Versions compare numerically.** `"0.10.0" < "0.9.0"` as text, which is
backwards and silently so — a policy picking the newest would take 0.9.0
over 0.10.0 and nothing would look wrong. A pre-release or build suffix is
dropped rather than made to mean something, and an unparseable version reads
as `0.0.0` rather than stopping a run. Three integers do not justify a
`semver` dependency in a crate whose default build has none.

**Several baseline claimants is expected here**, since every version of a
declaration marked `baseline` says so. `BaselinePolicy` settles it, and
defaults to `Oldest` — which makes a regression read the right way round,
the new code measured *against* the old. Picking the newest would report
every older version as a change *from* the code being written, which is
backwards.

That is only true across origins. Two *different* functions claiming the
baseline within one crate at one version is a plain contradiction, not a
version spread, and is still an error — resolving it by version would pick
one silently and hide the mistake.

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

---

# Running only some of the benchmarks

Once benchmarks are registered rather than assembled, a binary holds every
benchmark in the crate and a run measures all of them. That is the wrong
default for iterating on one function, and it gets worse as a crate grows —
`Suite` gives each entry its own `max_time`, so the cost of a run is linear
in how many there are.

## Command line *and* environment, not one or the other

Both, with the command line taking precedence. They fail in different places
and neither covers the other:

- **The command line** is what someone types. It is discoverable, it can
  have a `--help`, and `cargo bench -- <filter>` is the shape people already
  know from `cargo test`.
- **The environment** is what survives a wrapper. `make bench`, a CI step, a
  `cargo bench --workspace` that fans out over several crates — none of
  those thread arguments through without being taught to, and
  `SCALING_FILTER=sort make bench` needs nobody's cooperation.

## What cargo actually passes, measured

This is the part worth knowing before writing a parser, because it is not
what you would guess:

| invocation | argv the binary sees |
| --- | --- |
| `cargo bench --bench b` | `["…/b-<hash>", "--bench"]` |
| `cargo bench --bench b -- sort --exact` | `["…/b-<hash>", "sort", "--exact", "--bench"]` |
| `cargo test --bench b` | `["…/b-<hash>"]` |

Three consequences:

1. **`--bench` arrives even with `harness = false`, and even when the user
   passed no arguments at all.** A parser that rejects unknown flags fails on
   the plainest possible invocation, `cargo bench`. It has to be swallowed.
2. **Cargo appends it *after* the user's arguments**, so a parser cannot
   assume its own flags come last.
3. **`cargo test` passes nothing**, so the same binary must do something
   sensible with no arguments — which for a bench target under `cargo test`
   means "compile and exit quickly", not "measure everything". Worth a
   `--test` style fast path later; out of scope here.

## Surface — **built**

```
--filter PATTERN     keep entries whose name contains this; repeatable
--skip PATTERN       drop entries matching this, after the filters
--exact              match the whole name instead of a substring
--list               print what would run, and measure nothing
--bench, --test      ignored; cargo passes these whether or not you do
```

Parsed with [`auto-args`], behind the optional `cli` feature — the matching
itself needs no dependency and is always available. `auto-args` has no
positional arguments, so the filter is `--filter sort` where `cargo test`
would take a bare `sort`.

| variable | equivalent |
| --- | --- |
| `SCALING_FILTER` | space-separated `FILTER`s |
| `SCALING_SKIP` | space-separated `--skip` patterns |
| `SCALING_EXACT` | set to anything for `--exact` |

Substring by default, following `cargo test`; several filters are an OR, and
`--skip` is applied afterwards so `--skip` can carve a hole in a broad
filter.

`--list` earns its place here more than in most harnesses: with registered
benchmarks nobody wrote the names down, so "what is there?" has no other
answer. It should print the name and kind of each entry and exit 0 without
claiming the machine.

## What a filter matches, and the one thing it cannot do

Names are what the report shows: `mymod::fib_200` for a benchmark,
`sorting@reversed` for a matrix cell, the group name for a comparison.

**A comparison is atomic.** Its alternatives are measured in one interleaved
round precisely so their differences are paired, so "run only the
`unstable` alternative of the `sorting` comparison" is not a smaller version
of that comparison — it is a different measurement, and a worse one. So
filtering works at *entry* granularity: a comparison is in or out as a
whole, and a filter matching its name takes all of it.

Whether a filter matching an *alternative's* name should pull in its whole
comparison is a real choice. Pulling it in is surprising (you asked for one
thing and got four); not pulling it in is surprising the other way (you named
something real and got nothing). **Suggest: not matched, but `--list` shows
alternatives indented under their comparison** so the name you would have to
filter on is visible.

## Where it lives

A `Filter` on the `Suite`, not on `add_registered`, so that hand-added
benchmarks obey it too — a run that honours `--filter` for registered
benchmarks and silently ignores it for the two you added by hand is worse
than not having it.

```rust
let mut suite = cfg.suite().with_filter(Filter::from_env_and_args());
suite.add_registered();
suite.add("by_hand", || work());   // filtered on the same terms
println!("{}", suite.run());
```

`Filter::from_env_and_args()` is explicit rather than automatic: a library
that reads `argv` because it was linked in, without being asked, is a
library that surprises somebody. Under Design A the generated runner calls
it, so this composes forward rather than being replaced.

Filtered-out entries are **not added at all** — no `Clock`, no scheduler
slot, no time. `add*` still returns a `Token`, which simply never fills;
that matches what already happens when a suite is built and not run, and
keeps the return type honest without an `Option` at every call site.

### One consequence worth stating

Filtering changes the Bonferroni count, and it should. Run five comparisons
and you are exposed to five chances of a false positive; run one and you are
exposed to one. `Suite::run` computes the limit from what the suite actually
holds, so this already falls out — but it means **a comparison's verdict can
differ between a filtered and an unfiltered run**, and the docs must say so
rather than leaving someone to discover that a change was "significant"
alone and not in the suite.

## What was built

`Filter` (matching, always available), `Filter::from_args` /
`from_env` / `from_env_and_args` (behind `cli`), `Suite::with_filter`,
`Suite::names`, and `benches/filtered.rs` as a worked example. Verified
through real `cargo bench` invocations rather than only in tests: plain,
`-- --list`, `-- --filter sorting`, `-- --filter sorting --skip large`, and
`SCALING_FILTER=hashing` with no arguments passed at all.

Still open:

- **Registered benchmarks do not yet say what a filter skipped.** They
  simply do not appear, which is right for a report and thin for someone
  wondering whether their pattern matched anything.
- **`--list` shows entries, not alternatives.** A comparison lists under its
  own name, so the alternative names — the ones you would have to *stop*
  filtering on — are not shown. Indenting them under their comparison is the
  suggested fix.

[`auto-args`]: https://crates.io/crates/auto-args
