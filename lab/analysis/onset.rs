// Per wake-up: when the dip starts (first slow probe) and when it ends (first fast probe after it).
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
    let (long, reps) = (a[0], a[1] as usize);
    let mut x = 1u64;
    // full-speed probe time, measured warm
    let mut best = u64::MAX;
    for _ in 0..2000 { let t = Instant::now(); chain(256, &mut x); best = best.min(t.elapsed().as_nanos() as u64); }
    let (mut onsets, mut ends, mut none) = (Vec::new(), Vec::new(), 0);
    for _ in 0..reps {
        chain(long, &mut x);
        black_box(black_box(1.5f64) * black_box(2.5f64));
        let w = Instant::now();
        let (mut onset, mut end) = (None, None);
        loop {
            let t = Instant::now();
            chain(256, &mut x);
            let dt = t.elapsed().as_nanos() as u64;
            let now = w.elapsed().as_nanos() as f64 / 1e3;
            if onset.is_none() && dt * 100 > best * 110 { onset = Some(now); }
            if onset.is_some() && end.is_none() && dt * 100 < best * 103 { end = Some(now); break; }
            if now > 3000.0 { break; }
        }
        match (onset, end) { (Some(o), Some(e)) => { onsets.push(o); ends.push(e); } _ => none += 1 }
    }
    let q = |v: &mut Vec<f64>, p: f64| { v.sort_by(|a, b| a.partial_cmp(b).unwrap()); if v.is_empty() { f64::NAN } else { v[((v.len() - 1) as f64 * p) as usize] } };
    println!("idle {:6.1} ms: onset us p10 {:6.1} p50 {:6.1} p90 {:6.1} max {:6.1} | end us p10 {:6.1} p50 {:6.1} p90 {:6.1} | no dip seen {none}/{reps}",
        long as f64 * 2.36e-6, q(&mut onsets, 0.1), q(&mut onsets, 0.5), q(&mut onsets, 0.9), q(&mut onsets, 1.0), q(&mut ends, 0.1), q(&mut ends, 0.5), q(&mut ends, 0.9));
}
