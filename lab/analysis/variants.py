import glob, math, statistics as st, collections
CLOCK = {'cpu_canary','f64_sin','str_find','urandom_read'}
def load(v, mach):
    rows = []
    for fn in sorted(glob.glob(f'var/{v}-{mach}?.tsv')):
        for l in open(fn):
            f = l.rstrip('\n').split('\t')
            if f[3] != 'paired': continue
            t = float(f[4]); x = float(f[8]); se = float(f[9])
            rows.append(dict(pair=(f[1], f[2]), t=t, n=int(f[6]), capped=f[7]=='1', x=x, se=se,
                             g=math.log1p(t), B=math.log1p(4*t)))
    return rows
def stats(rs):
    ok = [r for r in rs if math.isfinite(r['x'])]
    N = len(rs)
    blow = sum(abs(r['x']) > r['B'] for r in ok)
    within = sum(abs(r['x']) <= r['g'] for r in ok) / N
    cover = sum(abs(r['x']) <= r['se'] for r in ok) / N
    capped = sum(r['capped'] for r in rs) / N
    return blow, N, within, cover, capped
print('paired estimator; blowups = off by more than 4x the goal.  rounds = total rounds spent, relative to B4')
for grp in ['clock/clock', 'other']:
    for mach in ['quiet', 'noisy']:
        print(f'\n== {mach}, {grp} pairs')
        base = None
        for v in ['B4', 'B4T', 'B6', 'B8', 'B8T', 'B12']:
            rs = [r for r in load(v, mach) if (r['pair'][0] in CLOCK and r['pair'][1] in CLOCK) == (grp == 'clock/clock')]
            line = f'  {v:4}'
            for t in [0.02, 0.01, 0.005]:
                s = [r for r in rs if r['t'] == t]
                b, N, w, c, cap = stats(s)
                # cost: median rounds per trial
                med = st.median(r['n'] for r in s)
                line += f'  | {100*t:.1f}%: blow {b:3}/{N:4} ({100*b/N:4.2f}%) within {100*w:3.0f}% cover {100*c:3.0f}% med {med:5}'
            b, N, *_ = stats(rs)
            line += f'  | all {100*b/N:.2f}%'
            print(line)
