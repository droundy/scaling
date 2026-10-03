import pickle, math, statistics as st
from levels import levels
CLOCK = {'cpu_canary','f64_sin','str_find','urandom_read'}
for m in ['quiet','noisy']:
    for p in '012':
        L = levels(pickle.load(open(f'diag/{m}{p}.pkl','rb')))
        ws = sorted(L)
        pairs = [('btree_miss','copy_64mb'),('btree_miss','f64_sin'),('copy_64mb','f64_sin'),('cpu_canary','f64_sin'),('str_find','urandom_read')]
        print(f'== {m}{p}: spread of window means of ln(t_A/t_B), as %, for windows of 250 / 1000 / 4000 rounds (~3 s / 13 s / 50 s); and share of 1000-round windows off by >2% from the whole-run value')
        for a, b in pairs:
            d = [x - y for x, y in zip(L[a], L[b])]
            # trimmed mean of the whole, and trimmed means of windows (as the estimators trim)
            def tm(v):
                v = sorted(v); k = len(v)//4; v = v[k:len(v)-k]; return sum(v)/len(v)
            tot = tm(d)
            out = []
            for w in (250, 1000, 4000):
                mm = [tm(d[i:i+w]) - tot for i in range(0, len(d)-w+1, w)]
                out.append(100*math.sqrt(sum(x*x for x in mm)/len(mm)))
            mm = [tm(d[i:i+1000]) - tot for i in range(0, len(d)-999, 1000)]
            far = sum(abs(x) > math.log(1.02) for x in mm)/len(mm)
            print(f'  {a:>11}/{b:<13} {out[0]:5.2f}% {out[1]:5.2f}% {out[2]:5.2f}%   off>2%: {100*far:4.1f}%')
        if p == '0': continue
