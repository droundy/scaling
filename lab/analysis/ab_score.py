# Scores context (A), keep-alive + probe (C) and long samples (B1/B3/B10) from lab/day/collect/ab,
# without assuming any mechanism:
#   1. known answers: chains whose ratio is exact by construction;
#   2. composition: how far each function moves when neighbours are added, against pass-to-pass noise;
#   3. cost: wall time a set spends, preparation included, to reach the Bonferroni target;
#   4. honesty: how often a stopped trial's error exceeds its own threshold.
import sys, os, glob, math, statistics as st
from functools import lru_cache

ROOT = os.environ.get('AB', os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'day', 'collect', 'ab'))
CANDS = ['A', 'C', 'B1', 'B3', 'B10']
COMPS = range(6)
# The fast chains are 100 and 64 links a call, but each call also pays ~1.4 links of call overhead,
# so their ratio is not 100/64. Against the canary - the same chain, link for link - their
# difference is exactly 36 links whatever that overhead is, as long as it is the same for both.
FAST = ('slow_cpu', 'slow_cpu2', 36)
SLOW = ('slow_cpu3', 'slow_cpu4', 3810000 / 2286000)
BASE = ['cpu_canary', 'slow_cpu', 'slow_cpu2', 'f64_sin', 'parse_u64', 'btree_miss', 'warm_dst']
NEIGHBOUR = {1: 'long integer calls', 2: 'fp_heavy', 3: 'cache thrasher', 4: 'warmer', 5: 'all of them'}
TRIM = 0.25
GOAL = math.log1p(0.01)       # a 1% goal ...
TAIL = 0.05 / 10 / 2          # ... at a 5% family-wise rate over 10 comparisons, two-sided
TRIALS = 300


# ---- reading -------------------------------------------------------------------------------

def load(path):
    """Per workload: [(n, ns)] per round, and the untimed cost of a sample at each n."""
    b = open(path, 'rb').read()
    i = b.index(b'DATA\n')
    head, body = b[:i].decode().splitlines(), b[i + 5:]
    order, ovh = [], {}
    for line in head:
        if line.startswith('# rung '):
            f = line.split()
            w, n = f[2].split('@')[0], int(f[3])
            order.append((w, n))
            ovh.setdefault(w, {})[n] = float(f[4])
    W = len({w for w, _ in order})
    rec, j = [], 0
    while j + 1 < len(body):
        idx = body[j]; j += 1
        v = sh = 0
        while True:
            c = body[j]; j += 1
            v |= (c & 0x7f) << sh; sh += 7
            if not c & 0x80:
                break
        rec.append((idx, v))
    R = len(rec) // W
    per = {w: [None] * R for w, _ in order}
    for r in range(R):
        for k in range(W):
            idx, v = rec[r * W + k]
            w, n = order[idx]
            per[w][r] = (n, v)
    return per, ovh


@lru_cache(maxsize=None)
def recording(cand, comp, p):
    g = glob.glob(f'{ROOT}/{cand}/c{comp}/p{p}/*.bin')
    if not g or os.path.getsize(g[0]) == 0:
        return None
    try:
        return load(g[0])
    except (ValueError, IndexError):
        return None   # still being written


# ---- estimators ----------------------------------------------------------------------------

