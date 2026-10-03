# How often |error| > z * sigma, with and without Student's t at the block count's degrees of freedom.
import glob, math, sys
CLOCK = {'cpu_canary','f64_sin','str_find','urandom_read'}
def pdf(x, k): return math.exp(math.lgamma((k+1)/2) - math.lgamma(k/2))/math.sqrt(k*math.pi)*(1+x*x/k)**(-(k+1)/2)
def cdf(x, k, N=4000):
    h = x/N; return 0.5 + sum(pdf((i+0.5)*h, k) for i in range(N))*h
def tq(p, k):
    lo, hi = 0.0, 60.0
    for _ in range(60):
        m = (lo+hi)/2
        if cdf(m, k) < p: lo = m
        else: hi = m
    return m
ZS = [1.0, 2.0, 2.5, 2.81, 3.29]
def ncdf(z): return 0.5*(1+math.erf(z/math.sqrt(2)))
P = {z: ncdf(z) for z in ZS}
TQ = {(z, k): tq(P[z], k) for z in ZS for k in range(3, 20)}
rows = []
for fn in sorted(glob.glob((sys.argv[1] if len(sys.argv) > 1 else 'three') + '/*.tsv')):
    mach = fn.split('/')[-1][:5]
    for l in open(fn):
        f = l.rstrip('\n').split('\t')
        if f[3] != 'paired': continue
        x, se, n = float(f[8]), float(f[9]), int(f[6])
        if not (math.isfinite(x) and math.isfinite(se) and se > 0): continue
        b = min(20, max(8, n//15))
        rows.append((mach, 'clock' if f[1] in CLOCK and f[2] in CLOCK else 'other', float(f[4]), x, se, b))
print('share of stopped trials with |error| > z*sigma; normal expectation: ' + '  '.join(f'z={z}: {100*2*(1-P[z]):.2f}%' for z in ZS))
for mach in ('quiet', 'noisy'):
    for grp in ('clock', 'other'):
        for t in sorted({r[2] for r in rows}, reverse=True):
            sel = [r for r in rows if r[0] == mach and r[1] == grp and r[2] == t]
            if not sel: continue
            plain = [100*sum(abs(r[3]) > z*r[4] for r in sel)/len(sel) for z in ZS]
            witht = [100*sum(abs(r[3]) > TQ[(z, r[5]-1)]*r[4] for r in sel)/len(sel) for z in ZS]
            print(f'  {mach} {grp:5} goal {100*t:.1f}% (n={len(sel):5}):  plain ' + ' '.join(f'{v:5.2f}' for v in plain) + '   with t ' + ' '.join(f'{v:5.2f}' for v in witht))
