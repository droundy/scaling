# Mechanism-agnostic detection: does a function's batch time depend on what ran just before it?
# The shuffle gives every batch a random predecessor, so this costs nothing extra to measure.
import glob, math, statistics as st, collections, sys, os
R = os.environ.get('AB', os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'day', 'collect', 'ab'))
def load_seq(path):
    b = open(path, 'rb').read(); i = b.index(b'DATA\n')
    order = [(l.split()[2].split('@')[0], int(l.split()[3])) for l in b[:i].decode().splitlines() if l.startswith('# rung ')]
    body = b[i+5:]; j = 0; out = []
    while j + 1 < len(body):
        idx = body[j]; j += 1; v = 0; sh = 0
        while True:
            c = body[j]; j += 1; v |= (c & 0x7f) << sh; sh += 7
            if not c & 0x80: break
        out.append((order[idx][0], order[idx][1], v))
    return out
def tm(v):
    v = sorted(v); k = len(v)//4; v = v[k:len(v)-k] or v; return sum(v)/len(v)
def main():
    for cand, comp in [(c, k) for c in sys.argv[1].split(',') for k in sys.argv[2].split(',')]:
        seq = load_seq(glob.glob(f'{R}/{cand}/c{comp}/p0/*.bin')[0])
        by = collections.defaultdict(lambda: collections.defaultdict(list))
        for prev, cur in zip(seq, seq[1:]):
            by[cur[0]][(cur[1], prev[0])].append(cur[2] / cur[1])
        print(f'== {cand} c{comp}: per-iteration cost by predecessor, as % from the function\'s median over predecessors (top rung, trimmed)')
        for w in sorted(by):
            top = max(n for n, _ in by[w])
            cells = {p: tm(v) for (n, p), v in by[w].items() if n == top and len(v) >= 20}
            if len(cells) < 2: continue
            med = st.median(cells.values())
            spread = 100*(max(cells.values()) - min(cells.values()))/med
            print(f'  {w:12} spread {spread:6.2f}%   ' + '  '.join(f'{p}:{100*(v/med-1):+.1f}' for p, v in sorted(cells.items(), key=lambda kv: kv[1])))

if __name__ == '__main__':
    main()
