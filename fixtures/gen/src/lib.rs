//! Deterministic synthetic option chains.
//!
//! The same [`ChainSpec`] always produces the same chain on a given platform.
//! Randomness comes from `SplitMix64` and pure arithmetic, so it is identical
//! everywhere; the IV surface and quotes go through `exp`/`ln`, whose last
//! bit can differ between platform math libraries, so IVs are rounded to
//! 1e-6 and quotes to the 0.01 tick. The committed files under
//! `fixtures/golden` are the source of truth for golden tests.

use gex_core::bsm::{self, BsmInput};
use gex_core::{Chain, OptionContract, OptionKind};

/// One calendar day as a year fraction.
pub const DAY: f64 = 1.0 / 365.0;
/// Time left in a 0DTE session with four hours to the close.
pub const ZERO_DTE: f64 = 4.0 / 24.0 * DAY;

/// `SplitMix64` (Steele, Lea and Flood 2014).
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// Seeds the generator.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)` with 53 random bits.
    #[allow(clippy::cast_precision_loss)] // a 53-bit integer is exact in f64
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1_u64 << 53) as f64)
    }

    /// Approximately standard normal: sum of 12 uniforms minus 6. Uses only
    /// arithmetic, so it is bit-identical on every platform.
    pub fn normal(&mut self) -> f64 {
        (0..12).map(|_| self.uniform()).sum::<f64>() - 6.0
    }
}

/// Parameters for [`generate`].
#[derive(Debug, Clone, PartialEq)]
pub struct ChainSpec {
    /// RNG seed.
    pub seed: u64,
    /// Underlying spot.
    pub spot: f64,
    /// Risk-free rate.
    pub rate: f64,
    /// Dividend yield.
    pub div_yield: f64,
    /// Expiries as year fractions.
    pub expiries: Vec<f64>,
    /// Strike spacing.
    pub strike_step: f64,
    /// Strikes on each side of the at-the-money strike.
    pub strikes_each_side: u32,
    /// ATM implied volatility.
    pub base_iv: f64,
    /// IV added per unit of `-ln(K/S)`: positive gives the usual put skew.
    pub skew: f64,
    /// IV added per unit of `ln(K/S)²`.
    pub smile: f64,
    /// Peak open interest near the money.
    pub oi_scale: f64,
    /// Width of the open interest hump, as a fraction of spot.
    pub oi_width: f64,
    /// Put OI multiplier below spot over call OI above it; > 1 is put-heavy.
    pub put_tilt: f64,
    /// Fraction of contracts left without a quote.
    pub missing_quote_rate: f64,
}

/// Builds the chain described by `spec`.
///
/// # Panics
///
/// Panics if `spec` yields an invalid chain (non-positive spot or strikes).
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
// Strike indices are small integers; OI is rounded from a non-negative f64.
pub fn generate(spec: &ChainSpec) -> Chain {
    let mut rng = Rng::new(spec.seed);
    let atm = (spec.spot / spec.strike_step).round() * spec.strike_step;
    let n = i64::from(spec.strikes_each_side);
    let mut contracts = Vec::new();
    for &expiry in &spec.expiries {
        for i in -n..=n {
            let strike = atm + i as f64 * spec.strike_step;
            if strike <= 0.0 {
                continue;
            }
            let m = (strike / spec.spot).ln();
            // Wings steepen as expiry shortens, as on real 0DTE surfaces.
            let term = (30.0 * DAY / expiry.max(DAY / 24.0)).sqrt().min(6.0);
            for kind in [OptionKind::Call, OptionKind::Put] {
                let iv = (spec.base_iv - spec.skew * m * term
                    + spec.smile * m * m * term * term
                    + 0.005 * rng.normal())
                .max(0.02);
                let iv = round_to(iv, 1e6);

                let hump = (-0.5 * (m / spec.oi_width).powi(2)).exp();
                let round_strike = if (strike / (5.0 * spec.strike_step)).fract() == 0.0 {
                    2.0
                } else {
                    1.0
                };
                let tilt = match kind {
                    OptionKind::Put if strike < spec.spot => spec.put_tilt,
                    OptionKind::Call if strike > spec.spot => 1.0,
                    _ => 0.3,
                };
                let noise = (0.4 * rng.normal()).exp();
                let oi = (spec.oi_scale * hump * round_strike * tilt * noise)
                    .round()
                    .max(0.0) as u64;
                let volume = (oi as f64 * 0.5 * rng.uniform()).round() as u64;

                let input = BsmInput {
                    kind,
                    spot: spec.spot,
                    strike,
                    t: expiry,
                    rate: spec.rate,
                    div_yield: spec.div_yield,
                    sigma: iv,
                };
                let fair = bsm::price(&input);
                let quoted = rng.uniform() >= spec.missing_quote_rate;
                let half = (0.02 * fair).max(0.05);
                let (bid, ask) = if quoted {
                    (
                        Some(round_to((fair - half).max(0.0), 100.0)),
                        Some(round_to(fair + half, 100.0)),
                    )
                } else {
                    (None, None)
                };
                let c = OptionContract::new(kind, strike, expiry, iv, oi)
                    .and_then(|c| c.with_volume(volume).with_quote(bid, ask))
                    .expect("generated contract is valid");
                contracts.push(c);
            }
        }
    }
    Chain::new(spec.spot, spec.rate, spec.div_yield, contracts).expect("generated chain is valid")
}

/// Rounds to `1 / per_unit` (e.g. `per_unit = 100.0` rounds to cents).
/// Dividing by the exact integer gives the double nearest the decimal, so
/// files show `0.408185` rather than `0.40818499999999996`.
fn round_to(x: f64, per_unit: f64) -> f64 {
    (x * per_unit).round() / per_unit
}

