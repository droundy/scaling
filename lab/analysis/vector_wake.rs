// Long chain, then short chain, timed with nothing else in between: no allocation, no I/O, no rand.
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
extern "C" { #[link_name = "getppid"] fn libc_getppid() -> i32; }
fn main() {
    let args: Vec<u64> = std::env::args().skip(1).map(|s| s.parse().unwrap()).collect();
    let (long, short, reps, avx) = (args[0], args[1], args[2], args.get(3).copied().unwrap_or(0));
    let mut x = 0x243F6A8885A308D3u64;
    let mut sink = std::io::BufWriter::new(std::fs::File::create("/dev/null").unwrap());
    let mut devnull = std::fs::File::create("/dev/null").unwrap();
    let mut per = Vec::with_capacity(reps as usize);
    for _ in 0..reps {
        chain(long, &mut x);
        if avx == 2 { black_box(std::process::id()); unsafe { libc_getppid(); } }
        if avx == 3 { let v: Vec<u8> = Vec::with_capacity(black_box(4096)); black_box(&v); }
        if avx == 4 { use std::io::Write; let _ = sink.write_all(b"0123456789"); }
        if avx == 5 { let v = vec![0u8; black_box(1 << 20)]; black_box(&v); }
        if avx == 7 { let v: Vec<u8> = Vec::with_capacity(black_box(1 << 20)); black_box(&v); }
        if avx == 8 { let v = vec![0u8; black_box(64 << 10)]; black_box(&v); }
        if avx == 9 { let mut v = vec![1u8; black_box(64 << 10)]; v[100] = 2; black_box(&v); }
        if avx == 10 { black_box(black_box(1.5f64) * black_box(2.5f64)); }
        if avx == 11 { unsafe { std::arch::asm!("subpd xmm0, xmm0", out("xmm0") _); } }
        if avx == 12 { black_box(black_box(std::time::Duration::from_nanos(123456789)).as_secs_f64()); }
        if avx == 13 { unsafe { std::arch::asm!("paddq xmm0, xmm0", out("xmm0") _); } }
        if avx == 6 { use std::io::Write; let _ = devnull.write_all(b"x"); }
        if avx == 1 {
            // one 256-bit integer add, outside the timed region
            unsafe { std::arch::asm!("vpaddq ymm0, ymm0, ymm0", "vzeroupper", out("ymm0") _); }
        }
        let t = Instant::now();
        chain(short, &mut x);
        per.push(t.elapsed().as_nanos() as f64 / short as f64);
    }
    per.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("long {long} short {short} avx {avx}: short chain {:.4} ns/link (median), p10 {:.4} p90 {:.4}", per[per.len()/2], per[per.len()/10], per[9*per.len()/10]);
}
