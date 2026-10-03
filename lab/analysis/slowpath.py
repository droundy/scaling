# The slow-function path: one rung (n=1), nanoseconds, blocks as small as one call.
import glob, math, statistics as st, pickle, sys
from rounds import load, COLLECT
T1 = [1.8373,1.3213,1.1969,1.1416,1.1105,1.0906,1.0767,1.0665,1.0587,1.0526,1.0476,1.0434,1.04,1.037,1.0345,1.0322,1.0303,1.0286,1.027]
TRIM = 0.25
def tmean(v):
    w = sorted(v); c = min(int(len(w)*TRIM), (len(w)-1)//2); w = w[c:len(w)-c]; return sum(w)/len(w)
def se_rel(v):
    # winsorise at the trim, batch means over b = clamp(len, minb, 20) blocks, / (1 - 2 trim); relative to the estimate
    n = len(v); w = sorted(v); c = min(int(n*TRIM), (n-1)//2); lo, hi = w[c], w[n-1-c]
    x = [min(max(a, lo), hi) for a in v]
    b = min(n, 20); per = n//b
    m = [sum(x[i*per:(i+1)*per])/per for i in range(b)]
    mu = sum(m)/b; var = sum((a-mu)**2 for a in m)/(b-1)
    return math.sqrt(var/b)/(1-2*TRIM)/tmean(v), b
def chi_factor(b, z):
    k = b-1; c = 2/(9*k); q = k*max(1-c-z*math.sqrt(c), 1e-6)**3; return math.sqrt(k/q)
def trial(v, s, goal, floor, mode):
    n = floor
    while True:
        seg = v[s:s+n]
        if len(seg) < n: return None
        est = tmean(seg); r, b = se_rel(seg)
        t = T1[b-2] if b >= 2 else 9
        bar = r*t if mode != 'plain' else r
        cut = r*chi_factor(b, 1.282) if mode == 'chi90' else bar
        if cut <= goal or n >= 400:
            return est, bar, n, n >= 400 and cut > goal
        n = max(n+1, int(n*1.3))
sources = []
for p in '012':
    per = pickle.load(open(f'diag/quiet{p}.pkl', 'rb'))
    for w in ('copy_64mb', 'str_find'):
        sources.append((f'quiet{p} {w}', [t for n, t in per[w] if n == 1]))
for d in ('i4000000', 'i250000'):
    per = load(glob.glob(f'{COLLECT}/slowq/{d}/*.bin')[0])
    sources.append((f'pinned slow_cpu {d}', [t for n, t in per['slow_cpu'] if n == 1]))
modes = [('plain', 8), ('t', 8), ('chi90', 8), ('chi90', 4), ('chi90', 6), ('plain', 20)]
for goal in (0.01, 0.005):
    g = goal; B = 4*goal
    print(f'\n== goal {100*goal:.1f}%  (truth = trimmed mean of the whole recording; blowup = off by more than 4x the goal)')
    for name, v in sources:
        truth = tmean(v)
        line = f'  {name:28} {len(v):6} calls, {st.median(v)/1e6:5.2f} ms:'
        for mode, floor in modes:
            outs = []; s = 0
            while len(outs) < 300:
                o = trial(v, s, g, floor, mode)
                if o is None: break
                outs.append(o); s += o[2]
            off = [abs(o[0]/truth - 1) for o in outs]
            N = len(outs)
            line += f' | {mode}/{floor}: blow {sum(x > B for x in off)}/{N} cov {100*sum(x <= o[1] for x, o in zip(off, outs))/N:3.0f}% calls {st.mean(o[2] for o in outs):5.1f}'
        print(line)
