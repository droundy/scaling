# Verdicts under a Bonferroni-corrected significance test, after the whole multi-pass pipeline.
#
# Reads three-pass dumps (`lab pairs` with LAB_RERUN_GAPS and LAB_TARGETS set to
# goal / z). Every trial's x is ln(est / truth), so the true effect is zero; adding
# delta to every pass gives a pair whose true ratio differs by that factor.
#
# Verdicts: changed, unchanged, refused. With delta = 0, "changed" is a false
# positive, and Bonferroni promises it at no more than 2 * (1 - Phi(z)).
import glob, math, sys, statistics as st
CLOCK = {'cpu_canary', 'f64_sin', 'str_find', 'urandom_read'}
Z = float(sys.argv[2]) if len(sys.argv) > 2 else 2.81
def pdf(x, k): return math.exp(math.lgamma((k+1)/2) - math.lgamma(k/2))/math.sqrt(k*math.pi)*(1+x*x/k)**(-(k+1)/2)
def cdf(x, k, N=3000):
    h = x/N; return 0.5 + sum(pdf((i+0.5)*h, k) for i in range(N))*h
P = 0.5*(1+math.erf(Z/math.sqrt(2)))
_tq = {}
def tq(k):
    k = max(1, min(k, 200))
    if k not in _tq:
        lo, hi = 0.0, 100.0
        for _ in range(60):
            m = (lo+hi)/2
            if cdf(m, k) < P: lo = m
            else: hi = m
        _tq[k] = m
    return _tq[k]
def blocks(n): return min(20, max(8, n//15))
def ok(p): return math.isfinite(p[0]) and math.isfinite(p[1]) and p[1] > 0
def ivw(ps):
    w = [1/p[1]**2 for p in ps]; return sum(wi*p[0] for wi, p in zip(w, ps))/sum(w), math.sqrt(1/sum(w))
def dl(ps):
    k = len(ps); w = [1/p[1]**2 for p in ps]; x, _ = ivw(ps)
    Q = sum(wi*(p[0]-x)**2 for wi, p in zip(w, ps)); c = sum(w) - sum(wi*wi for wi in w)/sum(w)
    tau2 = max(0.0, (Q-(k-1))/c) if c > 0 else 0.0
    ws = [1/(p[1]**2+tau2) for p in ps]
    return sum(wi*p[0] for wi, p in zip(ws, ps))/sum(ws), math.sqrt(1/sum(ws))
def changed1(p): return abs(p[0]) > tq(blocks(p[2])-1)*p[1]
def verdicts(a, b, c, g):
    """(RE rule, IUT rule) verdicts for passes a, b, c of (x, se, n)."""
    if abs(a[0]-b[0]) <= 2*math.sqrt(a[1]**2 + b[1]**2):
        x, s = ivw([a, b]); dof = blocks(a[2]) + blocks(b[2]) - 2
        v = 'changed' if abs(x) > tq(dof)*s else 'unchanged'
        return v, v, False
    ps = [a, b, c]
    x, s = dl(ps)
    if abs(x) > tq(2)*s: re = 'changed'
    elif tq(2)*s <= g: re = 'unchanged'
    else: re = 'refused'
    ch = [changed1(p) for p in ps]
    if all(ch) and len({p[0] > 0 for p in ps}) == 1: iut = 'changed'
    elif not any(ch) and all(abs(p[0]) <= g for p in ps): iut = 'unchanged'
    else: iut = 'refused'
    return re, iut, True
def main():
  rows = []
  for fn in sorted(glob.glob(sys.argv[1] + '/*.tsv')):
      mach = fn.split('/')[-1][:5]
      for l in open(fn):
          f = l.rstrip('\n').split('\t')
          if f[3] != 'paired' or len(f) < 16: continue
          grp = 'clock' if f[1] in CLOCK and f[2] in CLOCK else 'other'
          one = (float(f[8]), float(f[9]), int(f[6]))
          for fld in f[15:]:
              gap, rest = fld.split(':')
              parts = [tuple(float(v) for v in p.split(',')) for p in rest.split(';')]
              if len(parts) < 4: continue
              rows.append((mach, grp, float(f[4]), int(gap), one, [(p[0], p[1], int(p[2])) for p in parts[1:4]]))
  print(f'z = {Z} (promised false-positive rate {200*(1-P):.2f}%); t at each verdict\'s degrees of freedom; 14.1 = t at 2 dof')
  for gap in sorted({r[3] for r in rows}):
      print(f'\n######## passes {gap} rounds apart')
      for mach in ('quiet', 'noisy'):
          for grp in ('clock', 'other'):
              for sig in sorted({r[2] for r in rows}):
                  sel = [r for r in rows if r[0] == mach and r[1] == grp and r[2] == sig and r[3] == gap]
                  if not sel: continue
                  g = Z*math.log1p(sig)   # the goal this one-sigma target serves
                  line = f'  {mach} {grp:5} goal {100*(math.exp(g)-1):.1f}% (n={len(sel)}):'
                  for dname, d in (('no change', 0.0), ('change = goal', g), ('change = 2 goals', 2*g)):
                      one = [r for r in sel if ok(r[4])]
                      o_ch = sum(changed1((r[4][0]+d, r[4][1], r[4][2])) for r in one)/len(one)
                      res = {'RE': [0, 0, 0], 'IUT': [0, 0, 0]}; dis = 0; N = 0
                      for r in sel:
                          a, b, c = [(p[0]+d, p[1], p[2]) for p in r[5]]
                          if not (ok(a) and ok(b) and ok(c)): continue
                          N += 1
                          re, iut, dd = verdicts(a, b, c, g); dis += dd
                          for k, v in (('RE', re), ('IUT', iut)):
                              res[k][('changed', 'unchanged', 'refused').index(v)] += 1
                      line += f'\n      {dname:16} one pass: changed {100*o_ch:5.1f}%   third pass needed {100*dis/N:4.1f}%   ' + '   '.join(
                          f'{k}: changed {100*v[0]/N:5.1f}% unchanged {100*v[1]/N:5.1f}% refused {100*v[2]/N:5.1f}%' for k, v in res.items())
                  print(line)

if __name__ == '__main__':
    main()
