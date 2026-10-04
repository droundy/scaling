# Scores the lap design (lab/day/collect/laps): every sample is 4 laps of n iterations, lap 0 is
# warm-up and laps 1..3 the measurement. Each round gives one steady-state value per function, so a
# ratio is a trimmed mean of per-round log ratios and its bar the standard error over rounds, with
# Student's t at the trimmed mean's degrees of freedom. Nothing here assumes a mechanism.
import sys, os, glob, math, statistics as st
from functools import lru_cache
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ab_score import load

ROOT = os.environ.get('LAPS', os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'day', 'collect', 'laps'))
LAPS = os.environ.get('LAPDIRS', 'l1,l3,l10,l30,l100').split(',')
COMPS = [int(c) for c in os.environ.get('COMPS', '0,1,4,5').split(',')]
# 'auto': with unequal laps (shape 1,1,9) subtract the short measured lap from the long one, which cancels
# any fixed cost per lap; with equal laps sum them. 'sum': always sum the measured laps.
EST = os.environ.get('EST', 'auto')
NEIGHBOUR = {1: 'long integer calls', 4: 'warmer', 5: 'all of them'}
BASE = ['cpu_canary', 'slow_cpu', 'slow_cpu2', 'f64_sin', 'parse_u64', 'btree_miss', 'warm_dst']
GAMMA = 0.25
# Untimed preparation per iteration, from the A/B recordings' headers (lap mode records none).
PREP_NS = {'f64_sin': 12.4, 'parse_u64': 125.0}
PER_SAMPLE_NS = 200.0
FLOOR = 8


# ---- Student's t through the incomplete beta ---------------------------------------------------

def betacf(a, b, x):
    qab, qap, qam = a + b, a + 1, a - 1
    c, d = 1.0, 1 - qab * x / qap
    d = 1 / (d if abs(d) > 1e-300 else 1e-300); h = d
    for m in range(1, 300):
        m2 = 2 * m
        aa = m * (b - m) * x / ((qam + m2) * (a + m2))
        d = 1 + aa * d; d = 1 / (d if abs(d) > 1e-300 else 1e-300)
        c = 1 + aa / c if abs(c) > 1e-300 else 1e300
        h *= d * c
        aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2))
        d = 1 + aa * d; d = 1 / (d if abs(d) > 1e-300 else 1e-300)
        c = 1 + aa / c if abs(c) > 1e-300 else 1e300
        de = d * c; h *= de
        if abs(de - 1) < 1e-14:
            break
    return h


def betai(a, b, x):
    if x <= 0: return 0.0
    if x >= 1: return 1.0
    lb = math.lgamma(a + b) - math.lgamma(a) - math.lgamma(b) + a * math.log(x) + b * math.log(1 - x)
    if x < (a + 1) / (a + b + 2):
        return math.exp(lb) * betacf(a, b, x) / a
    return 1 - math.exp(lb) * betacf(b, a, 1 - x) / b


def two_sided(t, df):
    return betai(df / 2, 0.5, df / (df + t * t))


@lru_cache(maxsize=None)
def tq(df, m):
    """Two-sided Bonferroni threshold: P(|T_df| > t) = 0.05 / m."""
    alpha = 0.05 / m
    lo, hi = 0.0, 1e4
    for _ in range(200):
        mid = (lo + hi) / 2
        if two_sided(mid, df) > alpha: lo = mid
        else: hi = mid
    return mid


# ---- reading laps -------------------------------------------------------------------------------

@lru_cache(maxsize=None)
def recording(lap, comp, p):
    g = glob.glob(f'{ROOT}/{lap}/c{comp}/p{p}/*.bin')
    if not g or os.path.getsize(g[0]) == 0:
        return None
    try:
        per, _ = load(g[0])
    except (ValueError, IndexError):
        return None
    out = {}
    for w in {k.rsplit('.lap', 1)[0] for k in per}:
        k = sum(1 for name in per if name.rsplit('.lap', 1)[0] == w)
        laps = [per[f'{w}.lap{j}'] for j in range(k)]
        out[w] = [([x[0] for x in l], [x[1] for x in l]) for l in zip(*laps)]
    return out


def meas_one(ns, ls):
    if EST != 'sum' and len(ns) >= 3 and ns[-1] != ns[1]:
        return (ls[-1] - ls[1]) / (ns[-1] - ns[1])
    return sum(ls[1:]) / sum(ns[1:])


def fixed_ns(ns, ls):
    """Fixed cost per lap, from the short and the long measured lap (unequal laps only)."""
    return (ns[-1] * ls[1] - ns[1] * ls[-1]) / (ns[-1] - ns[1])


def meas(rows):
    return [meas_one(ns, ls) for ns, ls in rows]


