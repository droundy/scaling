import pickle, math, statistics as st, glob, os, itertools
from levels import levels
def tm(v):
    v = sorted(v); k = len(v)//4; v = v[k:len(v)-k]; return sum(v)/len(v)
def slope(x, y):
    mx, my = st.mean(x), st.mean(y); return sum((a-mx)*(b-my) for a, b in zip(x, y))/sum((a-mx)**2 for a in x)
W = 1000
srcs = [f'diag/noisy{p}.pkl' for p in '012'] + sorted(glob.glob('diag/battery-noisy-run*.pkl')) + sorted(glob.glob('diag/mains-noisy-run*.pkl'))
pts = []
for f in srcs:
    L = levels(pickle.load(open(f, 'rb')))
    win = lambda v: [tm(v[i:i+W]) for i in range(0, len(v)-W+1, W)]
    c = win(L['cpu_canary']); csd = st.pstdev(c)
    beta = {w: slope(c, win(L[w])) for w in L}
    for a, b in itertools.combinations(sorted(L), 2):
        d = win([x-y for x, y in zip(L[a], L[b])]); m = st.mean(d)
        obs = math.sqrt(st.mean([(x-m)**2 for x in d]))
        pred = abs(beta[a]-beta[b])*csd
        pts.append((os.path.basename(f), a, b, pred, obs))
print('predicted wander = |beta_A - beta_B| x sd of the clock, over 1000-round windows; observed = rms of the pair ratio over the same windows')
for f in sorted({p[0] for p in pts}):
    sel = [p for p in pts if p[0] == f]
    print(f'  {f:24} ' + '  '.join(f'{p[1][:5]}/{p[2][:5]} {100*p[3]:.1f}/{100*p[4]:.1f}' for p in sel))
x = [math.log(max(p[3],1e-4)) for p in pts]; y = [math.log(p[4]) for p in pts]
mx, my = st.mean(x), st.mean(y)
r = sum((a-mx)*(b-my) for a, b in zip(x, y))/math.sqrt(sum((a-mx)**2 for a in x)*sum((b-my)**2 for b in y))
print(f'  correlation of log predicted with log observed: {r:.2f}  (n={len(pts)})')
big = [p for p in pts if p[3] > 0.01]
print(f'  pairs predicted to wander >1%: {len(big)}; observed >1%: {sum(p[4] > 0.01 for p in big)}')
small = [p for p in pts if p[3] <= 0.005]
print(f'  pairs predicted <=0.5%: {len(small)}; observed: median {100*st.median(p[4] for p in small):.2f}%, max {100*max(p[4] for p in small):.2f}%  ({max(small, key=lambda p: p[4])[:3]})')
