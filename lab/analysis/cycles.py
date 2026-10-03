import sys, glob, math, pickle, os, statistics as st, csv
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import paired
from rounds import load, COLLECT
W = ['cpu_canary','f64_sin','str_find','urandom_read','btree_miss','copy_64mb']
def get(cond):
    out = []
    for d in sorted(glob.glob(f'{COLLECT}/{cond}/run*')):
        pk = f'diag/{cond}-{os.path.basename(d)}.pkl'
        if not os.path.exists(pk): pickle.dump(load(glob.glob(d + '/*.bin')[0]), open(pk, 'wb'))
        out.append((d, pickle.load(open(pk, 'rb'))))
    return out
def ns(per, w):
    ns_ = sorted({n for n, _ in per[w]})
    if len(ns_) < 2: return float('nan')
    a, b = ns_[-2], ns_[-1]
    ta = paired.tmean([t for n, t in per[w] if n == a]); tb = paired.tmean([t for n, t in per[w] if n == b])
    return (tb - ta)/(b - a)
for cond in sys.argv[1:] or ['battery-noisy', 'battery-quiet']:
    R = get(cond)
    print(f'== {cond}: per process, ns per iteration and cycles (= 4 x ratio to the canary)')
    for w in W:
        nsv = [ns(p, w) for _, p in R]
        cyc = [4*paired.ratio(paired.pair_rounds(p, w, 'cpu_canary')) if w != 'cpu_canary' else 4.0 for _, p in R]
        sp = lambda v: 100*st.pstdev(v)/st.mean(v)
        print(f'  {w:12} ns: ' + ' '.join(f'{x:10.3f}' for x in nsv) + f'  spread {sp(nsv):5.2f}%   cycles: ' + ' '.join(f'{x:10.1f}' for x in cyc) + f'  spread {sp(cyc):5.2f}%')
    # canary cycles per link from logged frequency: per-second median canary ns/link x GHz
    for d, p in R:
        rows = list(csv.DictReader(open(d + '/machine.csv')))
        top = max(n for n, _ in p['cpu_canary'])
        c = []
        for r0, r1 in zip(rows, rows[1:]):
            a, b = int(r0['round']), int(r1['round'])
            v = [t/n for n, t in p['cpu_canary'][a:b] if n == top]
            if len(v) < 5 or r0['cpu'] != r1['cpu'] or abs(int(r0['khz']) - int(r1['khz'])) > 50000: continue
            c.append(st.median(v) * int(r0['khz'])/1e6)
        if c:
            c.sort()
            print(f'  {os.path.basename(d)}: canary cycles/link in steady seconds: median {st.median(c):.3f}, 10-90% {c[len(c)//10]:.3f}-{c[9*len(c)//10]:.3f} (n={len(c)})')