def tstat(v):
    """Trimmed mean, its standard error (winsorised) and Tukey-McLaughlin degrees of freedom."""
    n = len(v); g = int(GAMMA * n); s = sorted(v)
    core = s[g:n - g]
    tm = sum(core) / len(core)
    w = [s[g]] * g + core + [s[n - g - 1]] * g
    sw = st.stdev(w) if n > 1 else float('inf')
    se = sw / ((1 - 2 * g / n) * math.sqrt(n))
    return tm, se, max(n - 2 * g - 1, 1)


def tmean(v):
    return tstat(v)[0]


def cost_ns(rec, members, r0, r1):
    s = 0.0
    for w in members:
        for ns, ls in rec[w][r0:r1]:
            s += sum(ls) + PREP_NS.get(w, 0.0) * sum(ns) + PER_SAMPLE_NS
    return s


def series(rec, name):
    """(per-round values, truth or None, members). Values are on a log scale (or a fraction near 0)."""
    can = meas(rec['cpu_canary'])
    if name == 'fast pair':
        a, b = meas(rec['slow_cpu']), meas(rec['slow_cpu2'])
        return [(x - y) / c / 36 - 1 for x, y, c in zip(a, b, can)], 0.0, ['slow_cpu', 'slow_cpu2', 'cpu_canary']
    if name == 'slow pair':
        a, b = meas(rec['slow_cpu3']), meas(rec['slow_cpu4'])
        return [math.log(x / y) for x, y in zip(a, b)], math.log(3810000 / 2286000), ['slow_cpu3', 'slow_cpu4']
    w = name.split('/')[0]
    return [math.log(x / c) for x, c in zip(meas(rec[w]), can)], None, [w, 'cpu_canary']


# ---- reports ------------------------------------------------------------------------------------

def pc(x):
    return '   -   ' if x is None or x != x else f'{100 * x:+7.3f}'


def known():
    print('\n1. KNOWN ANSWERS, whole run: error % (bar %), p0 / p1. Paired = trimmed mean of per-round values;'
          '\n   per-side = ratio of each side\'s trimmed mean.')
    for name, need, comps in (('fast pair', 'slow_cpu', COMPS), ('slow pair', 'slow_cpu3', [1, 5])):
        print(f'\n  {name}' + ('  (difference against the canary, exactly 36 links)' if name == 'fast pair' else '  (exactly 5/3)'))
        for lap in LAPS:
            row = f'  {lap:5}'
            for c in comps:
                cells = []
                for p in (0, 1):
                    rec = recording(lap, c, p)
                    if rec is None or need not in rec:
                        cells.append('    -    '); continue
                    v, truth, _ = series(rec, name)
                    tm, se, _ = tstat(v)
                    if name == 'fast pair':
                        a, b, cn = (tmean(meas(rec[w])) for w in ('slow_cpu', 'slow_cpu2', 'cpu_canary'))
                        side = (a - b) / cn / 36 - 1
                    else:
                        side = math.log(tmean(meas(rec['slow_cpu3'])) / tmean(meas(rec['slow_cpu4']))) - truth
                    cells.append(f'{pc(tm - truth)}({100 * se:.3f}) side{pc(side)}')
                row += f'  c{c}: ' + ' / '.join(cells)
            print(row)


def composition():
    print('\n2. COMPOSITION: shift of each function\'s steady-state time from c0, %, mean of passes;'
          '\n   null = rms pass-to-pass / sqrt 2')
    print('  lap   fn          ' + ''.join(f'{"+" + NEIGHBOUR[c][:16]:>19}' for c in (1, 4, 5)) + '     null')
    for lap in LAPS:
        for w in BASE:
            val = {}
            for c in COMPS:
                for p in (0, 1):
                    rec = recording(lap, c, p)
                    if rec is not None and w in rec:
                        val[(c, p)] = math.log(tmean(meas(rec[w])))
            m = lambda c: (sum(val[(c, p)] for p in (0, 1) if (c, p) in val) / max(1, sum((c, p) in val for p in (0, 1)))) if any((c, p) in val for p in (0, 1)) else None
            d = [val[(c, 0)] - val[(c, 1)] for c in COMPS if (c, 0) in val and (c, 1) in val]
            null = math.sqrt(sum(x * x for x in d) / len(d) / 2) if d else None
            b = m(0)
            print(f'  {lap:5} {w:11} ' + ''.join(f'{pc(m(c) - b) if b is not None and m(c) is not None else "   -   ":>19}' for c in (1, 4, 5)) + f'  {pc(null)}')
        print()


