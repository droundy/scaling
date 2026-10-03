# Pure statistics: iid Gaussian rounds, the lab's stopping rule, no machine at all.
import random, math
T1 = [1.8373,1.3213,1.1969,1.1416,1.1105,1.0906,1.0767,1.0665,1.0587,1.0526,1.0476,1.0434,1.04,1.037,1.0345,1.0322,1.0303,1.0286,1.027]
def trial(sig, g, minb, rng, cap=4000):
    n = max(60, 15*minb); xs = []
    while True:
        while len(xs) < n: xs.append(rng.gauss(0, sig))
        b = max(minb, min(20, n//15)) if n//15 >= minb else minb
        per = n//b
        m = [sum(xs[i*per:(i+1)*per])/per for i in range(b)]
        mu = sum(m)/b; se = math.sqrt(sum((v-mu)**2 for v in m)/(b-1)/b)
        if se <= g or n >= cap: return sum(xs[:n])/n, n
        n = int(n*1.3)+1
rng = random.Random(1)
g = 1.0; B = 3.9
print('iid rounds; per-round sd chosen so an honest stop needs about R rounds.  blowup = off by >3.9 goals')
for R in (60, 120, 240, 500):
    sig = g*math.sqrt(R)
    for minb in (4, 8):
        N = 20000; blow = 0; tot = 0
        for _ in range(N):
            x, n = trial(sig, g, minb, rng); tot += n
            blow += abs(x) > B*g
        print(f'  R={R:4} minblocks={minb}:  blowups {100*blow/N:5.2f}%   mean rounds {tot/N:6.0f}')
