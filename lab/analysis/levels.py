import pickle, math, statistics as st, collections
def levels(per):
    out = {}
    for w, rs in per.items():
        med = {}
        by = collections.defaultdict(list)
        for n, v in rs: by[n].append(v)
        for n, vs in by.items(): med[n] = math.log(st.median(vs))
        out[w] = [math.log(max(v,1)) - med[n] for n, v in rs]
    return out
def trimmed(v, t=0.25):
    v = sorted(v); k = int(len(v)*t); v = v[k:len(v)-k] or v
    return sum(v)/len(v)
def varcurve(d, ws=(1,4,15,60,240,1000,4000)):
    # variance of block means * w, relative to w=1: 1 means independent rounds
    res = []
    for w in ws:
        m = [sum(d[i:i+w])/w for i in range(0, len(d)-w+1, w)]
        mu = sum(m)/len(m)
        res.append(sum((x-mu)**2 for x in m)/(len(m)-1)*w)
    return [r/res[0] for r in res]
if __name__ == '__main__':
    import sys
    for m in ['quiet','noisy']:
        for p in '0':
            L = levels(pickle.load(open(f'diag/{m}{p}.pkl','rb')))
            ws = sorted(L)
            print(f'== {m}{p}: {len(L[ws[0]])} rounds.  var(mean of w rounds)*w / var(one round), for w = 1 4 15 60 240 1000 4000')
            # clip each round's level at +-0.5 to keep single huge outliers from dominating
            c = {w: [max(-0.5, min(0.5, x)) for x in L[w]] for w in ws}
            for w in ws:
                print(f'  {w:>14}      ', ' '.join(f'{x:6.2f}' for x in varcurve(c[w])))
            for i, a in enumerate(ws):
                for b in ws[i+1:]:
                    d = [x-y for x, y in zip(c[a], c[b])]
                    print(f'  {a:>14}-{b:<14}', ' '.join(f'{x:6.2f}' for x in varcurve(d)))