/// Named fixtures written to `fixtures/golden` by the `gen` binary.
pub mod presets {
    use super::{ChainSpec, DAY, ZERO_DTE, generate};
    use gex_core::{Chain, OptionContract, OptionKind};

    /// Index-like 0DTE chain, about 160 contracts.
    #[must_use]
    pub fn small_0dte() -> ChainSpec {
        ChainSpec {
            seed: 1,
            spot: 5_012.5,
            rate: 0.045,
            div_yield: 0.013,
            expiries: vec![ZERO_DTE],
            strike_step: 5.0,
            strikes_each_side: 40,
            base_iv: 0.14,
            skew: 0.6,
            smile: 2.0,
            oi_scale: 2_000.0,
            oi_width: 0.02,
            put_tilt: 0.5,
            missing_quote_rate: 0.05,
        }
    }

    /// ETF-like chain with four expiries, about 400 contracts.
    #[must_use]
    pub fn multi_expiry() -> ChainSpec {
        ChainSpec {
            seed: 2,
            spot: 451.3,
            rate: 0.045,
            div_yield: 0.013,
            expiries: vec![DAY, 7.0 * DAY, 30.0 * DAY, 90.0 * DAY],
            strike_step: 1.0,
            strikes_each_side: 25,
            base_iv: 0.16,
            skew: 0.5,
            smile: 1.5,
            oi_scale: 5_000.0,
            oi_width: 0.03,
            put_tilt: 0.6,
            missing_quote_rate: 0.1,
        }
    }

    /// Put-heavy chain whose total GEX at spot is negative.
    #[must_use]
    pub fn negative_gamma() -> ChainSpec {
        ChainSpec {
            seed: 3,
            spot: 98.7,
            rate: 0.03,
            div_yield: 0.0,
            expiries: vec![3.0 * DAY, 10.0 * DAY],
            strike_step: 1.0,
            strikes_each_side: 15,
            base_iv: 0.35,
            skew: 0.4,
            smile: 1.0,
            oi_scale: 800.0,
            oi_width: 0.06,
            put_tilt: 6.0,
            missing_quote_rate: 0.0,
        }
    }

    /// Full-size 0DTE chain for benches: 1,501 strikes, 3,002 contracts.
    #[must_use]
    pub fn bench_0dte() -> ChainSpec {
        ChainSpec {
            seed: 4,
            spot: 5_803.2,
            strikes_each_side: 750,
            oi_width: 0.04,
            ..small_0dte()
        }
    }

    /// Hand-built edge cases: zero IV, expired, deep ITM/OTM, one-sided and
    /// missing quotes, zero open interest, a strike with only a call.
    ///
    /// # Panics
    ///
    /// Never; the literals are valid.
    #[must_use]
    pub fn edge_cases() -> Chain {
        let c = |kind, strike, expiry, iv, oi| {
            OptionContract::new(kind, strike, expiry, iv, oi).expect("valid")
        };
        let (call, put) = (OptionKind::Call, OptionKind::Put);
        let contracts = vec![
            c(call, 100.0, 0.0, 0.25, 500),
            c(put, 100.0, 0.0, 0.25, 400),
            c(call, 100.0, 2.0 * DAY, 0.0, 300),
            c(put, 100.0, 2.0 * DAY, 0.3, 300)
                .with_quote(Some(1.1), Some(1.3))
                .expect("valid"),
            c(call, 100.0, 2.0 * DAY, 0.3, 0)
                .with_quote(Some(1.4), None)
                .expect("valid"),
            c(call, 1.0, 2.0 * DAY, 0.8, 50),
            c(put, 1.0, 2.0 * DAY, 0.8, 50),
            c(call, 500.0, 2.0 * DAY, 0.9, 50),
            c(put, 500.0, 2.0 * DAY, 0.9, 50)
                .with_quote(Some(399.0), Some(401.0))
                .expect("valid"),
            c(call, 99.5, 1e-9, 0.2, 1_000),
            c(put, 100.5, 1e-9, 0.2, 1_000),
            c(call, 103.0, 2.0 * DAY, 0.28, 250).with_volume(17),
        ];
        Chain::new(100.2, 0.04, 0.01, contracts).expect("valid")
    }

    /// Every golden fixture by file stem.
    #[must_use]
    pub fn golden() -> Vec<(&'static str, Chain)> {
        vec![
            ("small_0dte", generate(&small_0dte())),
            ("multi_expiry", generate(&multi_expiry())),
            ("negative_gamma", generate(&negative_gamma())),
            ("edge_cases", edge_cases()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix_reference_values() {
        // Seed 0's first output is the widely published SplitMix64 value; the
        // seed 1234567 outputs come from an independent Python implementation.
        assert_eq!(Rng::new(0).next_u64(), 0xe220_a839_7b1d_cdaf);
        let mut r = Rng::new(1_234_567);
        assert_eq!(r.next_u64(), 6_457_827_717_110_365_317);
        assert_eq!(r.next_u64(), 3_203_168_211_198_807_973);
    }

    #[test]
    fn same_seed_same_chain() {
        let a = generate(&presets::small_0dte());
        let b = generate(&presets::small_0dte());
        assert_eq!(a, b);
        let other = generate(&ChainSpec {
            seed: 99,
            ..presets::small_0dte()
        });
        assert_ne!(a, other);
    }

    #[test]
    fn bench_chain_has_about_3000_contracts() {
        assert_eq!(generate(&presets::bench_0dte()).contracts().len(), 3_002);
    }

    #[test]
    fn negative_gamma_preset_is_negative() {
        let r = gex_core::analyze(
            &generate(&presets::negative_gamma()),
            &gex_core::ExposureConfig::default(),
        )
        .unwrap();
        assert_eq!(r.regime, gex_core::Regime::NegativeGamma);
    }
}
