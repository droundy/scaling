# Replay the multi-pass protocols on dumped trials. x = ln(est/truth), se = bar on ln.
import glob, math, sys, collections, statistics as st
CLOCK = {'cpu_canary','f64_sin','str_find','urandom_read'}
def ok(p): return math.isfinite(p[0]) and math.isfinite(p[1]) and p[1] > 0
def ivw(ps):
    w = [1/p[1]**2 for p in ps]; x = sum(wi*p[0] for wi, p in zip(w, ps))/sum(w)
    return x, math.sqrt(1/sum(w))
def disagree(a, b): return abs(a[0]-b[0]) > 2*math.sqrt(a[1]**2 + b[1]**2)
def dl(ps):
    # DerSimonian-Laird random effects
    k = len(ps); w = [1/p[1]**2 for p in ps]; x, _ = ivw(ps)
    Q = sum(wi*(p[0]-x)**2 for wi, p in zip(w, ps))
    c = sum(w) - sum(wi*wi for wi in w)/sum(w)
    tau2 = max(0.0, (Q - (k-1))/c) if c > 0 else 0.0
    ws = [1/(p[1]**2 + tau2) for p in ps]
    return sum(wi*p[0] for wi, p in zip(ws, ps))/sum(ws), math.sqrt(1/sum(ws))
def protocols(row, g):
    one = row['one']; a, b, c = row['p']
    out = {}
    out['one pass'] = (one[0], one[1], one[2], False)
    if not (ok(a) and ok(b)):
        return out
    x, s = ivw([a, b]); n2 = a[2] + b[2]
    out['two, always report'] = (x, s, n2, False)
    dis = disagree(a, b)
    ps = [a, b] + ([c] if dis and ok(c) else [])
    n3 = n2 + (c[2] if dis else 0)
    # as written in ALGORITHM.md: bar includes between-pass spread; refuse above goal
    x, s = ivw(ps); xs = [p[0] for p in ps]
    sb = st.stdev(xs)/math.sqrt(len(xs))
    bar = max(s, sb)
    out['doc rule'] = (x, bar, n3, bar > g)
    # agree -> report on internal bar; disagree -> third, random effects, refuse if bar > goal
    if not dis:
        out['agree/RE'] = (*ivw([a, b]), n2, False)
    else:
        x, s = dl(ps) if len(ps) == 3 else ivw(ps)
        out['agree/RE'] = (x, s, n3, s > g)
    # agree -> report; disagree -> third; report the agreeing two if any, else refuse
    if not dis:
        out['majority'] = (*ivw([a, b]), n2, False)
    elif len(ps) == 3:
        pairs = [(a, c), (b, c)]
        good = [pp for pp in pairs if not disagree(*pp)]
        if good:
            best = min(good, key=lambda pp: abs(pp[0][0]-pp[1][0]))
            out['majority'] = (*ivw(list(best)), n3, False)
        else:
            out['majority'] = (*ivw(ps), n3, True)
    else:
        out['majority'] = (*ivw([a, b]), n3, True)
    return out
def main():
  pass
rows = []
for fn in (sorted(glob.glob(sys.argv[1] + '/*.tsv')) if __name__ == '__main__' else []):
    mach = fn.split('/')[-1][:5]
    for l in open(fn):
        f = l.rstrip('\n').split('\t')
        if f[3] != 'paired': continue
        t = float(f[4])
        grp = 'clock' if f[1] in CLOCK and f[2] in CLOCK else 'other'
        one = (float(f[8]), float(f[9]), int(f[6]))
        for fld in f[15:]:
            gap, rest = fld.split(':')
            parts = [tuple(float(v) for v in p.split(',')) for p in rest.split(';')]
            rows.append(dict(mach=mach, grp=grp, t=t, gap=int(gap), one=one, p=parts[1:4]))
names = ['one pass', 'two, always report', 'doc rule', 'agree/RE', 'majority']
for gap in (sorted({r['gap'] for r in rows}) if __name__ == '__main__' else []):
    print(f'\n######## gap {gap} rounds between passes')
    for mach in ['quiet', 'noisy']:
        for grp in ['clock', 'other']:
            print(f'== {mach} {grp}')
            for t in (0.02, 0.01, 0.005):
                g = math.log1p(t); B = math.log1p(4*t)
                sel = [r for r in rows if r['gap'] == gap and r['mach'] == mach and r['grp'] == grp and r['t'] == t]
                base = None
                line = f'  {100*t:.1f}%:'
                res = {}
                for nm in names:
                    rep = blown = cov = ref = 0; rounds = 0; N = 0
                    for r in sel:
                        o = protocols(r, g).get(nm)
                        if o is None: continue
                        N += 1; x, s, n, refused = o; rounds += n
                        if refused: ref += 1; continue
                        if not math.isfinite(x): continue
                        rep += 1; blown += abs(x) > B; cov += abs(x) <= s
                    res[nm] = (N, ref, blown, cov/max(rep, 1), rounds/max(N, 1))
                b0 = res['one pass'][4]
                print(f'  {100*t:.1f}% ' + ' | '.join(f'{nm}: blow {res[nm][2]:3} refuse {100*res[nm][1]/max(res[nm][0],1):4.1f}% cov {100*res[nm][3]:3.0f}% x{res[nm][4]/b0:.2f}' for nm in names) + f'  (n={res["one pass"][0]})')
