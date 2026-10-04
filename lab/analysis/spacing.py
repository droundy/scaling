# What interleaving buys, measured from the lap recordings:
#  1. SPACING: rounds of one group sit further apart when other groups' rounds run in between.
#     Simulated by keeping every s-th round. Does round-to-round correlation fade, and the plain
#     standard error over rounds become honest (batch-means SE / plain SE -> 1)?
#  2. CLOSENESS: within a round, how much a pair's per-round log ratio scatters as a function of how
#     far apart in time its two samples ran. That is what a short group round buys.
import sys, os, glob, math, statistics as st
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lap_score import PREP_NS, PER_SAMPLE_NS, LAPS, meas_one

ROOT = os.environ.get('LAPS', os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'day', 'collect', 'laps'))
COMP_DEFAULT = int(os.environ.get('COMP', 0))
PAIRS = [('slow_cpu', 'slow_cpu2'), ('f64_sin', 'cpu_canary'), ('parse_u64', 'cpu_canary'),
         ('btree_miss', 'cpu_canary'), ('warm_dst', 'cpu_canary')]
SPACINGS = [1, 2, 4, 8, 16, 32]


def load_ordered(path):
    """Rounds in execution order: each a list of (workload, n, [lap0..lap3], start_ns within the round)."""
    b = open(path, 'rb').read()
    i = b.index(b'DATA\n')
    names = []
    for line in b[:i].decode().splitlines():
        if line.startswith('# rung '):
            f = line.split()
            names.append((f[2], int(f[3])))
    body = b[i + 5:]
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
    W = len(names)
    nlaps = {}
    for name, _ in names:
        w = name.rsplit('.lap', 1)[0]
        nlaps[w] = nlaps.get(w, 0) + 1
    rounds = []
    for r in range(len(rec) // W):
        samples, cur, t = [], None, 0.0
        for idx, v in rec[r * W:(r + 1) * W]:
            name, n = names[idx]
            w, lap = name.rsplit('.lap', 1)
            if lap == '0':
                cur = [w, [n], [v], t]
                samples.append(cur)
            else:
                cur[1].append(n); cur[2].append(v)
            if int(lap) == nlaps[w] - 1:
                t += sum(cur[2]) + PREP_NS.get(w, 0.0) * sum(cur[1]) + PER_SAMPLE_NS
        rounds.append(samples)
    return rounds


def per_round(rounds, a, b):
    """Per-round ln(measA / measB) and the separation (ms) between the two samples' starts."""
    v, sep = [], []
    for samples in rounds:
        d = {s[0]: s for s in samples}
        if a not in d or b not in d:
            continue
        (_, na, la, ta), (_, nb, lb, tb) = d[a][:4], d[b][:4]
        ma, mb = meas_one(na, la), meas_one(nb, lb)
        if ma > 0 and mb > 0:
            v.append(math.log(ma / mb)); sep.append(abs(ta - tb) / 1e6)
    return v, sep


def lag1(v):
    mu = st.mean(v); d = [x - mu for x in v]
    den = sum(x * x for x in d)
    return sum(p * q for p, q in zip(d, d[1:])) / den if den else float('nan')


def bm_ratio(v, k=5):
    nb = len(v) // k
    if nb < 8:
        return float('nan')
    bm = [st.mean(v[i * k:(i + 1) * k]) for i in range(nb)]
    return (st.stdev(bm) / math.sqrt(nb)) / (st.stdev(v) / math.sqrt(len(v)))


def robust_sd(v):
    m = st.median(v)
    return 1.4826 * st.median(abs(x - m) for x in v)


def spacing(comp=0, p=0):
    print(f'\n1. SPACING (c{comp} p{p}): keep every s-th round. Each cell: lag-1 autocorrelation / batch-means SE over plain SE,'
          '\n   averaged over the s phase offsets. Round spacing in seconds is s x the round time shown.')
    for lap in LAPS:
        g = glob.glob(f'{ROOT}/{lap}/c{comp}/p{p}/*.bin')
        if not g or os.path.getsize(g[0]) == 0:
            continue
        rounds = load_ordered(g[0])
        rt = st.median(sum(sum(s[2]) for s in r) for r in rounds) / 1e9
        print(f'\n  {lap}: {len(rounds)} rounds, ~{rt:.3f} s a round (timed only)')
        print('  pair                    ' + ''.join(f'{"s=" + str(s):>15}' for s in SPACINGS))
        for a, b in PAIRS:
            v, _ = per_round(rounds, a, b)
            cells = []
            for s in SPACINGS:
                rs, bs = [], []
                for ph in range(s):
                    sub = v[ph::s]
                    if len(sub) >= 20:
                        rs.append(lag1(sub))
                        x = bm_ratio(sub)
                        if x == x:
                            bs.append(x)
                cells.append(f'{st.mean(rs):+.2f}/{st.mean(bs):.2f}' if rs and bs else (f'{st.mean(rs):+.2f}/  - ' if rs else '      -     '))
            print(f'  {a + "/" + b:24}' + ''.join(f'{c:>15}' for c in cells))


def closeness(comp=0, p=0):
    print(f'\n2. CLOSENESS (c{comp} p{p}): robust sd of the per-round log ratio, %, by how far apart the pair ran'
          '\n   within the round (thirds of the rounds by separation; median separation in ms in brackets)')
    for lap in LAPS:
        g = glob.glob(f'{ROOT}/{lap}/c{comp}/p{p}/*.bin')
        if not g or os.path.getsize(g[0]) == 0:
            continue
        rounds = load_ordered(g[0])
        print(f'\n  {lap}')
        for a, b in PAIRS:
            v, sep = per_round(rounds, a, b)
            if len(v) < 30:
                continue
            # remove slow drift so only within-round scatter is compared: subtract a running median
            k = 10
            resid = [v[i] - st.median(v[max(0, i - k):i + k + 1]) for i in range(len(v))]
            order = sorted(range(len(v)), key=lambda i: sep[i])
            thirds = [order[i * len(v) // 3:(i + 1) * len(v) // 3] for i in range(3)]
            cells = [f'{100 * robust_sd([resid[i] for i in t]):.3f} ({st.median(sep[i] for i in t):6.1f})' for t in thirds]
            print(f'  {a + "/" + b:24}' + ''.join(f'{c:>22}' for c in cells))


if __name__ == '__main__':
    comp = int(os.environ.get('COMP', 0))
    spacing(comp)
    closeness(comp)
