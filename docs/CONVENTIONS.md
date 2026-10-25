# Conventions

This is the full statement of the units, signs and rules `gex-core` uses.
The Python reference (`fixtures/reference/gex_reference.py`) follows the
same rules, and the golden tests hold the two together.

## Inputs

| Quantity | Unit | Notes |
|---|---|---|
| `spot`, `strike`, `bid`, `ask` | price | `spot > 0`, `strike > 0`; `0 <= bid <= ask` |
| `expiry` (`T`) | year fraction | `T >= 0`. The caller picks the day count. |
| `iv` (`σ`) | decimal (0.20 = 20%) | `iv <= 0` is accepted but skipped by exposures (see below) |
| `rate` (`r`), `div_yield` (`q`) | decimal, continuously compounded | may be negative |
| `open_interest`, `volume` | contracts | `u64` |
| `multiplier` | units per contract | on `Chain`, default 100 |

**Expiry groups.** Expiries are grouped with `EXPIRY_TOLERANCE = 1e-9`
years (about 0.03 s). Sorted expiries join the current group while they are
within the tolerance of its first (smallest) member, which represents the
group in `Chain::expiries()`. The groups partition the chain: each contract
is in exactly one. `Chain::contracts_expiring(e)` returns the group `e` falls
in: the last representative `r <= e` with `e - r < 1e-9`, or failing that the
first representative above `e` within `1e-9`. It is empty if neither exists.

## Pricing model

Black-Scholes-Merton for European options with a continuous dividend yield:

```
d1 = (ln(S/K) + (r − q + σ²/2)·T) / (σ√T)        d2 = d1 − σ√T
call = S·e^{−qT}·N(d1) − K·e^{−rT}·N(d2)         put = K·e^{−rT}·N(−d2) − S·e^{−qT}·N(−d1)
Δ_call = e^{−qT}·N(d1)                           Δ_put = −e^{−qT}·N(−d1)
Γ = e^{−qT}·φ(d1) / (S·σ√T)                      vega = S·e^{−qT}·φ(d1)·√T
vanna = ∂Δ/∂σ = −e^{−qT}·φ(d1)·d2/σ
charm = −∂Δ/∂T
      = ±q·e^{−qT}·N(±d1) − e^{−qT}·φ(d1)·(2(r−q)T − d2·σ√T)/(2T·σ√T)   (+ call, − put)
```

`bsm` returns vega and vanna **per 1.00 of volatility** and charm **per
year**. Charm's sign means the delta drift as time passes: a positive charm
means delta rises as expiry approaches.

`N` and `φ` are implemented in `math.rs` (Cody's rational approximations for
`erfc`). Relative accuracy is about 1e-15 near the centre, and in the lower
tail it falls off like `x²·1e-16`.

### Degenerate inputs

When `σ <= 0` or `T <= 0` (with negative `T` treated as 0), or when `σ√T`
underflows, the functions return limits instead of NaN:

- price: discounted intrinsic value, `max(±(S·e^{−qT} − K·e^{−rT}), 0)`
- delta: a step function, `e^{−qT}` for a call when `S·e^{−qT} > K·e^{−rT}`,
  0 below that and half of it exactly at the money-forward (mirrored for puts)
- gamma, vega, vanna, charm: 0

## Exposures

Each contract contributes `w = sign · OI · multiplier` times a greek:

| Exposure | Formula | Meaning |
|---|---|---|
| GEX | `w · Γ · S² · 0.01` | dollar gamma per 1% move in spot |
| DEX | `w · Δ · S` | dollar delta |
| Vanna | `w · vanna · S · 0.01` | change in dollar delta per 1 vol point |
| Charm | `w · charm · S / 365` | drift in dollar delta per calendar day |

- **Dealer sign.** By default `call_sign = +1` and `put_sign = −1`: dealers
  are assumed long the calls customers sold and short the puts customers
  bought. The same signs apply to all four exposures, so DEX is the dealer's
  delta (short puts give positive DEX). Both signs are configurable.
- **Time floor.** Exposures use `max(T, min_time)`, default one minute
  (`1/(365·1440)` years), so ATM gamma stays finite on expiring contracts.
  The pure `bsm` functions are not floored.
