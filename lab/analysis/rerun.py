import glob, math, statistics as st, collections
CLOCK = {'cpu_canary','f64_sin','str_find','urandom_read'}
rows = []
for fn in sorted(glob.glob('rerun/*.tsv')):
    mach = fn.split('/')[-1][:5]
    for l in open(fn):
        f = l.rstrip('\n').split('\t')
        if f[3] != 'paired': continue
        t = float(f[4])
        r = dict(mach=mach, grp='clock' if f[1] in CLOCK and f[2] in CLOCK else 'other', t=t, n=int(f[6]),
                 x=float(f[8]), se=float(f[9]), g=math.log1p(t), B=math.log1p(4*t), re={})
        for fld in f[15:]:
            gap, rest = fld.split(':')
            parts = [tuple(float(v) for v in p.split(',')) for p in rest.split(';')]
            r['re'][int(gap)] = parts   # again, first-half, second-half: (x, se, n, capped)
        rows.append(r)
blown = lambda x, r: math.isfinite(x) and abs(x) > r['B']

print('== Where do blowups stop?  (8 blocks, the default)  share of trials by rounds at stop')
for mach in ['quiet','noisy']:
    for grp in ['clock','other']:
        rs = [r for r in rows if r['mach']==mach and r['grp']==grp]
        bl = [r for r in rs if blown(r['x'], r)]
        def dist(v):
            c = collections.Counter('120 (floor)' if r['n']==120 else '157-453' if r['n']<=453 else '589-1875' if r['n']<=1875 else '2438+' for r in v)
            return '  '.join(f'{k} {100*c[k]/len(v):3.0f}%' for k in ['120 (floor)','157-453','589-1875','2438+'])
        print(f'  {mach} {grp:5}: blowups {len(bl):4}/{len(rs)}   blowups: {dist(bl)}')
        print(f'  {"":17}             all trials: {dist(rs)}')

print('\n== Rerun: does a second run catch the blowups?  disagree = |x1-x2| > 2*sqrt(se1^2+se2^2)')
for mach in ['quiet','noisy']:
    for grp in ['clock','other']:
        rs = [r for r in rows if r['mach']==mach and r['grp']==grp]
        for gap in (0, 4700, 47000):
            bl = [r for r in rs if blown(r['x'], r)]
            ok = [r for r in rs if math.isfinite(r['x']) and not blown(r['x'], r)]
            def dis(r):
                x2, s2, *_ = r['re'][gap][0]
                return not math.isfinite(x2) or abs(r['x']-x2) > 2*math.sqrt(r['se']**2+s2**2)
            # combined: inverse-variance mean, if both runs finite
            def comb(r):
                x2, s2, *_ = r['re'][gap][0]
                if not (math.isfinite(x2) and s2 > 0 and r['se'] > 0): return r['x']
                w1, w2 = 1/r['se']**2, 1/s2**2
                return (w1*r['x']+w2*x2)/(w1+w2)
            cb = sum(blown(comb(r), r) for r in rs)
            # split budget: two runs at goal*sqrt2, averaged
            def split(r):
                (xa, sa, na, _), (xb, sb, nb, _) = r['re'][gap][1], r['re'][gap][2]
                return (xa+xb)/2, na+nb
            sp = [split(r) for r in rs]
            sb = sum(blown(x, r) for (x, _), r in zip(sp, rs))
            cost = st.mean(n for _, n in sp) / st.mean(r['n'] for r in rs)
            print(f'  {mach} {grp:5} gap {gap:5}: one run blows {len(bl):4}; rerun disagrees on {100*sum(map(dis,bl))/max(1,len(bl)):3.0f}% of those, {100*sum(map(dis,ok))/len(ok):4.1f}% of the rest;  both averaged blow {cb:4};  split budget blows {sb:4} at x{cost:.2f} the rounds')
