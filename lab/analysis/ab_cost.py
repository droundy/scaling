# Cost to the Bonferroni target and honesty, in parallel over (candidate, composition, quantity).
import sys, os, statistics as st
from multiprocessing import Pool
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import ab_score
from ab_score import recording, trials, summarize, CANDS

ab_score.TRIALS = int(os.environ.get('TRIALS', 100))
NAMES = ['fast pair', 'slow pair', 'f64_sin/canary', 'parse_u64/canary', 'btree_miss/canary', 'warm_dst/canary']
COMPS = [0, 1, 5]


def job(args):
    cand, c, name = args
    ts = []
    for p in (0, 1):
        rec = recording(cand, c, p)
        if rec is not None:
            ts += trials(*rec, name)
    return args, summarize(ts)


if __name__ == '__main__':
    for k in range(7, 20):      # warm the t quantiles once, before forking
        ab_score.tq(k)
    jobs = [(cand, c, name) for c in COMPS for name in NAMES for cand in CANDS
            if not (name == 'slow pair' and c == 0)]
    with Pool(int(os.environ.get('JOBS', 12))) as pool:
        res = dict(pool.map(job, jobs, chunksize=1))
    print('COST to the Bonferroni target (1% goal, 10 comparisons, t at the blocks): median set wall time per'
          '\nstopped trial, preparation included; median rounds; for exact answers, stopped trials whose error'
          '\nexceeds their own threshold (promise: 0.5%). Capped trials (ran off the recording) are excluded.')
    for c in COMPS:
        print(f'\n  composition c{c}')
        for name in NAMES:
            if name == 'slow pair' and c == 0:
                continue
            row = f'  {name:18}'
            for cand in CANDS:
                s = res.get((cand, c, name))
                if s is None:
                    row += f'{cand:>5}: {"(none stopped)":>27}'
                    continue
                h = f' {s["over"]}/{s["nerr"]}' if s['nerr'] else ''
                cap = f' +{s["capped"]}cap' if s['capped'] else ''
                row += f'{cand:>5}: {s["cost_ms"]:9.1f} ms {s["rounds"]:6.0f} r{h}{cap}'.ljust(34)
            print(row)
