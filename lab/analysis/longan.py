# Long samples against a known answer. slow_cpu (k ms) and slow_cpu2 (0.6k ms) run the
# same chain, so slow_cpu's cost per call over slow_cpu2's is exactly 1270000/762000 = 5/3.
# Rungs are 1 and 2 calls. Reads day/collect/long/long-{fp,int}-{9,15,30,60}ms, written by longs.sh;
# 1,270,000 links take 3 ms at 1.7 GHz, so the names give slow_cpu's call length.
import sys, os, glob, math, statistics as st
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import paired
from rounds import load, COLLECT

SP = os.path.join(COLLECT, 'long')
TRUTH = 1270000 / 762000

def tmean(v):
    return paired.tmean(v)

def by_rung(samples):
    out = {}
    for n, t in samples:
        if t > 0:
            out.setdefault(n, []).append(t)
    return out

def slope(r, mean):
    (n1, v1), (n2, v2) = sorted(r.items())[-2:]
    return (mean(v2) - mean(v1)) / (n2 - n1), (n2 * mean(v1) - n1 * mean(v2)) / (n2 - n1)

def one_call(rs):
    # Rounds where both ran one call: the ratio with no subtraction at all.
    v = [math.log(ta) - math.log(tb) for na, ta, nb, tb in rs if na == 1 and nb == 1]
    return math.exp(tmean(v)), math.exp(st.mean(v))

def pct(x):
    return f'{100 * (x / TRUTH - 1):+.3f}%'

for d in sorted(glob.glob(SP + '/long-*'), key=lambda p: (p.split('-')[1], int(p.split('-')[-1].rstrip('ms')))):
    bins = glob.glob(d + '/*.bin')
    if not bins:
        print(os.path.basename(d), 'no recording yet')
        continue
    per = load(bins[0])
    rs = paired.pair_rounds(per, 'slow_cpu', 'slow_cpu2')
    a, b = by_rung(per['slow_cpu']), by_rung(per['slow_cpu2'])
    sa_t, fa_t = slope(a, tmean); sb_t, fb_t = slope(b, tmean)
    sa_m, fa_m = slope(a, st.mean); sb_m, fb_m = slope(b, st.mean)
    one_t, one_m = one_call(rs)
    log_model = paired.ratio(rs)
    bar = paired.bar(rs)
    k = os.path.basename(d)
    call_ms = st.median(a[1]) / 1e6
    print(f'{k:12} {len(rs):6} rounds, slow_cpu call {call_ms:5.2f} ms   workloads: {", ".join(sorted(per))}')
    print(f'    ratio against exactly 5/3:  log model {pct(log_model)}   '
          f'subtract trimmed {pct(sa_t / sb_t)}   subtract plain {pct(sa_m / sb_m)}   '
          f'one call trimmed {pct(one_t)}   one call plain {pct(one_m)}')
    print(f'    whole-run bar {100 * bar:.3f}%   fixed cost per batch (trimmed): '
          f'slow_cpu {fa_t / 1e3:+.1f} us, slow_cpu2 {fb_t / 1e3:+.1f} us')
    links_a = int(k.split('-')[-1].rstrip('ms')) // 3 * 1270000
    if 'cpu_canary' in per:
        c = by_rung(per['cpu_canary'])
        sc, fc = slope(c, tmean)
        la_t, la_m = sa_t / links_a, sa_m / links_a
        print(f'    per link: slow_cpu trimmed {la_t:.4f} plain {la_m:.4f} ns, canary {sc:.4f} ns '
              f'-> slow over canary {100 * (la_t / sc - 1):+.2f}% trimmed, {100 * (la_m / sc - 1):+.2f}% plain; '
              f'canary fixed cost {fc:+.0f} ns')
    for name, r in (('slow_cpu', a), ('slow_cpu2', b)):
        for n, v in sorted(r.items()):
            q = st.quantiles(v, n=10)
            med = st.median(v)
            print(f'    {name:9} n={n}: median {med / 1e6:7.3f} ms   p10 {(q[0] - med) / 1e3:+6.1f} us   '
                  f'p90 {(q[-1] - med) / 1e3:+6.1f} us   max {(max(v) - med) / 1e3:+7.1f} us')
