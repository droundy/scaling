# The lab's paired ratio estimator, bar and stopping rule, in Python (mirrors replay.rs).
import math
from rounds import load
TRIM = 0.25
def tmean(v):
    v = sorted(v); n = len(v); c = min(int(n*TRIM), (n-1)//2)
    w = v[c:n-c]; return sum(w)/len(w)
def pair_rounds(per, a, b):
    A, B = per[a], per[b]
    return [(na, ta, nb, tb) for (na, ta), (nb, tb) in zip(A, B) if ta > 0 and tb > 0]
def ratio(rs):
    cells = {}
    for na, ta, nb, tb in rs:
        cells.setdefault((na, nb), []).append(math.log(ta) - math.log(tb))
    ns = sorted({k[0] for k in cells}); ms = sorted({k[1] for k in cells})
    if len(ns) < 2 or len(ms) < 2 or len(cells) < 3: return float('nan')
    d = {k: (tmean(v), len(v)) for k, v in cells.items()}
    al = {x: 0.0 for x in ns}; be = {y: 0.0 for y in ms}
    for _ in range(60):
        for x in ns:
            s = w = 0.0
            for y in ms:
                if (x, y) in d: m, c = d[(x, y)]; s += c*(m+be[y]); w += c
            if w: al[x] = s/w
        for y in ms:
            s = w = 0.0
            for x in ns:
                if (x, y) in d: m, c = d[(x, y)]; s += c*(al[x]-m); w += c
            if w: be[y] = s/w
    n1, n2 = ns[-2], ns[-1]; m1, m2 = ms[-2], ms[-1]
    ba = (math.exp(al[n2]) - math.exp(al[n1]))/(n2-n1)
    bb = (math.exp(be[m2]) - math.exp(be[m1]))/(m2-m1)
    return ba/bb if ba > 0 and bb > 0 else float('nan')
def bar(rs, minb=8, per_min=15):
    b = max(minb, min(20, len(rs)//per_min)); per = len(rs)//b
    if per < per_min: return float('inf')
    e = []
    for i in range(b):
        r = ratio(rs[i*per:(i+1)*per])
        if not (r > 0): return float('inf')
        e.append(math.log(r))
    m = sum(e)/b
    return math.sqrt(sum((x-m)**2 for x in e)/(b-1)/b)
def trial(rs, start, goal_ln, cap=4000):
    n = 120
    while True:
        seg = rs[start:start+n]
        r, s = ratio(seg), bar(seg)
        if len(seg) < n: return r, s, len(seg), True
        if r > 0 and s <= goal_ln: return r, s, n, False
        if n >= cap: return r, s, n, True
        n = int(n*1.3)+1
