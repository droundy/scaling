import os
COLLECT = os.environ.get('LAB_COLLECT', os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'day', 'collect'))
import math, pickle, sys, os, statistics as st
def load(path):
    b = open(path,'rb').read()
    i = b.index(b'DATA\n'); head = b[:i].decode().splitlines(); body = b[i+5:]
    order = []
    for l in head:
        if l.startswith('# rung '):
            f = l.split(); order.append((f[2].split('@')[0], int(f[3])))
    bases = sorted({o[0] for o in order}); W = len(bases)
    rec = []; j = 0; L = len(body)
    while j + 1 < L:
        idx = body[j]; j += 1
        v = 0; sh = 0
        while True:
            c = body[j]; j += 1
            v |= (c & 0x7f) << sh; sh += 7
            if not c & 0x80: break
        rec.append((idx, v))
    R = len(rec)//W
    # per workload: list of (n, ns) per round
    per = {w: [None]*R for w in bases}
    for r in range(R):
        for k in range(W):
            idx, v = rec[r*W+k]; w, n = order[idx]
            per[w][r] = (n, v)
    return per
if __name__ == '__main__':
    for m in ['quiet','noisy']:
        for p in '012':
            src = f'{COLLECT}/pairs-{m}/btree_miss+copy_64mb+f64_sin+str_find+urandom_read.p{p}.bin'
            out = f'diag/{m}{p}.pkl'
            if not os.path.exists(out):
                pickle.dump(load(src), open(out,'wb'))
            print(out)
