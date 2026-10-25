# gexkit

A Rust workspace for options dealer-exposure analytics (GEX, DEX, vanna,
charm, zero-gamma flip, walls, max pain, expected move).

## Roadmap

- **Phase 1 (current):** `gex-core` — pure computation library, synthetic
  fixtures with a Python reference, property tests, benches, CI.
- Later phases (not started; do not add without an explicit request):
  data providers, a CLI, and an MCP server built on `gex-core`.

## Layout

```
crates/gex-core/          core library (types, BSM greeks, exposure engine, levels)
  src/error.rs            GexError
  src/types.rs            OptionKind, OptionContract, Chain
  src/math.rs             norm_cdf / norm_pdf (own implementation)
  src/bsm.rs              Black-Scholes-Merton price and greeks
  src/exposure/           per-strike exposure, totals, regime, zero-gamma flip
  src/levels.rs           walls, max pain, expected move
  tests/golden.rs         vs Python reference (needs the `serde` feature)
  tests/bsm_props.rs      proptest invariants for the greeks
  tests/exposure_props.rs proptest invariants for the engine and levels
  benches/chain.rs        criterion, 3,002-contract 0DTE chain
fixtures/gen/             gex-fixtures: seeded synthetic chain generator (lib + bin)
fixtures/reference/       gex_reference.py — stdlib-only Python reference
fixtures/golden/          committed *.chain.json inputs and *.expected.json outputs
docs/CONVENTIONS.md       full statement of units and sign conventions
```

## Commands

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy -p gex-core --lib -- -D warnings   # no features; dev-deps would enable serde
cargo test --workspace --all-features
cargo bench --workspace --no-run
```

Regenerate goldens (only when a convention or fixture intentionally changes):

```
cargo run -p gex-fixtures --bin gen -- fixtures/golden
py -3 -I fixtures/reference/gex_reference.py fixtures/golden
```

## Workflow rules

- Small commits; run fmt, clippy (pedantic, `-D warnings`) and test before each.
- Clippy pedantic is configured in `[workspace.lints]`. Allow a lint only
  locally (`#[allow]` on the item) with a one-line justification.
- `gex-core` stays dependency-light: `thiserror`, optional `serde`. No numeric
  crates — the normal CDF/PDF are implemented in `math.rs`.
- Determinism: outputs must not depend on input contract order. Sort before
  summing; break ties by distance to spot, then lower strike.
- Never change a golden file to make a test pass without explaining why the
  convention or fixture changed.
- Commit `*.proptest-regressions` files; proptest replays them first.
- Stress property tests before trusting a tolerance:
  `PROPTEST_CASES=20000 cargo test --release -p gex-core --test <name>`.
- Totals with cancelling terms need tolerances scaled by the sum of absolute
  contributions, not by the result (see `*_abs` in the expected JSON).
- serde_json uses `float_roundtrip`; without it parsed floats can be 1 ulp off.
- The Python reference must stay stdlib-only and written from textbook
  formulas, not ported from the Rust code.

## Conventions (authoritative summary; details in docs/CONVENTIONS.md)

- Rates, dividend yields and IVs are decimals, continuously compounded.
- `expiry` / `T` is a year fraction; the caller picks the day count.
  "Per day" quantities use T/365 (calendar days).
- Contract multiplier lives on `Chain`, default 100. OI and volume are `u64`.
- Dealer sign: calls +1, puts −1 (dealers long calls, short puts), applied
  uniformly to GEX, DEX, vanna and charm exposure; configurable.
- GEX = Γ·OI·mult·S²·0.01 (dollar gamma per 1% move).
- DEX = Δ·OI·mult·S (dollar delta).
- Vanna exposure = vanna·OI·mult·S·0.01 (dollar-delta change per 1 vol point).
- Charm exposure = charm·OI·mult·S / 365, with charm = −∂Δ/∂T
  (dollar-delta drift per calendar day as time passes).
- Greeks with σ ≤ 0 or T ≤ 0 return limits: discounted intrinsic price,
  step delta (half at the money), all other greeks 0. Never NaN.
- Exposure engine floors T at `min_time` (default 1 minute = 1/(365·1440)) and
  skips contracts with iv ≤ 0, counting them in `skipped`.
- Regime = sign of total GEX at spot with a neutral band (default 0).
- Zero-gamma flip: sweep spot over ±10% in 201 points with sticky-strike IV,
  refine each sign change by bisection; report all crossings, `level` is the
  one nearest spot.
- Call wall = strike with largest call GEX; put wall = strike with most
  negative put GEX.
- Max pain and expected move take an `expiry` (matched within 1e-9 years).
- Expected move = raw ATM straddle at the strike nearest spot; each leg uses
  mid when bid and ask exist, else the BSM model price (source recorded).