def laps():
    print('\n3. LAPS: warm-up lap against the measured laps, and the trend across laps 1..3 (median over rounds), %')
    print('  lap   comp ' + ''.join(f'{w:>22}' for w in BASE))
    for lap in LAPS:
        for c in (0, 5):
            rec = recording(lap, c, 0)
            if rec is None:
                continue
            cells = []
            for w in BASE:
                rows = rec[w]
                warm = st.median(ls[0] / ns[0] / meas_one(ns, ls) - 1 for ns, ls in rows)
                ns0 = rows[0][0]
                if ns0[-1] != ns0[1]:
                    # unequal laps: fixed cost per lap, as % of the short lap
                    second = st.median(fixed_ns(ns, ls) / ls[1] for ns, ls in rows)
                else:
                    second = st.median(ls[-1] / ns[-1] / (ls[1] / ns[1]) - 1 for ns, ls in rows)
                cells.append(f'{100 * warm:+7.2f} {100 * second:+6.2f}')
            print(f'  {lap:5} c{c}   ' + ''.join(f'{x:>22}' for x in cells))
    print('  (each cell: warm-up excess; then, for equal laps, last lap over first measured lap,'
          '\n   and for unequal laps the fixed cost per lap as a fraction of the short lap)')


def correlation():
    print('\n4. ROUND-TO-ROUND CORRELATION of per-round values (lag 1), c0 p0; and batch-means SE (blocks of 5) over plain SE')
    for lap in LAPS:
        rec = recording(lap, 0, 0)
        if rec is None:
            continue
        out = []
        for name in ('fast pair', 'f64_sin/canary', 'parse_u64/canary', 'btree_miss/canary', 'warm_dst/canary'):
            v, _, _ = series(rec, name)
            mu = st.mean(v); dv = [x - mu for x in v]
            r1 = sum(a * b for a, b in zip(dv, dv[1:])) / sum(x * x for x in dv)
            k = 5; nb = len(v) // k
            if nb >= 8:
                bm = [st.mean(v[i * k:(i + 1) * k]) for i in range(nb)]
                ratio_se = (st.stdev(bm) / math.sqrt(nb)) / (st.stdev(v) / math.sqrt(len(v)))
            else:
                ratio_se = float('nan')
            out.append(f'{name.split("/")[0]}: r1 {r1:+.2f} bm/se {ratio_se:.2f}')
        print(f'  {lap:5} ({len(v)} rounds)  ' + '   '.join(out))


def trials(rec, name, goal, m):
    v, truth, members = series(rec, name)
    goal_ln = math.log1p(goal)
    out, s = [], 0
    while s + FLOOR <= len(v):
        n = FLOOR
        while True:
            seg = v[s:s + n]
            capped = len(seg) < n
            tm, se, df = tstat(seg)
            thr = se * (tq(df, m) if m else 1.0)
            if capped or thr <= goal_ln:
                break
            n = int(n * 1.3) + 1
        err = tm - truth if truth is not None else None
        out.append((err, thr, len(seg), cost_ns(rec, members, s, s + len(seg)), capped))
        s += len(seg)
    return out


MODES = [('strict 2.5%, family 500 (tinyset old vs new)', 0.025, 500),
         ('strict 1%, family 10', 0.01, 10),
         ('strict 1%, family 1500', 0.01, 1500),
         ('strict 5%, family 10', 0.05, 10),
         ('rough 20%, one sigma (tinyset vs std)', 0.20, None),
         ('rough 5%, one sigma', 0.05, None)]


def cost():
    names = ['fast pair', 'slow pair', 'f64_sin/canary', 'parse_u64/canary', 'btree_miss/canary', 'warm_dst/canary']
    print('\n5. COST per stopped trial: median set wall time in seconds (preparation included) / median rounds;'
          '\n   for exact answers, trials whose error exceeds their own threshold. Pooled over passes; c0 (c1 for the slow pair).')
    for label, goal, m in MODES:
        print(f'\n  {label}')
        print('  ' + ' ' * 18 + ''.join(f'{lap:>24}' for lap in LAPS))
        for name in names:
            row = f'  {name:18}'
            for lap in LAPS:
                ts = []
                for p in (0, 1):
                    rec = recording(lap, 1 if name == 'slow pair' else 0, p)
                    if rec is not None:
                        ts += trials(rec, name, goal, m)
                done = [t for t in ts if not t[4]]
                if not done:
                    row += f'{"(none stopped)":>24}'; continue
                c = st.median(t[3] for t in done) / 1e9
                r = st.median(t[2] for t in done)
                errs = [t for t in done if t[0] is not None]
                h = f' {sum(abs(t[0]) > t[1] for t in errs)}/{len(errs)}' if errs else ''
                row += f'{c:9.2f}s {r:5.0f}r{h:>7}'.rjust(24)
            print(row)


if __name__ == '__main__':
    have = sorted(glob.glob(f'{ROOT}/*/c*/p*/*.bin'))
    print(f'{len(have)} of {len(LAPS) * len(COMPS) * 2} recordings present')
    what = sys.argv[1:] or ['known', 'composition', 'laps', 'correlation', 'cost']
    for w in what:
        globals()[w]()
