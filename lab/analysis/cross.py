# Separate processes as passes: does one process's ratio agree with another's?
import sys, glob, math, pickle, os, statistics as st, itertools
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import paired
from rounds import load, COLLECT
CLOCK = {'cpu_canary','f64_sin','str_find','urandom_read'}
W = ['btree_miss','copy_64mb','cpu_canary','f64_sin','str_find','urandom_read']
def runs(cond):
    out = []
    for d in sorted(glob.glob(f'{COLLECT}/{cond}/run*')):
        f = glob.glob(d + '/*.bin')[0]
        pk = f'diag/{cond}-{os.path.basename(d)}.pkl'
        if not os.path.exists(pk): pickle.dump(load(f), open(pk, 'wb'))
        out.append(pickle.load(open(pk, 'rb')))
    return out
cond = sys.argv[1]
only = sys.argv[2] if len(sys.argv) > 2 else None
R = runs(cond)
print(f'== {cond}: {len(R)} processes, rounds {[len(r["cpu_canary"]) for r in R]}')
# canary per-iteration cost per process (trimmed, top rung)
for i, r in enumerate(R):
    top = max(n for n, _ in r['cpu_canary'])
    v = [t/n for n, t in r['cpu_canary'] if n == top]
    print(f'  process {i+1}: canary {paired.tmean(v):.4f} ns/link at n={top}')
print('\n  A. whole-process ratio per process; spread between processes vs the bar each claims')
print(f'  {"pair":28} {"ratio per process":>44}  between sd  mean bar  ratio')
rows = []
for a, b in itertools.combinations(W, 2):
    lr, bs = [], []
    for r in R:
        rs = paired.pair_rounds(r, a, b)
        lr.append(math.log(paired.ratio(rs))); bs.append(paired.bar(rs))
    sd = st.stdev(lr); mb = st.mean(bs)
    kind = 'clock' if a in CLOCK and b in CLOCK else 'other'
    rows.append((kind, a, b, lr, bs))
    print(f'  {a+"/"+b:28} ' + ' '.join(f'{math.exp(x - lr[0])*100-100:+6.2f}%' for x in lr) + f'   {100*sd:6.2f}%   {100*mb:6.2f}%  {sd/mb:5.1f}x  {kind}')
print('  (ratios shown relative to process 1)')
if only is None: sys.exit()
print('\n  B. the two-pass rule across processes: pass = a trial at the start of a process, goal sqrt2 looser;')
print('     truth = mean over all processes of the whole-process ratio')
for kind in ('clock', 'other'):
    for t in [float(only)]:
        g = math.log1p(t); B = math.log1p(4*t)
        n = dis = blow1 = blowc = 0
        for _, a, b, lr, _ in [x for x in rows if x[0] == kind]:
            pn = pd = pb = 0; p1 = len([1]) * 0
            truth = st.mean(lr)
            # several trials per process: starts every 5000 rounds
            P = []
            for r in R:
                rs = paired.pair_rounds(r, a, b)
                P.append([paired.trial(rs, s, math.sqrt(2)*g) for s in range(0, len(rs)-6000, 3000)])
            single = [paired.trial(paired.pair_rounds(R[0], a, b), s, g) for s in range(0, len(paired.pair_rounds(R[0], a, b))-6000, 3000)]
            blow1 += sum(abs(math.log(x[0]) - truth) > B for x in single if x[0] > 0)
            for i, j in itertools.combinations(range(len(R)), 2):
                for x, y in zip(P[i], P[j]):
                    if not (x[0] > 0 and y[0] > 0): continue
                    x1, s1, x2, s2 = math.log(x[0]), x[1], math.log(y[0]), y[1]
                    n += 1
                    dis += abs(x1 - x2) > 2*math.sqrt(s1*s1 + s2*s2)
                    w1, w2 = 1/s1**2, 1/s2**2
                    c = (w1*x1 + w2*x2)/(w1 + w2)
                    blowc += abs(c - truth) > B
                    pn += 1; pd += abs(x1 - x2) > 2*math.sqrt(s1*s1 + s2*s2); pb += abs(c - truth) > B
            sb = sum(abs(math.log(x[0]) - truth) > B for x in single if x[0] > 0)
            print(f'      {a+"/"+b:26} single: {sb}/{len(single)} blown   two-pass: disagree {pd}/{pn}, combined blown {pb}/{pn}')
        print(f'    {kind:5} {100*t:.1f}%: pass pairs {n:4}  disagree {100*dis/max(n,1):5.1f}%   combined blowups {blowc:3}   (one full-goal pass from process 1: {blow1} blowups)')
