//! Times Horizon's 2D simplex noise on the samples of the fork's Java
//! benchmark (fork `afterwinter`, vanilla feasibility): n x n samples at
//! (x * 0.137, z * 0.091), seed 42, five passes.
//!
//!     simplex_bench <n>
use horizon_core::{noise::SimplexNoise, random::LegacyRandom};
use std::time::Instant;

fn main() {
    let n: i32 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(2000);
    let noise = SimplexNoise::new(&mut LegacyRandom::new(42));
    for pass in 0..5 {
        let t = Instant::now();
        let mut sum = 0.0f64;
        for x in 0..n {
            for z in 0..n {
                sum += f64::from(noise.get2(f64::from(x) * 0.137, f64::from(z) * 0.091));
            }
        }
        println!(
            "pass {pass}: {:.1} ms sum={sum:.6}",
            t.elapsed().as_secs_f64() * 1e3
        );
    }
}
