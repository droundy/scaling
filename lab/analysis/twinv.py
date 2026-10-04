# The A-against-A' null: verdicts for each twin pair after the whole pipeline, judged against the
# long-run ratio (does wander cancel?) and against exactly 1 (what a user comparing identical code sees).
import glob, math, sys, re, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from verdicts import verdicts, changed1, ok, P, Z
d = sys.argv[1]
truths = {}
for out in glob.glob(d + '/*.out'):
    for l in open(out):
        m = re.match(r'===== (\S+) / (\S+) =====  truth (\S+)', l)
        if m: truths[(m.group(1), m.group(2))] = float(m.group(3))
print(f'z = {Z}, promised false "changed" {200*(1-P):.2f}%')
for fn in sorted(glob.glob(d + '/*.tsv')):
    rows = []
    for l in open(fn):
        f = l.rstrip('\n').split('\t')
        if f[3] != 'paired' or len(f) < 16 or not f[2].endswith('_b') or f[1] + '_b' != f[2]: continue
        one = (float(f[8]), float(f[9]), int(f[6]))
        for fld in f[15:]:
            gap, rest = fld.split(':')
            parts = [tuple(float(v) for v in p.split(',')) for p in rest.split(';')]
            rows.append((float(f[4]), int(gap), one, [(p[0], p[1], int(p[2])) for p in parts[1:4]], (f[1], f[2])))
    if not rows: continue
    pair = rows[0][4]; lt = math.log(truths[pair])
    print(f'\n== {pair[0]} / {pair[1]}: long-run ratio {truths[pair]:.5f} ({100*(truths[pair]-1):+.3f}%)')
    for sig in sorted({r[0] for r in rows}):
        g = Z*math.log1p(sig)
        for gap in sorted({r[1] for r in rows}):
            sel = [r for r in rows if r[0] == sig and r[1] == gap]
            for ref, shift in (('long-run ratio', 0.0), ('exactly 1', lt)):
                one = [r for r in sel if ok(r[2])]
                o_ch = sum(changed1((r[2][0]+shift, r[2][1], r[2][2])) for r in one)/len(one)
                cnt = {'changed': 0, 'unchanged': 0, 'refused': 0}; N = 0
                for r in sel:
                    a, b, c = [(p[0]+shift, p[1], p[2]) for p in r[3]]
                    if not (ok(a) and ok(b) and ok(c)): continue
                    N += 1; _, iut, _ = verdicts(a, b, c, g); cnt[iut] += 1
                print(f'  goal {100*(math.exp(g)-1):.0f}%, passes {gap:7} rounds apart, against {ref:14}: one pass changed {100*o_ch:5.2f}%   pipeline (IUT): '
                      + '  '.join(f'{k} {100*v/N:5.2f}%' for k, v in cnt.items()) + f'   (n={N})')
