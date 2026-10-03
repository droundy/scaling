import os
import sys, glob, pickle, math
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import paired
for cond in sys.argv[1:]:
    R = [pickle.load(open(f, 'rb')) for f in sorted(glob.glob(f'diag/{cond}-run*.pkl'))]
    for a, b in [('f64_sin', 'cpu_canary'), ('str_find', 'cpu_canary'), ('urandom_read', 'cpu_canary')]:
        allr = [paired.ratio(paired.pair_rounds(r, a, b)) for r in R]
        ref = sum(math.log(x) for x in allr)/len(allr)
        line = f'{cond:14} {a:12}/canary, 8 windows per process, % from the mean of processes:\n   '
        for r in R:
            rs = paired.pair_rounds(r, a, b); k = len(rs)//8
            line += ' '.join(f'{100*(math.log(paired.ratio(rs[i*k:(i+1)*k]))-ref):+5.1f}' for i in range(8)) + '  |  '
        print(line)
