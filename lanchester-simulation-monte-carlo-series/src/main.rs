// Lanchester's two laws, fought out unit by unit.

// Aimed fire: every living shooter picks a random living enemy each step and
// hits with probability `P`. Melee: only a front of `FRONT` pairs can fight,
// and each engaged pair trades blows. Both armies have identical quality, so
// any difference in the result comes from the geometry of the fight.
//
// One step is one simultaneous volley: both sides fire from the roster they had
// at the start of the step, then everyone killed is removed.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::format_push_string,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::needless_for_each,
    clippy::needless_range_loop
)]

use std::fs;

/// Pseudo-random generator: xoshiro256++ seeded through `SplitMix64`.
#[derive(Debug, Clone)]
struct Rng {
    state: [u64; 4],
}

impl Rng {
    fn new(seed: u64) -> Self {
        let mut sm = seed;
        let mut next = || {
            sm = sm.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = sm;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        Self {
            state: [next(), next(), next(), next()],
        }
    }

    fn next_u64(&mut self) -> u64 {
        let s = &mut self.state;
        let result = s[0].wrapping_add(s[3]).rotate_left(23).wrapping_add(s[0]);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    /// Uniform float in `[0, 1)` with 53 random bits.
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1_u64 << 53) as f64
    }

    /// Uniform index in `0..n`.
    fn below(&mut self, n: usize) -> usize {
        ((self.unit() * n as f64) as usize).min(n - 1)
    }
}

/// Mean and standard deviation of a sample.
fn mean_sd(xs: &[f64]) -> (f64, f64) {
    let n = xs.len() as f64;
    let m = xs.iter().sum::<f64>() / n;
    let var = xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1.0).max(1.0);
    (m, var.sqrt())
}

/// Format a slice of floats as a JSON array.
fn json_arr(xs: &[f64]) -> String {
    let body: Vec<String> = xs.iter().map(|x| format!("{x:.6}")).collect();
    format!("[{}]", body.join(","))
}

/// Chance that one shot or one blow kills in one step.
const P: f64 = 0.05;
/// Width of the melee front, in pairs of fighters.
const FRONT: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Law {
    Square,
    Linear,
}

/// One battle between `a0` and `b0` fighters. Returns survivors and, per fighter, the step of death.
fn fight(law: Law, a0: usize, b0: usize, rng: &mut Rng) -> (usize, usize, Vec<u32>, Vec<u32>) {
    let (mut a_dead, mut b_dead) = (vec![0_u32; a0], vec![0_u32; b0]);
    let (mut a_alive, mut b_alive): (Vec<usize>, Vec<usize>) =
        ((0..a0).collect(), (0..b0).collect());
    let mut step = 0_u32;
    while !a_alive.is_empty() && !b_alive.is_empty() {
        step += 1;
        let (mut a_hit, mut b_hit) = (Vec::new(), Vec::new());
        match law {
            Law::Square => {
                for _ in &a_alive {
                    if rng.unit() < P {
                        b_hit.push(b_alive[rng.below(b_alive.len())]);
                    }
                }
                for _ in &b_alive {
                    if rng.unit() < P {
                        a_hit.push(a_alive[rng.below(a_alive.len())]);
                    }
                }
            }
            Law::Linear => {
                let engaged = a_alive.len().min(b_alive.len()).min(FRONT);
                for i in 0..engaged {
                    if rng.unit() < P {
                        b_hit.push(b_alive[i]);
                    }
                    if rng.unit() < P {
                        a_hit.push(a_alive[i]);
                    }
                }
            }
        }
        for id in b_hit {
            if b_dead[id] == 0 {
                b_dead[id] = step;
            }
        }
        for id in a_hit {
            if a_dead[id] == 0 {
                a_dead[id] = step;
            }
        }
        a_alive.retain(|&i| a_dead[i] == 0);
        b_alive.retain(|&i| b_dead[i] == 0);
    }
    (a_alive.len(), b_alive.len(), a_dead, b_dead)
}

/// Root-mean-square of a sample.
fn rms(xs: &[f64]) -> f64 {
    (xs.iter().map(|x| x * x).sum::<f64>() / xs.len() as f64).sqrt()
}

/// Mean and RMS of the signed margin (A survivors minus B survivors) over `runs` battles.
///
/// The square law is a statement about the mean square of the margin, not its mean:
/// the deterministic `sqrt(A^2 - B^2)` is the RMS survivor count, and Jensen's
/// inequality pulls the mean below it by roughly `Var / (8 * mean^3)`.
fn margin_stats(law: Law, a0: usize, b0: usize, runs: usize, rng: &mut Rng) -> (f64, f64) {
    let mut sum = 0.0;
    let mut sum_sq = 0.0;
    for _ in 0..runs {
        let (a, b, _, _) = fight(law, a0, b0, rng);
        let margin = a as f64 - b as f64;
        sum += margin;
        sum_sq += margin * margin;
    }
    let n = runs as f64;
    (sum / n, (sum_sq / n).sqrt())
}

