# Changelog

## 0.9.0

A new design under the name of the 0.1 releases, which were a fork of
`easybench` with five functions. Those functions are gone and the types that
remain have changed, so this is a rewrite to port to rather than an upgrade;
`scaling = "0.1"` keeps working as before, and Cargo will not move anyone from
it to 0.9.

### Coming from 0.1

| 0.1 | 0.9 |
|---|---|
| `bench(\|\| f())` | `#[scaling::bench] fn name() -> O { f() }` |
| `bench_env(env, f)` | `#[scaling::bench(input = env)] fn name(env: &mut I) -> O` |
| `bench_gen_env(gen, f)` | `#[scaling::bench(make_input = gen)] fn name(env: &mut I) -> O` |
| `bench_scaling(f, nmin)` | `#[scaling::bench_scaling(nmin = N)] fn name(n: usize) -> O` |
| `bench_scaling_gen(gen, f, nmin)` | `#[scaling::bench_scaling(nmin = N, make_input = \|n\| ...)] fn name(input: &mut I) -> O` |
| `Stats` (`ns_per_iter`, `goodness_of_fit`, `iterations`, `samples`) | `Timing` (`ns_per_iter`, `std_error`, `iterations`, `samples`, `hit_limit`, ...), from `Report::timing` |
| `ScalingStats`, `Scaling` | `ScalingStats`, `Scaling`, from `Report::scaling`; `Scaling` is now optional, `None` when no law was found |
