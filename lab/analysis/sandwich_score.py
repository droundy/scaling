# Scores lab/day/collect/pin-sandwich: c0's groups at 1,1,9 laps with 3 ms units, four variants
# (plain/sandwiched canary x unpinned/pinned). For the group {slow_cpu, slow_cpu2, cpu_canary}:
#   plain: one canary sample per round; ratio of each chain to it, split by whether they were adjacent.
#   sand:  B#0 C B#1 C B#2; ratio of each chain to the mean of its two neighbouring canary copies,
#          to one neighbour, and to the canary copy that was not adjacent.
# Also the canary's clock states (its long lap per link), which show what pinning changes.
import sys, os, glob, math, statistics as st
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from spacing import load_ordered, lag1, bm_ratio, robust_sd
from lap_score import meas_one, tstat

ROOT = os.environ.get('PS', os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'day', 'collect', 'pin-sandwich'))
VARIANTS = ['plain-free', 'plain-pin', 'sand-free', 'sand-pin']
GROUP = {'slow_cpu', 'slow_cpu2', 'cpu_canary'}
CHAINS = ('slow_cpu', 'slow_cpu2')


def base(w):
    return w.split('#')[0]


def group_seq(samples):
    return [(s[0], math.log(meas_one(s[1], s[2]))) for s in samples if base(s[0]) in GROUP and meas_one(s[1], s[2]) > 0]


def series(rounds, sandwiched):
    """Per chain: per-round log ratios against the canary, by kind of reference."""
    out = {c: {} for c in CHAINS}
    for samples in rounds:
        seq = group_seq(samples)
        for i, (w, lm) in enumerate(seq):
            if w not in CHAINS:
                continue
            cans = [(j, l) for j, (x, l) in enumerate(seq) if base(x) == 'cpu_canary']
            if not cans:
                continue
            adj = [l for j, l in cans if abs(j - i) == 1]
            far = [l for j, l in cans if abs(j - i) > 1]
            d = out[w]
            if sandwiched:
                if len(adj) == 2:
                    d.setdefault('mean of both neighbours', []).append(lm - (adj[0] + adj[1]) / 2)
                if adj:
                    d.setdefault('one neighbour', []).append(lm - adj[0])
                if far:
                    d.setdefault('non-adjacent copy', []).append(lm - far[0])
            else:
                key = 'adjacent' if adj else 'not adjacent'
                d.setdefault(key, []).append(lm - (adj or far)[0])
                d.setdefault('either', []).append(lm - (adj or far)[0])
    return out


def describe(v):
    if len(v) < 20:
        return f'n={len(v)}'
    tm, se, df = tstat(v)
    return f'n={len(v):5} robust sd {100 * robust_sd(v):6.3f}%  bar {100 * se:.3f}%  lag-1 {lag1(v):+.2f}  bm/se {bm_ratio(v):.2f}'


def clock(rounds):
    v = [meas_one(s[1], s[2]) for r in rounds for s in r if base(s[0]) == 'cpu_canary']
    q = st.quantiles(v, n=20)
    return (f'canary ns/link p5 {q[0]:.3f} p50 {st.median(v):.3f} p95 {q[-1]:.3f};  near top turbo (<0.93) '
            f'{100 * sum(x < 0.93 for x in v) / len(v):.0f}%, near base clock (>2.2) {100 * sum(x > 2.2 for x in v) / len(v):.1f}%')


if __name__ == '__main__':
    for var in VARIANTS:
        for p in (0, 1):
            g = glob.glob(f'{ROOT}/{var}/p{p}/*.bin')
            if not g or os.path.getsize(g[0]) == 0:
                continue
            rounds = load_ordered(g[0])
            print(f'\n{var} p{p}: {len(rounds)} rounds.  {clock(rounds)}')
            s = series(rounds, var.startswith('sand'))
            for c in CHAINS:
                for k, v in sorted(s[c].items()):
                    print(f'  {c:9} vs canary, {k:24} {describe(v)}')
            # the fast pair's exact-ish answer from the best reference available
            key = 'mean of both neighbours' if var.startswith('sand') else 'either'
            a, b = s['slow_cpu'].get(key, []), s['slow_cpu2'].get(key, [])
            if a and b:
                ra, rb = math.exp(tstat(a)[0]), math.exp(tstat(b)[0])
                print(f'  difference against the canary: {(ra - rb) / 36 - 1:+.3%} of 36 links')
