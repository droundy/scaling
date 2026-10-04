# Mechanism-agnostic detection, second form: does a function's batch time depend on how much
# else ran since its own previous batch? Measured as the summed batch time of everything between.
import glob, statistics as st, collections, sys, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from predecessor import load_seq, tm, R
for cand, comp in [(c, k) for c in sys.argv[1].split(',') for k in sys.argv[2].split(',')]:
    seq = load_seq(glob.glob(f'{R}/{cand}/c{comp}/p0/*.bin')[0])
    last = {}; acc = 0.0; rows = collections.defaultdict(list)
    for w, n, t in seq:
        if w in last:
            rows[w].append((acc - last[w], n, t / n))
        acc += t
        last[w] = acc
    print(f'== {cand} c{comp}: per-iteration cost (top rung) by time since the function\'s own previous batch')
    for w in sorted(rows):
        top = max(n for _, n, _ in rows[w])
        r = sorted((g, c) for g, n, c in rows[w] if n == top)
        if len(r) < 200: continue
        k = len(r) // 4
        qs = [r[i*k:(i+1)*k] for i in range(4)]
        base = tm([c for _, c in qs[0]])
        print(f'  {w:12} ' + '  '.join(f'gap {st.median([g for g, _ in q])/1e3:8.1f}us: {100*(tm([c for _, c in q])/base-1):+6.2f}%' for q in qs))
