# Two candidate fixes for memory pairs' over-confident bars, scored on single passes:
# (a) variance ratio: also take the bar from blocks twice as long (adjacent blocks merged)
#     and use the larger; (b) a floor on sigma.
import glob, math, sys, statistics as st
sys.path.insert(0, __import__('os').path.dirname(__import__('os').path.abspath(__file__)))
from verdicts import tq, blocks, CLOCK, P, Z
rows = []
for fn in sorted(glob.glob(sys.argv[1] + '/*.tsv')):
    mach = fn.split('/')[-1][:5]
    for l in open(fn):
        f = l.rstrip('\n').split('\t')
        if f[3] != 'paired' or not f[14]: continue
        bl = [float(v) for v in f[14].split(',')]
        x, se, n = float(f[8]), float(f[9]), int(f[6])
        if not (math.isfinite(x) and math.isfinite(se) and se > 0): continue
        rows.append((mach, 'clock' if f[1] in CLOCK and f[2] in CLOCK else 'other', float(f[4]), x, se, n, bl))
def long_bar(bl):
    m = [(bl[i]+bl[i+1])/2 for i in range(0, len(bl)-1, 2)]
    if len(m) < 2: return float('inf')
    mu = st.mean(m); return math.sqrt(sum((v-mu)**2 for v in m)/(len(m)-1)/len(m))
print(f'single passes at the Bonferroni target; false "changed" at z={Z} with t (promise {200*(1-P):.2f}%), and how much the fix widens the bar')
for sig in sorted({r[2] for r in rows}):
    for mach in ('quiet', 'noisy'):
        for grp in ('clock', 'other'):
            sel = [r for r in rows if r[0] == mach and r[1] == grp and r[2] == sig]
            if not sel: continue
            def rate(widen):
                fp = 0; w = []
                for r in sel:
                    s = widen(r); w.append(s/r[4])
                    fp += abs(r[3]) > tq(blocks(r[5])-1)*s
                return 100*fp/len(sel), st.median(w)
            out = [('none', lambda r: r[4]), ('(a) max(short, long)', lambda r: max(r[4], long_bar(r[6])))]
            out += [(f'(b) floor {f}%', (lambda f: lambda r: max(r[4], f/100))(f)) for f in (0.2, 0.3, 0.5)]
            print(f'  goal {100*(math.exp(Z*math.log1p(sig))-1):.0f}%  {mach} {grp:5} n={len(sel):5}: ' + '  |  '.join(f'{nm}: {a:5.2f}% x{b:.2f}' for nm, (a, b) in [(nm, rate(fn)) for nm, fn in out]))
