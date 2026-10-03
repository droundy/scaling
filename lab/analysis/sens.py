# Clock sensitivity: slope of each workload's per-round log time against the canary's, within rounds.
import pickle, math, statistics as st, glob, os, itertools
from levels import levels
def slope(x, y):
    mx, my = st.mean(x), st.mean(y)
    sxx = sum((a-mx)**2 for a in x); return sum((a-mx)*(b-my) for a, b in zip(x, y))/sxx
def smooth(v, w=50):
    # block means over w rounds: the clock moves slowly, per-round noise averages out
    return [st.mean(v[i:i+w]) for i in range(0, len(v)-w+1, w)]
srcs = [f'diag/{m}{p}.pkl' for m in ('quiet', 'noisy') for p in '012'] + sorted(glob.glob('diag/battery-noisy-run*.pkl'))
for f in srcs:
    L = levels(pickle.load(open(f, 'rb')))
    c = smooth(L['cpu_canary'])
    clock_sd = st.pstdev(c)
    betas = {w: slope(c, smooth(L[w])) for w in L if w != 'cpu_canary'}
    print(f'{os.path.basename(f):24} clock sd {100*clock_sd:5.2f}%  beta: ' + '  '.join(f'{w} {b:+.2f}' for w, b in sorted(betas.items())))
