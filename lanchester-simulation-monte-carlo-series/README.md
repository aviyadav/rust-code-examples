# Monte Carlo Series

Two independent Monte Carlo experiments live in this crate:

- **Lanchester's laws** — the main binary: a dependency-free, unit-level
  combat simulator, described first below.
- **Service latency** — Cargo examples: p50/p95 and SLA estimation for a
  three-stage request path, available sequentially and with Rayon. See
  [Service latency Monte Carlo example](#service-latency-monte-carlo-example).

The main binary uses only the standard library. The latency examples pull in
`rand` 0.8, `rand_distr` 0.4, and `rayon` 1 as dev-dependencies, so the combat
simulator stays dependency-free.

**Lanchester's laws.** A unit-level Monte Carlo simulator of [Lanchester's two
laws of combat](https://en.wikipedia.org/wiki/Lanchester%27s_laws):

- **Square law** — aimed fire, every soldier can hit every enemy.
- **Linear law** — melee, only a limited front can engage.

Every soldier is simulated as an individual, and each step is one simultaneous
volley. The two laws are therefore *emergent*: the code never integrates the
Lanchester differential equations, it just lets thousands of soldiers shoot at
each other and averages the result. That is the point of the Monte Carlo
treatment — the classical laws describe the deterministic limit, and the
simulation shows how far an individual battle can stray from it.

## Quick start

```bash
cargo run --release
```

The main binary needs no external crates (Rust 2024, standard library only). Run
it from the crate root: it prints three summary lines and writes
[`results/lanchester.json`](results/lanchester.json), creating `results/` if
needed. A full run takes about a second.

```text
100 v 70: square mean 67.8 (sd 8.1) rms 68.3 theory 71.4
100 v 70: linear mean 29.7 (sd 11.4) theory 30.0
100 v 50 then 50: square leaves 65.0 (rms 65.5, theory 70.7), linear leaves 5.5 (theory 0)
```

> **Verdict on accuracy:** the model is correct, but the natural output — the
> *mean* survivor margin — is **not** the quantity the square law predicts.
> The square law pins down the *mean square* of the margin, so its correct
> finite-sample counterpart is the **RMS** margin. The README explains this and
> the small residual discretization bias below. See
> [Accuracy](#accuracy-what-matches-and-what-does-not).

## The theory

Both armies are identical in quality, so both sides have the same per-shot kill
probability `P`. All differences come from the geometry of the fight.

### Square law (aimed fire)

Every living soldier can target every living enemy. With army sizes `A` and `B`:

```text
dA/dt = -P * B        dB/dt = -P * A
```

The combination `A² - B²` is conserved:

```text
d/dt (A² - B²) = 2A(-P B) - 2B(-P A) = 0
```

So the deterministic prediction is

```text
margin = sign(A0² - B0²) * sqrt(|A0² - B0²|)
```

`A` wins if and only if `A0 > B0`. An army's strength is the **square** of its
size, hence the name.

### Linear law (melee)

Only a frontage `F` can fight, so casualties per volley are capped instead of
scaling with army size:

```text
dA/dt = -P * min(A, B, F)      dB/dt = -P * min(A, B, F)
```

Now the *difference* is conserved:

```text
d/dt (A - B) = 0
```

The deterministic prediction is simply

```text
margin = A0 - B0
```

An army's strength is **linear** in its size: concentrating force pays off.

| | Square law | Linear law |
|---|---|---|
| Combat | Aimed fire | Melee / area fire |
| Casualty rate | proportional to enemy strength | capped by frontage |
| Conserved quantity | `A² - B²` | `A - B` |
| Deterministic survivors | `sqrt(A0² - B0²)` | `A0 - B0` |
| Force concentration | rewarded superlinearly | rewarded linearly |

## How one battle is simulated

`fight(law, a0, b0, rng)` in [`src/main.rs`](src/main.rs) runs a single battle:

1. Each side is a list of living soldiers. Each step is **one simultaneous
   volley**: both sides fire from the roster they had at the *start* of the
   step, and all casualties are applied afterwards. This removes any
   first-mover advantage.
2. **Square law:** every living soldier fires once. The shot kills with
   probability `P`, and its target is drawn uniformly from the living enemy
   roster *with replacement*.
3. **Linear law:** `engaged = min(|A|, |B|, FRONT)` soldier pairs fight. Each
   pair makes two independent kill attempts, one in each direction. The front
   is refilled from the survivors every step.
4. Each casualty records the step on which it died (first landing hit wins).
   The dead are removed from the roster.
5. Repeat until one side is empty. The survivors and the per-unit death steps
   are returned.

A square-law soldier is always a threat; a linear-law soldier is only a threat
when it can reach the front. That is the entire difference between the laws in
this model.

### Parameters

| Parameter | Code | Value | Meaning |
|---|---|---|---|
| `P` | `const P` | `0.05` | Kill probability per shot/blow, per volley |
| `F` | `const FRONT` | `10` | Pairs that can engage in melee |
| Seed | `Rng::new(1916)` | `1916` | Fixed RNG seed (reproducible runs) |
| Sweep samples | `margin_stats(..., 2000, ...)` | `2000` | Battles per sweep point |
| Headline samples | `for _ in 0..4000` | `4000` | Battles per law at 100 v 70 |
| Divide-and-conquer samples | `(0..4000)` | `4000` | Two-stage battles |
| Trace candidates | `(0..200)` | `200` | Candidate battles for the animation trace |

Raising `P` makes battles shorter and coarser; lowering it makes them longer
and closer to the continuous-time ideal. `FRONT` controls how strongly the
linear law caps the casualty rate. The seed and sample counts only trade
accuracy for runtime.

## Random numbers

The simulator ships its own RNG so results are exactly reproducible with no
external crates:

- **xoshiro256++**, seeded through **SplitMix64** (`Rng::new`).
- `unit()` returns a uniform `f64` in `[0, 1)` from the top 53 random bits.
- `below(n)` returns a uniform index in `0..n`.
- One fixed seed for the whole run means two runs produce byte-identical
  output.

## Output format

`results/lanchester.json` is a single JSON object. Field names and nesting are
shown below (values abbreviated):

```json
{
  "sweep": {
    "a0": [60, 70, 80, 90, 100, 110, 120, 130, 140],
    "square": [],
    "square_rms": [],
    "linear": [],
    "square_theory": [],
    "linear_theory": []
  },
  "headline": {
    "square": [0, 0],
    "square_rms": 0,
    "square_theory": 0,
    "linear": [0, 0]
  },
  "divided": {
    "square": 0,
    "square_rms": 0,
    "linear": 0
  },
  "trace_square": {
    "a_dead": [],
    "b_dead": [],
    "a_left": 0,
    "b_left": 0
  },
  "trace_linear": { "a_dead": [], "b_dead": [], "a_left": 0, "b_left": 0 }
}
```

### `sweep` — 60…140 attackers vs 100 defenders

For each attacker size `a0` and each law, `2000` independent battles are played.

| Field | Meaning |
|---|---|
| `a0` | Attacker sizes, 60 to 140 in steps of 10 |
| `square` | **Mean signed margin** (`A` survivors − `B` survivors) under the square law |
| `square_rms` | **Root-mean-square margin** under the square law |
| `linear` | Mean signed margin under the linear law |
| `square_theory` | `sign(a0² − 100²) · sqrt(|a0² − 100²|)` |
| `linear_theory` | `a0 − 100` |

A positive margin means `A` holds the field, a negative one means `B` does.
The margin (rather than only `A`'s survivors) is used so that a rare upset is
recorded as a negative result instead of a zero.

### `headline` — 100 v 70

`4000` battles per law. `square` and `linear` are `[mean, standard deviation]`
of the margin. `square_rms` and `square_theory` are the RMS margin and the
deterministic prediction `sqrt(100² − 70²) = 71.41`.

### `divided` — defeat in detail

Army `A` (100) fights a force of 50, then — with whoever survived the first
battle — fights another 50. `4000` trials per law.

- `square`: mean `A` survivors after the second battle; `square_rms` its RMS.
- `linear`: mean `A` survivors after the second battle.

Deterministic predictions: square law `sqrt(100² − 50² − 50²) = 70.71`;
linear law `(100 − 50) − 50 = 0`.

### `trace_square` / `trace_linear` — animation data

One representative 100 v 70 battle per law, chosen from 200 candidates as the
one whose margin is closest to the mean.

| Field | Meaning |
|---|---|
| `a_dead[i]` | Volley number in which soldier `i` of `A` died; `0` means it survived |
| `b_dead[i]` | Same for `B` |
| `a_left` | `A` survivors at the end |
| `b_left` | `B` survivors at the end |

Death steps are 1-based (`step` starts at 1), so `0` is an unambiguous
"survived" marker.

## Results

### Sweep: mean and RMS margin, 2000 battles per point

| `a0` | square mean | square RMS | square theory | linear mean | linear theory |
|---:|---:|---:|---:|---:|---:|
| 60 | −77.12 | 77.36 | −80.00 | −39.83 | −40 |
| 70 | −67.89 | 68.34 | −71.41 | −30.24 | −30 |
| 80 | −55.42 | 56.76 | −60.00 | −20.14 | −20 |
| 90 | −35.93 | 42.14 | −43.59 | −9.98 | −10 |
| 100 | 1.48 | 32.74 | 0.00 | −0.39 | 0 |
| 110 | 35.89 | 43.92 | 45.83 | 9.86 | 10 |
| 120 | 60.90 | 62.74 | 66.33 | 19.91 | 20 |
| 130 | 78.05 | 78.91 | 83.07 | 29.58 | 30 |
| 140 | 93.18 | 93.67 | 97.98 | 40.13 | 40 |

### Headline and divide-and-conquer

| Scenario | Mean margin | SD | RMS | Deterministic prediction |
|---|---:|---:|---:|---:|
| 100 v 70, square | 67.79 | 8.15 | 68.28 | 71.41 |
| 100 v 70, linear | 29.69 | 11.43 | — | 30.00 |
| 100 → 50, then 50, square | 64.98 | — | 65.51 | 70.71 |
| 100 → 50, then 50, linear | 5.45 | — | — | 0.00 |

## Accuracy: what matches and what does not

### The linear law matches very closely

Across the whole sweep, the simulated mean margin is within about **1.4 %** of
`a0 − 100` (e.g. −39.83 vs −40 at `a0 = 60`, 40.13 vs 40 at `a0 = 140`). This
is expected: each engagement removes the same expected number of soldiers from
both sides, so the difference is a martingale and its mean is preserved exactly.

### The square law pins down the *RMS*, not the mean

The conserved quantity is `A² − B²`, not `A − B`. At the end of a battle the
loser has zero soldiers, so `margin² = |A² − B²|`. On paths where the predicted
winner actually wins,

```text
E[margin²] ≈ A0² − B0²
RMS(margin) ≈ sqrt(A0² − B0²)   ← the deterministic prediction
```

In the sweep, `square_rms` tracks `square_theory` to within **3–5.5 %** away
from parity, while `square` (the mean) is pulled toward zero. The gap is Jensen's
inequality: on those decisive paths the margin is non-negative and `sqrt` is
concave, so `E[margin] < sqrt(E[margin²])`. The compression grows near parity,
where the battle-to-battle variance is largest:

- `a0 = 140` vs 100: mean −4.9 % below theory, RMS −4.4 %.
- `a0 = 110` vs 100: mean −21.7 % below theory, RMS −4.2 %.

At exact parity (`a0 = 100`) the deterministic prediction is `0`, and indeed the
mean margin is ≈ 0 (1.48 in this run). But a single battle is not "0": it is a
coin flip whose RMS margin is **32.74**. Whichever side gets an early edge
snowballs it. The deterministic law predicts the expectation, not the outcome of
one battle.

> This is why `square_rms` exists in the output. Plotting the mean against
> `square_theory` makes a correct model look systematically wrong; plotting the
> RMS makes the agreement visible.

### Small residual bias: finite volleys

At the shipped `P = 0.05` the RMS is still 3–5.5 % below theory. Two O(`P`)
discretization effects cause this:

1. **The doomed side gets a free last volley.** Because volleys are
   simultaneous, an army that is wiped out in a step still fires that step's
   full volley, removing a few enemies that continuous-time combat would never
   have let it kill.
2. **Wasted shots.** In the square law, targets are drawn with replacement from
   the living roster. Several soldiers can shoot at the same enemy within one
   volley; hits on an already-condemned target are wasted.

Both effects shrink as the volley becomes finer. Measured for the headline
100 v 70 battle (4000 samples):

| `P` | Mean | RMS | Deterministic theory |
|---:|---:|---:|---:|
| 0.05 (shipped) | 67.8 | 68.3 | 71.41 |
| 0.01 | 70.2 | 70.7 | 71.41 |
| 0.002 | 70.6 | 71.1 | 71.41 |

`P = 0.05` is kept so that a 100 v 70 battle resolves in roughly 20–200 volleys,
which keeps the JSON trace a reasonable length for animation. The bias is
documented rather than tuned away.

### Why "defeat in detail" disagrees with `0` for the linear law

The deterministic linear prediction is `0`: 100 split into two fights of 50 is
annihilated by an equal 100. The simulation returns a mean of **5.45**
survivors, and this is correct, not a bug.

The second battle starts at roughly 50 v 50. In a stochastic battle the
difference is a random walk: the expected difference stays at zero, but at the
moment one side reaches zero the other still holds `|difference| > 0`
survivors. Averaging the winner's residual gives `E|margin| ≈ 5.5`. This number
is stable when the time step shrinks (5.6 at `P = 0.01`, 5.7 at `P = 0.002`),
which confirms it is the genuine winner-take-all diffusion, not a
discretization artifact. The deterministic `0` means "no expected net force",
not "nobody left standing".

The square-law divided result, 64.98 (RMS 65.51) against 70.71, is lower for the
same reason as the headline: finite volleys plus the Jensen effect applied
twice in a chained battle.

### No correctness bugs found

The deviations above are statistical and discretization effects, not errors:

- Every soldier fires at most once per volley; the linear front is refilled each
  step; casualties take effect simultaneously.
- `A` and `B` are treated identically, so an equal-army battle is unbiased
  (at `P = 0.01`, a 20,000-battle check gave a mean margin of 0.43 with a
  standard error of 0.25).
- Death steps are recorded once (first landing hit) and `0` is reserved for
  survivors, so the trace is unambiguous.
- Results are deterministic: two runs with seed 1916 produce byte-identical
  JSON.

## Service latency Monte Carlo example

A second, independent Monte Carlo experiment models the end-to-end latency of
a request that passes through three stages:

| Stage | Distribution | Parameters | Resulting latency |
|---|---|---|---|
| Auth service | Normal | mean 40 ms, sd 5 ms | 40 ms mean |
| Database query | Log-normal | `mu = 3.8`, `sigma = 0.4` (log space) | median ≈ 44.7 ms, mean ≈ 48.4 ms |
| Third-party API | Uniform | 20–100 ms | 60 ms mean |

Each iteration samples one latency from each stage and sums them. The example
then estimates:

- **p50** and **p95** latency with `select_nth_unstable_by` (Quickselect, O(N)
  average) instead of a full O(N log N) sort.
- **SLA compliance**: the share of simulated requests with total latency at or
  below 200 ms.

The outcome buffer is pre-allocated with `Vec::with_capacity`, and sampling uses
`thread_rng()`, which is thread-local and lock-free.

### Running

```bash
cargo run --release --example latency_monte_carlo
cargo run --release --example latency_monte_carlo_parallel
```

The first example runs 100,000 iterations sequentially. The second runs
10,000,000 iterations across all cores with Rayon; `map_init(thread_rng, ...)`
gives each worker its own RNG, so there is no shared RNG state.

These examples are not seeded, so exact numbers vary between runs. A typical
run:

```text
p50 latency: 147.71 ms
p95 latency: 200.56 ms
P(total latency <= 200 ms): 94.84%
```

### Lognormal parameters

`rand_distr::LogNormal::new(mu, sigma)` takes the mean and standard deviation
of the **underlying normal distribution**, not the mean and sd of the latency
itself. With `mu = 3.8` and `sigma = 0.4`:

- median = `exp(3.8)` ≈ 44.7 ms
- mean = `exp(3.8 + 0.4² / 2)` ≈ 48.4 ms

A comment calling 3.8 the "mean" is referring to a log-space mean; the example
source states the resulting latencies explicitly.

### Notes

- Percentiles require every sample, so the parallel version still materializes
  a 10M-element `Vec<f64>` (about 80 MB). A streaming sketch (t-digest, DDSketch)
  would trade memory for approximate percentiles.
- The percentile index is the 0-based floor of `n * p`, so `p95` is the 95,001st
  smallest value for `n = 100,000`.
- The SLA check uses `<= 200 ms`; for continuous distributions, `<` and `<=`
  differ only on a probability-zero set.
- The sequential and parallel examples use different RNG streams, so their
  results agree statistically but not exactly.

## Verification

```bash
cargo build --release
cargo clippy --release --all-targets
cargo run --release
cargo run --release --example latency_monte_carlo
cargo run --release --example latency_monte_carlo_parallel
```

`Cargo.toml` enables the `clippy::pedantic` and `clippy::perf` lint groups; the
current code (main binary and examples) is warning-free. `--all-targets`
includes the examples.

The Lanchester binary is deterministic: because the seed is fixed, the printed
values and `results/lanchester.json` are identical on every run. Changing `P`,
`FRONT`, the seed, or the sample counts changes its numbers. The latency
examples use `thread_rng()` and therefore vary from run to run.

## Code map

| Item | Purpose |
|---|---|
| `Rng` | xoshiro256++ PRNG seeded through SplitMix64 |
| `P`, `FRONT` | Combat parameters |
| `Law` | `Square` or `Linear` |
| `fight` | One battle; returns survivors and per-unit death steps |
| `mean_sd` | Sample mean and standard deviation |
| `rms` | Root-mean-square |
| `margin_stats` | Mean and RMS margin over many battles |
| `main` | Runs the sweep, headline, divided, and trace experiments; writes JSON |
| `examples/latency_monte_carlo.rs` | Sequential three-stage latency Monte Carlo |
| `examples/latency_monte_carlo_parallel.rs` | Rayon-parallel latency Monte Carlo |

## Design choices

- **Simultaneous volleys.** Both sides use the start-of-step roster, so neither
  side gains a first-mover advantage.
- **Identical lethality.** `P` is shared by both armies, isolating the effect of
  force geometry from the effect of troop quality.
- **Targets with replacement in the square law.** A volley is simultaneous and
  uncoordinated; two soldiers may pick the same target, and the extra hit is
  wasted. This is what a real volley of independent aimed shots looks like.
- **Front rotation in the linear law.** Fallen front-rank soldiers are replaced
  from the survivors each step, so the front stays full while the army lasts.
- **Per-unit death steps.** The trace is enough to animate the battle without
  re-running the simulation.

## Limitations and possible extensions

- Lethality is a single constant; it does not separate accuracy, rate of fire,
  and stopping power, and it cannot model heterogeneous troop quality (`α ≠ β`).
- No morale, retreat, reinforcements, terrain, or ranged/melee force mix.
- The square law's with-replacement targeting wastes shots in a way that
  aimed fire would not; sampling targets without replacement (or tracking
  per-unit wounds) would remove this O(`P`) artifact.
- The JSON contains summary statistics only; confidence intervals and
  per-battle survivor histograms would make the uncertainty easier to read.
- Natural follow-ups: Lanchester with reinforcements, the salvo model for
  modern missile combat, and a small plotting script over
  `results/lanchester.json`.



> Run the python script `mini-simulation.py` to see the results.
```bash
uv run --with numpy mini-simulation.py
```
