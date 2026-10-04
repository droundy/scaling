// The shape of recovery: long integer chain, one f64 op, then back-to-back timed probes.
use std::hint::black_box;
use std::time::Instant;
#[inline(never)]
fn chain(n: u64, x: &mut u64) {
    for _ in 0..n {
        let v = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *x = v;
        black_box(v);
    }
}
fn main() {
    let a: Vec<u64> = std::env::args().skip(1).map(|s| s.parse().unwrap()).collect();
    let (long, probe, nprobe, reps) = (a[0], a[1], a[2] as usize, a[3] as usize);
    let mut x = 1u64;
    let mut prof = vec![Vec::with_capacity(reps); nprobe];
    for _ in 0..reps {
        chain(long, &mut x);
        black_box(black_box(1.5f64) * black_box(2.5f64));
        let mut ts = [0u64; 4096];
        for i in 0..nprobe {
            let t = Instant::now();
            chain(probe, &mut x);
            ts[i] = t.elapsed().as_nanos() as u64;
        }
        for i in 0..nprobe { prof[i].push(ts[i]); }
    }
    let mut cum = 0u64;
    for (i, p) in prof.iter_mut().enumerate() {
        p.sort();
        let med = p[p.len()/2];
        cum += med;
        if i < 8 || i % 8 == 0 { println!("probe {i:4}  t={:7.1}us  median {:6} ns  p10 {:6} p90 {:6}  ({:.3} ns/link)", cum as f64/1e3, med, p[p.len()/10], p[9*p.len()/10], med as f64/probe as f64); }
    }
}
