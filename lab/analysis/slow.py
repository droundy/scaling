# slow_cpu against the canary: the true ratio is exactly LAB_SLOW_ITERS.
import sys, glob, os, math, pickle, statistics as st, re
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import paired
from rounds import load, COLLECT
def ratio_one(rs):
    # slow_cpu on its n=1 rung only, no subtraction; canary still subtracted.
    cells = {}
    for na, ta, nb, tb in rs:
        if na != 1: continue
        cells.setdefault(nb, []).append(math.log(ta) - math.log(tb))
    if len(cells) < 2: return float('nan')
    (n1, v1), (n2, v2) = sorted(cells.items())[-2:]
    m1, m2 = paired.tmean(v1), paired.tmean(v2)
    d = math.exp(-m2) - math.exp(-m1)
    return (n2 - n1)/d if d > 0 else float('nan')
def bar(rs, est):
    b = max(8, min(20, len(rs)//15)); per = len(rs)//b
    if per < 15: return float('inf')
    e = [est(rs[i*per:(i+1)*per]) for i in range(b)]
    if not all(x > 0 for x in e): return float('inf')
    e = [math.log(x) for x in e]; m = st.mean(e)
    return math.sqrt(sum((x-m)**2 for x in e)/(b-1)/b)
def trial(rs, s, goal, est):
    n = 120
    while True:
        seg = rs[s:s+n]
        r, b = est(seg), bar(seg, est)
        if len(seg) < n: return r, b, len(seg), True
        if r > 0 and b <= goal: return r, b, n, False
        if n >= 4000: return r, b, n, True
        n = int(n*1.3) + 1
for d in sorted(glob.glob(COLLECT + '/slow/i*'), key=lambda p: (int(re.search(r'i(\d+)', p).group(1)), p)):
    it = int(re.search(r'i(\d+)', d).group(1))
    per = load(glob.glob(d + '/*.bin')[0])
    rs = paired.pair_rounds(per, 'slow_cpu', 'cpu_canary')
    t1 = [t for n, t in per['slow_cpu'] if n == 1]
    ms = st.median(t1)/1e6
    two = paired.ratio(rs); one = ratio_one(rs)
    # noise: sd of per-round log ratio at n=1 against canary, within cells
    print(f'{os.path.basename(d):12} {len(rs):5} rounds  one call {ms:6.2f} ms  '
          f'excess over the exact ratio: two-rung {100*(two/it-1):+.3f}%  one-rung {100*(one/it-1):+.3f}%   '
          f'whole-run bar: two {100*bar(rs, paired.ratio):.3f}%  one {100*bar(rs, ratio_one):.3f}%')
    for goal in (0.01, 0.005):
        g = math.log1p(goal); B = math.log1p(4*goal)
        for nm, est in (('two-rung', paired.ratio), ('one-rung', ratio_one)):
            outs = []; s = 0
            while s + 120 <= len(rs) and len(outs) < 60:
                o = trial(rs, s, g, est); outs.append(o); s += o[2]
            truth = math.log(it)
            good = [o for o in outs if o[0] > 0]
            off = [math.log(o[0]) - truth for o in good]
            cost = st.mean(o[2] for o in outs) * (1.5 if nm == 'two-rung' else 1.0)
            print(f'      goal {100*goal:.1f}% {nm}: trials {len(outs):3} within {100*sum(abs(x) <= g for x in off)/len(outs):3.0f}%  '
                  f'cover {100*sum(abs(x) <= o[1] for x, o in zip(off, good))/len(outs):3.0f}%  blown {sum(abs(x) > B for x in off)}  '
                  f'median offset {100*st.median(off):+.3f}%  cost {cost:6.0f} calls')