- **Missing IV.** Contracts with `iv <= 0` contribute nothing and are counted
  in `ExposureReport::skipped`. Their open interest still appears in
  `call_oi` / `put_oi`. If *every* contract has `iv <= 0`, `analyze` and
  `zero_gamma` return `GexError::NoUsableContracts { skipped }` instead of a
  report, so the count is never lost; `GexError::EmptyChain` means the chain
  has no contracts at all.
- **Per strike.** Rows sum every expiry at a strike and come back in
  ascending strike order.
- **Determinism.** Contracts are sorted into a canonical order over every
  field before summing or picking, so results are bit-identical whatever
  the input order.

## Regime

`total_gex.abs() <= neutral_band` gives `Neutral` (the default band is 0).
Otherwise the sign of total GEX at spot gives `PositiveGamma` or
`NegativeGamma`.

## Zero-gamma flip

Total GEX is evaluated on a spot grid. The default grid runs from `0.9·S`
to `1.1·S` in 201 points, ends included, as
`S·(lower + (upper − lower)·j/(n−1))`. Each contract's IV is held fixed as
spot moves (sticky strike), and T and r are unchanged.

**Negligible values.** Near expiry, gamma underflows to exactly 0 a few
percent from every strike, and far-wing tails can change sign at
magnitudes like 1e-30 dollars. So `|GEX| <= negligible_gex` counts as zero,
where

```
negligible_gex = 1e-9 · Σ_{iv > 0} |sign · OI · multiplier| · Γ(S = K) · K² · 0.01
```

is a fraction (`NEGLIGIBLE_GEX_FRACTION`) of the sum of each contract's
peak dollar gamma, taken with spot at its own strike. It doesn't depend on
the grid.

**Crossings.** A crossing is a sign change between consecutive
non-negligible grid values:

- **Neighbouring points:** the bracket is bisected until it is narrower
  than `1e-10·S`, and the midpoint is taken.
- **Across a negligible gap:** both edges of the gap (where `|GEX|` falls
  to the threshold) are found by bisection, and the crossing is the
  midpoint of the two edges. The same applies if bisection between
  neighbouring points lands in a negligible value.

`ZeroGamma::crossings` lists every crossing in ascending order, and `level`
is the one nearest spot (the lower one on a tie). The result is `None` when
every grid value is negligible (for example, zero open interest) or when the
non-negligible values all have one sign (for example, a calls-only chain).

## Levels

Ties throughout are broken by **distance to spot, then the lower strike**.

- **Walls.** The call wall is the strike with the largest `|call_gex|`, and
  the put wall the strike with the largest `|put_gex|`. Strikes with zero
  GEX are ignored. Under the default signs these are the largest positive
  call GEX and the most negative put GEX.
- **Max pain** (per expiry). The candidates are the strikes listed for that
  expiry. `pain(P) = Σ_calls OI·max(P − K, 0) + Σ_puts OI·max(K − P, 0)`,
  computed directly. Values within `1e-12` relative of the minimum count as
  ties.
- **Expected move** (per expiry). The straddle strike is the strike nearest
  spot that lists both a call and a put. Each leg uses its mid
  (`(bid + ask)/2`, when both are quoted and `ask > 0`), or otherwise the
  BSM price at the contract's own `T` and IV. The source is recorded. The
  result is the raw straddle, `lower = S − straddle` and
  `upper = S + straddle`. No 0.85 factor is applied, and no ±1σ conversion
  is made.

## Fixtures and golden tolerances

`fixtures/golden/<name>.chain.json` is a serialized `Chain`:
`{spot, rate, div_yield, multiplier, contracts: [{kind, strike, expiry, iv,
open_interest, volume, bid, ask}]}`, where `kind` is `"call"` or `"put"` and
a missing quote is `null`. `<name>.expected.json` is the Python reference's
output. `tests/golden.rs` checks:

| Output | Tolerance |
|---|---|
| per-contract greeks | `|a − b| <= 1e-12 + 1e-9·|b|` |
| per-strike and total exposures | `|a − b| <= 1e-9 · Σ|contributions|` |
| walls, max pain, straddle strike, regime, skipped, OI | exact |
| zero-gamma crossings and level | `1e-6 · spot` |
| straddle, lower, upper | `1e-12 + 1e-9·|b|` |