def tmean(v):
    v = sorted(v); n = len(v); c = min(int(n * TRIM), (n - 1) // 2)
    w = v[c:n - c]
    return sum(w) / len(w)


def solve(A, y):
    n = len(y)
    M = [row[:] + [y[i]] for i, row in enumerate(A)]
    for c in range(n):
        p = max(range(c, n), key=lambda r: abs(M[r][c]))
        if abs(M[p][c]) < 1e-300:
            return None
        M[c], M[p] = M[p], M[c]
        for r in range(n):
            if r != c:
                f = M[r][c] / M[c][c]
                for k in range(c, n + 1):
                    M[r][k] -= f * M[c][k]
    return [M[i][n] / M[i][i] for i in range(n)]


def slope_of(levels):
    """Per-iteration cost from batch-time levels (up to a shared factor): subtract two, or divide one."""
    ns = sorted(levels)
    if len(ns) == 1:
        return math.exp(levels[ns[0]]) / ns[0]
    n1, n2 = ns[-2], ns[-1]
    return (math.exp(levels[n2]) - math.exp(levels[n1])) / (n2 - n1)


def ratio(rs):
    """A's per-iteration cost over B's: trimmed cell means of ln tA - ln tB, fitted as alpha(nA) - beta(nB)."""
    cells = {}
    for na, ta, nb, tb in rs:
        cells.setdefault((na, nb), []).append(math.log(ta) - math.log(tb))
    xs = sorted({k[0] for k in cells}); ys = sorted({k[1] for k in cells})
    if len(cells) < len(xs) + len(ys) - 1:
        return float('nan')
    # unknowns: alpha for each xs, beta for ys[1:] (beta of ys[0] fixed at 0)
    nu = len(xs) + len(ys) - 1
    A = [[0.0] * nu for _ in range(nu)]; y = [0.0] * nu
    for (x, yy), v in cells.items():
        m, c = tmean(v), len(v)
        row = [0.0] * nu
        row[xs.index(x)] = 1.0
        if ys.index(yy) > 0:
            row[len(xs) + ys.index(yy) - 1] = -1.0
        for i in range(nu):
            y[i] += c * row[i] * m
            for k in range(nu):
                A[i][k] += c * row[i] * row[k]
    s = solve(A, y)
    if s is None:
        return float('nan')
    al = {x: s[i] for i, x in enumerate(xs)}
    be = {ys[0]: 0.0, **{yy: s[len(xs) + i - 1] for i, yy in enumerate(ys) if i > 0}}
    a, b = slope_of(al), slope_of(be)
    return a / b if a > 0 and b > 0 else float('nan')


def absolute(samples):
    """Per-iteration ns: trimmed-mean subtraction of the top two rungs, or one rung divided."""
    by = {}
    for n, t in samples:
        by.setdefault(n, []).append(t)
    ns = sorted(by)
    if len(ns) == 1:
        return tmean(by[ns[0]]) / ns[0]
    n1, n2 = ns[-2], ns[-1]
    if len(by[n1]) < 2 or len(by[n2]) < 2:
        return float('nan')
    return (tmean(by[n2]) - tmean(by[n1])) / (n2 - n1)


def block_rounds(rs):
    ra = len({r[0] for r in rs}); rb = len({r[2] for r in rs})
    return {(2, 2): 15, (1, 2): 6, (2, 1): 6, (1, 1): 1}.get((min(ra, 2), min(rb, 2)), 15)


def bar(rs, est, k):
    """Batch means on the log scale: (sigma, blocks), or (inf, 0)."""
    b = max(8, min(20, len(rs) // k)); per = len(rs) // b
    if per < k:
        return float('inf'), 0
    e = []
    for i in range(b):
        x = est(rs[i * per:(i + 1) * per])
        if not x > 0:
            return float('inf'), 0
        e.append(math.log(x))
    m = sum(e) / b
    return math.sqrt(sum((x - m) ** 2 for x in e) / (b - 1) / b), b


# ---- Student's t at the Bonferroni tail ----------------------------------------------------

def tpdf(x, k):
    return math.exp(math.lgamma((k + 1) / 2) - math.lgamma(k / 2)) / math.sqrt(k * math.pi) * (1 + x * x / k) ** (-(k + 1) / 2)


@lru_cache(maxsize=None)
def tq(k):
    """Upper TAIL quantile of t with k degrees of freedom."""
    lo, hi = 0.0, 80.0
    for _ in range(60):
        m = (lo + hi) / 2
        N = 4000; h = m / N
        tail = 0.5 - sum(tpdf((i + 0.5) * h, k) for i in range(N)) * h
        if tail > TAIL:
            lo = m
        else:
            hi = m
    return m


# ---- trials ---------------------------------------------------------------------------------

def pair(per, a, b):
    return [(na, ta, nb, tb) for (na, ta), (nb, tb) in zip(per[a], per[b]) if ta > 0 and tb > 0]


def triple(per, a, b, c):
    return [(na, ta, nb, tb, nc, tc) for (na, ta), (nb, tb), (nc, tc) in zip(per[a], per[b], per[c])
            if ta > 0 and tb > 0 and tc > 0]


def fast_diff(rows):
    """(R(a/canary) - R(b/canary)) / 36: exactly 1 for the fast chains."""
    ra = ratio([(r[0], r[1], r[4], r[5]) for r in rows])
    rb = ratio([(r[2], r[3], r[4], r[5]) for r in rows])
    return (ra - rb) / FAST[2]


def quantity(per, name):
    """(rows, estimator, rounds per block, exact answer or None, members) for a scored quantity."""
    if name == 'fast pair':
        return triple(per, 'slow_cpu', 'slow_cpu2', 'cpu_canary'), fast_diff, 15, 1.0, ['slow_cpu', 'slow_cpu2', 'cpu_canary']
    if name == 'slow pair':
        rs = pair(per, 'slow_cpu3', 'slow_cpu4')
        return rs, ratio, block_rounds(rs), SLOW[2], ['slow_cpu3', 'slow_cpu4']
    w = name.split('/')[0]
    rs = pair(per, w, 'cpu_canary')
    return rs, ratio, block_rounds(rs), None, [w, 'cpu_canary']


def cost_ns(per, ovh, members, r0, r1):
    s = 0.0
    for w in members:
        for n, t in per[w][r0:r1]:
            s += t + ovh[w][n]
    return s


def trials(per, ovh, name):
    """Stopped trials laid end to end. Each: (ln error or None, sigma, threshold, rounds, cost_ns, capped)."""
    rows, est, k, truth, members = quantity(per, name)
    out, s = [], 0
    while s + 8 * k <= len(rows) and len(out) < TRIALS:
        n = 8 * k
        while True:
            seg = rows[s:s + n]
            capped = len(seg) < n
            r = est(seg)
            sig, blocks = bar(seg, est, k)
            thr = tq(blocks - 1) * sig if blocks else float('inf')
            if capped or (r > 0 and thr <= GOAL):
                break
            n = int(n * 1.3) + 1
        err = math.log(r) - math.log(truth) if truth and r > 0 else None
        out.append((err, sig, thr, len(seg), cost_ns(per, ovh, members, s, s + len(seg)), capped))
        s += len(seg)
    return out


def summarize(ts):
    done = [t for t in ts if not t[5]]
    if not done:
        return None
    errs = [t for t in done if t[0] is not None]
    over = sum(abs(t[0]) > t[2] for t in errs)
    return dict(n=len(done), capped=len(ts) - len(done),
                rounds=st.median(t[3] for t in done),
                cost_ms=st.median(t[4] for t in done) / 1e6,
                over=over, nerr=len(errs),
                med_err=st.median(t[0] for t in errs) if errs else None)


# ---- reports --------------------------------------------------------------------------------

def pc(x):
    return '   -   ' if x is None or x != x else f'{100 * x:+7.3f}'


def known():
    print('\n1. KNOWN ANSWERS: whole-run error against the exact answer, % (whole-run bar in brackets); p0 / p1')
    for name, label, need in (('fast pair', 'fast pair: 100 - 64 links against the canary, exactly 36', 'slow_cpu'),
                              ('slow pair', 'slow pair: 9 / 5.4 ms, exactly 5/3', 'slow_cpu3')):
        print(f'\n  {label}')
        print('  cand ' + ''.join(f'{"c" + str(c):>28}' for c in COMPS))
        for cand in CANDS:
            row = f'  {cand:4} '
            for c in COMPS:
                cell = []
                for p in (0, 1):
                    rec = recording(cand, c, p)
                    if rec is None or need not in rec[0]:
                        cell.append('       -     ')
                        continue
                    rows, est, k, truth, _ = quantity(rec[0], name)
                    r = est(rows)
                    sig, _ = bar(rows, est, k)
                    cell.append(f'{pc(math.log(r / truth)) if r > 0 else "   nan "}({100 * sig:.3f})')
                row += f'{" / ".join(cell):>28}'
            print(row)


def composition():
    print('\n2. COMPOSITION: shift from c0 when a neighbour is added, %, mean of the two passes;'
          '\n   null = rms pass-to-pass difference / sqrt 2 over all compositions')
    for kind in ('absolute ns', 'ratio to canary'):
        print(f'\n  {kind}')
        print('  cand fn          ' + ''.join(f'{"+" + NEIGHBOUR[c][:14]:>17}' for c in range(1, 6)) + '     null')
        for cand in CANDS:
            for w in BASE:
                if kind == 'ratio to canary' and w == 'cpu_canary':
                    continue
                val = {}
                for c in COMPS:
                    for p in (0, 1):
                        rec = recording(cand, c, p)
                        if rec is None:
                            continue
                        per = rec[0]
                        x = absolute(per[w]) if kind == 'absolute ns' else ratio(pair(per, w, 'cpu_canary'))
                        if x > 0:
                            val[(c, p)] = math.log(x)
                def m(c):
                    v = [val[(c, p)] for p in (0, 1) if (c, p) in val]
                    return sum(v) / len(v) if v else None
                d = [val[(c, 0)] - val[(c, 1)] for c in COMPS if (c, 0) in val and (c, 1) in val]
                null = math.sqrt(sum(x * x for x in d) / len(d) / 2) if d else None
                base = m(0)
                cells = ''.join(f'{pc(m(c) - base) if base is not None and m(c) is not None else "   -   ":>17}' for c in range(1, 6))
                print(f'  {cand:4} {w:11} {cells}  {pc(null)}')
            print()


def cost_and_honesty():
    print('\n3-4. COST to the Bonferroni target (1% goal, t at the blocks, 10 comparisons) and HONESTY;'
          '\n   median set wall time per stopped trial in ms (preparation included), median rounds,'
          '\n   and for the known pair: stopped trials whose error exceeds their own threshold')
    names = ['fast pair', 'slow pair', 'f64_sin/canary', 'parse_u64/canary', 'btree_miss/canary', 'warm_dst/canary']
    for c in (0, 1, 5):
        print(f'\n  composition c{c}')
        for name in names:
            if name == 'slow pair' and c not in (1, 5):
                continue
            row = f'  {name:18}'
            for cand in CANDS:
                ts = []
                for p in (0, 1):
                    rec = recording(cand, c, p)
                    if rec is not None:
                        ts += trials(*rec, name)
                s = summarize(ts)
                if s is None:
                    row += f'{cand:>5}: {"-":>24}'
                    continue
                h = f' over {s["over"]}/{s["nerr"]}' if s['nerr'] else ''
                row += f'{cand:>5}: {s["cost_ms"]:8.1f} ms {s["rounds"]:6.0f} r{h:14}'
            print(row)


if __name__ == '__main__':
    have = sorted(glob.glob(f'{ROOT}/*/c*/p*/*.bin'))
    print(f'{len(have)} of {len(CANDS) * 6 * 2} recordings present under {ROOT}')
    what = sys.argv[1:] or ['known', 'composition', 'cost']
    if 'known' in what:
        known()
    if 'composition' in what:
        composition()
    if 'cost' in what:
        cost_and_honesty()
