# Multi-pass rules with each pass from a different process.
import sys, glob, math, pickle, os, statistics as st, itertools
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import paired
from rules import ivw, disagree, dl, ok
CLOCK = {'cpu_canary','f64_sin','str_find','urandom_read'}
W = ['btree_miss','copy_64mb','cpu_canary','f64_sin','str_find','urandom_read']
cond, t = sys.argv[1], float(sys.argv[2])
R = [pickle.load(open(f, 'rb')) for f in sorted(glob.glob(f'diag/{cond}-run*.pkl'))]
g = math.log1p(t); B = math.log1p(4*t)
from levels import levels
def tmw(v):
    v = sorted(v); k = len(v)//4; v = v[k:len(v)-k]; return sum(v)/len(v)
def sl(x, y):
    mx, my = st.mean(x), st.mean(y); return sum((p-mx)*(q-my) for p, q in zip(x, y))/sum((p-mx)**2 for p in x)
PRED = []
for r in R:
    L = levels(r); win = lambda v: [tmw(v[i:i+1000]) for i in range(0, len(v)-999, 1000)]
    c = win(L['cpu_canary']); csd = st.pstdev(c); be = {w: sl(c, win(L[w])) for w in L}
    PRED.append({(x, y): abs(be[x]-be[y])*csd for x in L for y in L})
res = {}
for a, b in itertools.combinations(W, 2):
    kind = 'clock' if a in CLOCK and b in CLOCK else 'other'
    RS = [paired.pair_rounds(r, a, b) for r in R]
    truth = st.mean(math.log(paired.ratio(rs)) for rs in RS)
    starts = range(0, min(len(rs) for rs in RS) - 6000, 4000)
    half = [[paired.trial(rs, s, math.sqrt(2)*g) for s in starts] for rs in RS]
    full = [[paired.trial(rs, s, g) for s in starts] for rs in RS]
    to = lambda tr: (math.log(tr[0]) - truth, tr[1], tr[2]) if tr[0] > 0 else (float('nan'), float('inf'), tr[2])
    acc = res.setdefault(kind, {k: [0, 0, 0, 0] for k in ('one pass', 'two, always', 'agree/RE', '+ wander check')})
    for P in full:
        for tr in P:
            x, s, n = to(tr); acc['one pass'][0] += 1; acc['one pass'][3] += n
            acc['one pass'][2] += math.isfinite(x) and abs(x) > B
    for i, j, k in itertools.permutations(range(len(R)), 3):
        for si in range(len(starts)):
            A, Bp, C = to(half[i][si]), to(half[j][si]), to(half[k][si])
            if not (ok(A) and ok(Bp)): continue
            x2, s2 = ivw([A, Bp]); n2 = A[2] + Bp[2]
            acc['two, always'][0] += 1; acc['two, always'][3] += n2; acc['two, always'][2] += abs(x2) > B
            wand = max(PRED[i][(a, b)], PRED[j][(a, b)]) > float(os.environ.get('WK', '1'))*g
            for nm in ('agree/RE', '+ wander check'):
                acc[nm][0] += 1
                if nm == '+ wander check' and wand:
                    acc[nm][1] += 1; acc[nm][3] += n2; continue
                if not disagree(A, Bp):
                    acc[nm][3] += n2; acc[nm][2] += abs(x2) > B
                else:
                    acc[nm][3] += n2 + C[2]
                    ps = [A, Bp] + ([C] if ok(C) else [])
                    x, s = dl(ps) if len(ps) == 3 else ivw(ps)
                    if s > g: acc[nm][1] += 1
                    else: acc[nm][2] += abs(x) > B
for kind, acc in res.items():
    base = acc['one pass'][3]/acc['one pass'][0]
    print(f'{cond} {100*t:.1f}% {kind:5}: ' + ' | '.join(f'{nm}: blown {100*v[2]/v[0]:5.2f}% refused {100*v[1]/v[0]:5.1f}% cost x{v[3]/v[0]/base:.2f}' for nm, v in acc.items()))