fn main() {
    let mut rng = Rng::new(1916);
    let mut json = String::from("{");

    // Sweep: A against 100 defenders, A from 60 to 140.
    //
    // The square-law column is the mean margin, which sits below `square_theory`
    // because a stochastic battle is a random walk: `square_rms` is the quantity
    // the theory actually predicts, and it lands on the curve.
    let sizes: Vec<usize> = (60..=140).step_by(10).collect();
    let (mut sq, mut sq_rms, mut li) = (vec![], vec![], vec![]);
    let (mut sq_theory, mut li_theory) = (vec![], vec![]);
    for &a0 in &sizes {
        let (mean, rms) = margin_stats(Law::Square, a0, 100, 2000, &mut rng);
        sq.push(mean);
        sq_rms.push(rms);
        li.push(margin_stats(Law::Linear, a0, 100, 2000, &mut rng).0);
        let d = (a0 * a0) as f64 - 10_000.0;
        sq_theory.push(d.signum() * d.abs().sqrt());
        li_theory.push(a0 as f64 - 100.0);
    }
    json += &format!(
        "\"sweep\":{{\"a0\":{},\"square\":{},\"square_rms\":{},\"linear\":{},\"square_theory\":{},\"linear_theory\":{}}},",
        json_arr(&sizes.iter().map(|&x| x as f64).collect::<Vec<_>>()),
        json_arr(&sq),
        json_arr(&sq_rms),
        json_arr(&li),
        json_arr(&sq_theory),
        json_arr(&li_theory)
    );

    // The headline fight: 100 against 70, averaged, plus one traced battle of each kind.
    // Track the margin (A survivors minus B survivors), not just A's survivors, so a
    // rare upset shows up as a negative result instead of a zero.
    let (mut sq_margins, mut li_margins) = (vec![], vec![]);
    for _ in 0..4000 {
        let (a, b, _, _) = fight(Law::Square, 100, 70, &mut rng);
        sq_margins.push(a as f64 - b as f64);
        let (a, b, _, _) = fight(Law::Linear, 100, 70, &mut rng);
        li_margins.push(a as f64 - b as f64);
    }
    let (m_sq, s_sq) = mean_sd(&sq_margins);
    let (m_li, s_li) = mean_sd(&li_margins);
    let (head_rms, head_theory) = (rms(&sq_margins), (10_000.0_f64 - 4_900.0).sqrt());
    println!(
        "100 v 70: square mean {m_sq:.1} (sd {s_sq:.1}) rms {head_rms:.1} theory {head_theory:.1}"
    );
    println!("100 v 70: linear mean {m_li:.1} (sd {s_li:.1}) theory 30.0");
    json += &format!(
        "\"headline\":{{\"square\":[{m_sq:.3},{s_sq:.3}],\"square_rms\":{head_rms:.3},\"square_theory\":{head_theory:.3},\"linear\":[{m_li:.3},{s_li:.3}]}},"
    );

    // Defeat in detail: army A of 100 meets two forces of 50 one after the other, and keeps whoever survives the first fight.
    let divided = |law: Law, rng: &mut Rng| -> f64 {
        let first = fight(law, 100, 50, rng).0;
        if first == 0 {
            0.0
        } else {
            fight(law, first, 50, rng).0 as f64
        }
    };
    let (d_sq, d_li): (Vec<f64>, Vec<f64>) = (0..4000)
        .map(|_| {
            (
                divided(Law::Square, &mut rng),
                divided(Law::Linear, &mut rng),
            )
        })
        .unzip();
    let (dm_sq, dm_li) = (mean_sd(&d_sq).0, mean_sd(&d_li).0);
    let dm_sq_rms = rms(&d_sq);
    println!(
        "100 v 50 then 50: square leaves {dm_sq:.1} (rms {dm_sq_rms:.1}, theory 70.7), linear leaves {dm_li:.1} (theory 0)"
    );
    json += &format!(
        "\"divided\":{{\"square\":{dm_sq:.3},\"square_rms\":{dm_sq_rms:.3},\"linear\":{dm_li:.3}}},"
    );

    // Traced battles for the animation: pick a run whose survivors sit near the mean.
    let mut traces = Vec::new();
    for law in [Law::Square, Law::Linear] {
        let target = if law == Law::Square { m_sq } else { m_li };
        let best = (0..200)
            .map(|_| fight(law, 100, 70, &mut rng))
            .min_by(|x, y| {
                let x_margin = (x.0 as f64 - x.1 as f64 - target).abs();
                let y_margin = (y.0 as f64 - y.1 as f64 - target).abs();
                x_margin.total_cmp(&y_margin)
            })
            .expect("at least one run");
        let ints = |v: &[u32]| {
            format!(
                "[{}]",
                v.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
            )
        };
        traces.push(format!(
            "{{\"a_dead\":{},\"b_dead\":{},\"a_left\":{},\"b_left\":{}}}",
            ints(&best.2),
            ints(&best.3),
            best.0,
            best.1
        ));
    }
    json += &format!(
        "\"trace_square\":{},\"trace_linear\":{}}}",
        traces[0], traces[1]
    );
    fs::create_dir_all("results").expect("create results dir");
    fs::write("results/lanchester.json", json).expect("write results");
}
