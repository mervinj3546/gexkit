//! Dealer exposure by strike and in total.
//!
//! Every contract contributes `sign · OI · multiplier` times a greek, where
//! `sign` is the assumed dealer position (`call_sign`, `put_sign`). Units:
//!
//! | Exposure | Formula | Meaning |
//! |---|---|---|
//! | GEX   | `Γ · S² · 0.01`   | dollar gamma per 1% move |
//! | DEX   | `Δ · S`           | dollar delta |
//! | vanna | `vanna · S · 0.01` | dollar-delta change per 1 vol point |
//! | charm | `charm · S / 365` | dollar-delta drift per calendar day |
//!
//! Volatility is held per contract (sticky strike) when spot is moved.

mod flip;

pub use flip::{NEGLIGIBLE_GEX_FRACTION, ZeroGamma, negligible_gex, zero_gamma};

use crate::bsm::{self, BsmInput};
use crate::error::{GexError, Result};
use crate::levels::{Walls, walls};
use crate::types::{Chain, OptionContract, OptionKind, strike_groups};

/// One percent, the move size GEX and vanna exposure are quoted for.
const PER_PCT: f64 = 0.01;
/// Calendar days per year, for per-day charm.
const DAYS_PER_YEAR: f64 = 365.0;

/// Spot grid swept to find the zero-gamma level, as multiples of spot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpotGrid {
    /// Lowest grid spot as a multiple of spot.
    pub lower: f64,
    /// Highest grid spot as a multiple of spot.
    pub upper: f64,
    /// Number of grid points, including both ends.
    pub points: usize,
}

impl Default for SpotGrid {
    /// ±10% of spot in 201 points.
    fn default() -> Self {
        Self {
            lower: 0.9,
            upper: 1.1,
            points: 201,
        }
    }
}

/// Settings for the exposure engine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExposureConfig {
    /// Dealer position per call contract (+1: dealers long calls).
    pub call_sign: f64,
    /// Dealer position per put contract (−1: dealers short puts).
    pub put_sign: f64,
    /// Floor on time to expiry, in years, so ATM gamma stays finite at expiry.
    pub min_time: f64,
    /// Grid for the zero-gamma sweep.
    pub flip_grid: SpotGrid,
    /// `|total GEX|` at or below this is reported as [`Regime::Neutral`].
    pub neutral_band: f64,
}

impl Default for ExposureConfig {
    /// Calls +1, puts −1, a one-minute time floor, the default grid and no
    /// neutral band.
    fn default() -> Self {
        Self {
            call_sign: 1.0,
            put_sign: -1.0,
            min_time: 1.0 / (DAYS_PER_YEAR * 1440.0),
            flip_grid: SpotGrid::default(),
            neutral_band: 0.0,
        }
    }
}

impl ExposureConfig {
    /// Checks every field.
    ///
    /// # Errors
    ///
    /// Returns [`GexError::InvalidConfig`] naming the first bad field.
    pub fn validate(&self) -> Result<()> {
        let bad = |msg| Err(GexError::InvalidConfig(msg));
        if !(self.call_sign.is_finite() && self.put_sign.is_finite()) {
            return bad("dealer signs must be finite");
        }
        if !(self.min_time.is_finite() && self.min_time >= 0.0) {
            return bad("min_time must be finite and non-negative");
        }
        if !(self.neutral_band.is_finite() && self.neutral_band >= 0.0) {
            return bad("neutral_band must be finite and non-negative");
        }
        let g = self.flip_grid;
        if !(g.lower.is_finite() && g.upper.is_finite() && 0.0 < g.lower && g.lower < g.upper) {
            return bad("flip grid needs 0 < lower < upper");
        }
        if g.points < 2 {
            return bad("flip grid needs at least 2 points");
        }
        Ok(())
    }

    fn sign(&self, kind: OptionKind) -> f64 {
        match kind {
            OptionKind::Call => self.call_sign,
            OptionKind::Put => self.put_sign,
        }
    }
}

/// Exposure summed over every contract at one strike, across expiries.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StrikeExposure {
    /// Strike price.
    pub strike: f64,
    /// GEX from calls.
    pub call_gex: f64,
    /// GEX from puts.
    pub put_gex: f64,
    /// `call_gex + put_gex`.
    pub gex: f64,
    /// Dollar delta exposure.
    pub dex: f64,
    /// Vanna exposure.
    pub vanna: f64,
    /// Charm exposure.
    pub charm: f64,
    /// Call open interest, including contracts skipped for missing IV.
    pub call_oi: u64,
    /// Put open interest, including contracts skipped for missing IV.
    pub put_oi: u64,
}

/// Overall dealer gamma positioning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Regime {
    /// Dealers net long gamma: hedging dampens moves.
    PositiveGamma,
    /// Dealers net short gamma: hedging amplifies moves.
    NegativeGamma,
    /// `|total GEX|` within the neutral band.
    Neutral,
}

/// Result of [`analyze`].
#[derive(Debug, Clone, PartialEq)]
pub struct ExposureReport {
    /// Spot the report was computed at.
    pub spot: f64,
    /// Per-strike exposure in ascending strike order.
    pub per_strike: Vec<StrikeExposure>,
    /// Total GEX.
    pub total_gex: f64,
    /// Total DEX.
    pub total_dex: f64,
    /// Total vanna exposure.
    pub total_vanna: f64,
    /// Total charm exposure.
    pub total_charm: f64,
    /// Sign of total GEX, with the neutral band applied.
    pub regime: Regime,
    /// Zero-gamma level, if total GEX changes sign on the grid.
    pub zero_gamma: Option<ZeroGamma>,
    /// Call and put GEX walls.
    pub walls: Walls,
    /// Contracts left out because `iv <= 0`.
    pub skipped: usize,
}

/// Computes per-strike and total exposure at the chain's spot.
///
/// # Errors
///
/// Returns [`GexError::InvalidConfig`] for a bad config,
/// [`GexError::EmptyChain`] if the chain has no contracts, or
/// [`GexError::NoUsableContracts`] if every contract has `iv <= 0`.
pub fn analyze(chain: &Chain, cfg: &ExposureConfig) -> Result<ExposureReport> {
    cfg.validate()?;
    let prep = Prepared::new(chain);
    prep.require_usable()?;
    let per_strike = per_strike_prepared(chain, &prep, cfg);
    let sum = |f: fn(&StrikeExposure) -> f64| per_strike.iter().map(f).sum::<f64>();
    let total_gex = sum(|s| s.gex);
    let regime = if total_gex.abs() <= cfg.neutral_band {
        Regime::Neutral
    } else if total_gex > 0.0 {
        Regime::PositiveGamma
    } else {
        Regime::NegativeGamma
    };
    Ok(ExposureReport {
        spot: chain.spot(),
        total_gex,
        total_dex: sum(|s| s.dex),
        total_vanna: sum(|s| s.vanna),
        total_charm: sum(|s| s.charm),
        walls: walls(&per_strike, chain.spot()),
        per_strike,
        regime,
        zero_gamma: flip::zero_gamma_prepared(chain, &prep, cfg),
        skipped: prep.skipped,
    })
}

/// Per-strike exposure at the chain's spot, in ascending strike order. Does
/// not validate `cfg`; [`analyze`] does.
#[must_use]
pub fn per_strike(chain: &Chain, cfg: &ExposureConfig) -> Vec<StrikeExposure> {
    per_strike_prepared(chain, &Prepared::new(chain), cfg)
}

/// Total GEX if the underlying were at `spot`, holding each contract's IV.
/// Does not validate `cfg`.
#[must_use]
pub fn total_gex_at(chain: &Chain, spot: f64, cfg: &ExposureConfig) -> f64 {
    Prepared::new(chain).total_gex_at(chain, spot, cfg)
}

/// Contracts sorted into a canonical order, so sums do not depend on input
/// order, with unusable (`iv <= 0`) contracts set aside.
pub(crate) struct Prepared<'a> {
    pub(crate) usable: Vec<&'a OptionContract>,
    /// All contracts in canonical order, for open interest totals.
    all: Vec<&'a OptionContract>,
    skipped: usize,
}

impl<'a> Prepared<'a> {
    /// [`GexError::EmptyChain`] when there are no contracts, and
    /// [`GexError::NoUsableContracts`] when every one has `iv <= 0`.
    pub(crate) fn require_usable(&self) -> Result<()> {
        if self.all.is_empty() {
            Err(GexError::EmptyChain)
        } else if self.usable.is_empty() {
            Err(GexError::NoUsableContracts {
                skipped: self.skipped,
            })
        } else {
            Ok(())
        }
    }

    pub(crate) fn new(chain: &'a Chain) -> Self {
        let mut all: Vec<&OptionContract> = chain.contracts().iter().collect();
        all.sort_by(|a, b| OptionContract::canonical_cmp(a, b));
        let usable: Vec<_> = all.iter().copied().filter(|c| is_usable(c)).collect();
        Self {
            skipped: all.len() - usable.len(),
            usable,
            all,
        }
    }

    pub(crate) fn total_gex_at(&self, chain: &Chain, spot: f64, cfg: &ExposureConfig) -> f64 {
        self.usable
            .iter()
            .map(|c| {
                weight(chain, c, cfg)
                    * bsm::gamma(&input(chain, c, spot, cfg))
                    * spot
                    * spot
                    * PER_PCT
            })
            .sum()
    }
}

fn per_strike_prepared(
    chain: &Chain,
    prep: &Prepared<'_>,
    cfg: &ExposureConfig,
) -> Vec<StrikeExposure> {
    // `all` is in canonical order (strike first), so each strike group's
    // contracts are adjacent.
    strike_groups(&prep.all)
        .into_iter()
        .filter_map(|group| strike_row(chain, group, cfg))
        .collect()
}

/// Sums one strike group into a row labelled with the group's smallest
/// strike; `None` only for an empty group, which `strike_groups` never makes.
fn strike_row(
    chain: &Chain,
    group: &[&OptionContract],
    cfg: &ExposureConfig,
) -> Option<StrikeExposure> {
    let spot = chain.spot();
    let mut row = StrikeExposure {
        strike: group.first()?.strike(),
        ..StrikeExposure::default()
    };
    for c in group {
        match c.kind() {
            OptionKind::Call => row.call_oi += c.open_interest(),
            OptionKind::Put => row.put_oi += c.open_interest(),
        }
        if !is_usable(c) {
            continue;
        }
        let w = weight(chain, c, cfg);
        let g = bsm::greeks(&input(chain, c, spot, cfg));
        let gex = w * g.gamma * spot * spot * PER_PCT;
        match c.kind() {
            OptionKind::Call => row.call_gex += gex,
            OptionKind::Put => row.put_gex += gex,
        }
        row.gex += gex;
        row.dex += w * g.delta * spot;
        row.vanna += w * g.vanna * spot * PER_PCT;
        row.charm += w * g.charm * spot / DAYS_PER_YEAR;
    }
    Some(row)
}

/// Contracts the engine can price: those with a positive IV.
fn is_usable(c: &OptionContract) -> bool {
    c.iv() > 0.0
}

/// `sign · OI · multiplier`.
#[allow(clippy::cast_precision_loss)] // open interest is far below 2^53
pub(crate) fn weight(chain: &Chain, c: &OptionContract, cfg: &ExposureConfig) -> f64 {
    cfg.sign(c.kind()) * c.open_interest() as f64 * chain.multiplier()
}

pub(crate) fn input(
    chain: &Chain,
    c: &OptionContract,
    spot: f64,
    cfg: &ExposureConfig,
) -> BsmInput {
    BsmInput {
        kind: c.kind(),
        spot,
        strike: c.strike(),
        t: c.expiry().max(cfg.min_time),
        rate: chain.rate(),
        div_yield: chain.div_yield(),
        sigma: c.iv(),
    }
}

#[cfg(test)]
// These tests compare exact values on purpose: identical code paths, exact
// limits such as 0 and 1, and values passed through unchanged.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn contract(kind: OptionKind, strike: f64, expiry: f64, iv: f64, oi: u64) -> OptionContract {
        OptionContract::new(kind, strike, expiry, iv, oi).unwrap()
    }

    fn rel_eq(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-12 * b.abs().max(1e-300)
    }

    #[test]
    fn single_call_matches_hand_formula() {
        let c = contract(OptionKind::Call, 100.0, 0.25, 0.2, 1_000);
        let chain = Chain::new(100.0, 0.03, 0.01, vec![c.clone()]).unwrap();
        let cfg = ExposureConfig::default();
        let r = analyze(&chain, &cfg).unwrap();
        let g = bsm::greeks(&input(&chain, &c, 100.0, &cfg));
        let w = 1_000.0 * 100.0;
        assert!(rel_eq(r.total_gex, w * g.gamma * 100.0 * 100.0 * 0.01));
        assert!(rel_eq(r.total_dex, w * g.delta * 100.0));
        assert!(rel_eq(r.total_vanna, w * g.vanna * 100.0 * 0.01));
        assert!(rel_eq(r.total_charm, w * g.charm * 100.0 / 365.0));
        assert_eq!(r.regime, Regime::PositiveGamma);
        assert_eq!(r.per_strike.len(), 1);
        assert_eq!(r.per_strike[0].call_oi, 1_000);
    }

    #[test]
    fn puts_count_negative_gamma_under_default_signs() {
        let chain = Chain::new(
            100.0,
            0.0,
            0.0,
            vec![contract(OptionKind::Put, 100.0, 0.1, 0.3, 10)],
        )
        .unwrap();
        let r = analyze(&chain, &ExposureConfig::default()).unwrap();
        assert!(r.total_gex < 0.0);
        assert!(r.total_dex > 0.0, "dealer short puts is long delta");
        assert_eq!(r.regime, Regime::NegativeGamma);
        assert!(rel_eq(r.per_strike[0].put_gex, r.total_gex));
        assert_eq!(r.per_strike[0].call_gex, 0.0);
    }

    #[test]
    fn strikes_merge_across_expiries_and_sort() {
        let chain = Chain::new(
            100.0,
            0.0,
            0.0,
            vec![
                contract(OptionKind::Call, 105.0, 0.1, 0.2, 1),
                contract(OptionKind::Put, 95.0, 0.1, 0.2, 2),
                contract(OptionKind::Call, 105.0, 0.5, 0.2, 3),
                contract(OptionKind::Put, 105.0, 0.5, 0.0, 4),
            ],
        )
        .unwrap();
        let rows = per_strike(&chain, &ExposureConfig::default());
        let strikes: Vec<f64> = rows.iter().map(|r| r.strike).collect();
        assert_eq!(strikes, vec![95.0, 105.0]);
        assert_eq!((rows[1].call_oi, rows[1].put_oi), (4, 4));
        assert_eq!(rows[1].put_gex, 0.0, "iv = 0 put is skipped");
        assert_eq!(
            analyze(&chain, &ExposureConfig::default()).unwrap().skipped,
            1
        );
    }

    #[test]
    fn neutral_band_and_unusable_chains() {
        let chain = Chain::new(
            100.0,
            0.0,
            0.0,
            vec![contract(OptionKind::Call, 100.0, 0.1, 0.2, 1)],
        )
        .unwrap();
        let cfg = ExposureConfig {
            neutral_band: 1e12,
            ..ExposureConfig::default()
        };
        assert_eq!(analyze(&chain, &cfg).unwrap().regime, Regime::Neutral);

        let no_iv = Chain::new(
            100.0,
            0.0,
            0.0,
            vec![contract(OptionKind::Call, 100.0, 0.1, 0.0, 1)],
        )
        .unwrap();
        assert_eq!(
            analyze(&no_iv, &ExposureConfig::default()),
            Err(GexError::NoUsableContracts { skipped: 1 })
        );
        assert_eq!(
            zero_gamma(&no_iv, &ExposureConfig::default()),
            Err(GexError::NoUsableContracts { skipped: 1 })
        );
        let empty = Chain::new(100.0, 0.0, 0.0, vec![]).unwrap();
        assert_eq!(
            analyze(&empty, &ExposureConfig::default()),
            Err(GexError::EmptyChain)
        );
        assert_eq!(
            zero_gamma(&empty, &ExposureConfig::default()),
            Err(GexError::EmptyChain)
        );
    }

    #[test]
    fn min_time_floors_expired_contracts() {
        let chain = Chain::new(
            100.0,
            0.0,
            0.0,
            vec![contract(OptionKind::Call, 100.0, 0.0, 0.2, 1)],
        )
        .unwrap();
        let r = analyze(&chain, &ExposureConfig::default()).unwrap();
        assert!(r.total_gex > 0.0 && r.total_gex.is_finite());
        let no_floor = ExposureConfig {
            min_time: 0.0,
            ..ExposureConfig::default()
        };
        assert_eq!(analyze(&chain, &no_floor).unwrap().total_gex, 0.0);
    }

    #[test]
    fn total_gex_at_spot_matches_report() {
        let chain = Chain::new(
            100.0,
            0.01,
            0.0,
            vec![
                contract(OptionKind::Call, 102.0, 0.1, 0.2, 7),
                contract(OptionKind::Put, 97.0, 0.1, 0.25, 9),
            ],
        )
        .unwrap();
        let cfg = ExposureConfig::default();
        let r = analyze(&chain, &cfg).unwrap();
        assert!(rel_eq(total_gex_at(&chain, 100.0, &cfg), r.total_gex));
    }

    /// A chain with both signs of GEX and a flip, for the config tests.
    fn mixed_chain() -> Chain {
        Chain::new(
            100.0,
            0.02,
            0.01,
            vec![
                contract(OptionKind::Put, 95.0, 0.05, 0.25, 4_000),
                contract(OptionKind::Call, 100.0, 0.05, 0.2, 1_500),
                contract(OptionKind::Put, 100.0, 0.05, 0.21, 1_000),
                contract(OptionKind::Call, 106.0, 0.05, 0.18, 3_000),
            ],
        )
        .unwrap()
    }

    fn totals(r: &ExposureReport) -> [f64; 4] {
        [r.total_gex, r.total_dex, r.total_vanna, r.total_charm]
    }

    #[test]
    fn multiplier_scales_every_exposure() {
        let cfg = ExposureConfig::default();
        let base = analyze(&mixed_chain(), &cfg).unwrap();
        let tenth = analyze(&mixed_chain().with_multiplier(10.0).unwrap(), &cfg).unwrap();
        for (a, b) in totals(&base).into_iter().zip(totals(&tenth)) {
            assert!(rel_eq(b * 10.0, a), "{b} * 10 vs {a}");
        }
        for (a, b) in base.per_strike.iter().zip(&tenth.per_strike) {
            assert!(
                rel_eq(b.gex * 10.0, a.gex),
                "{} {} vs {}",
                a.strike,
                b.gex,
                a.gex
            );
        }
        // Levels are scale-free.
        assert_eq!(tenth.walls, base.walls);
        let (z0, z1) = (base.zero_gamma.unwrap(), tenth.zero_gamma.unwrap());
        assert!((z0.level - z1.level).abs() < 1e-8, "{z0:?} vs {z1:?}");
    }

    #[test]
    fn dealer_signs_are_applied_per_kind() {
        let chain = mixed_chain();
        let base = analyze(&chain, &ExposureConfig::default()).unwrap();
        // Dealers short calls and long puts: every exposure flips sign exactly.
        let swapped_cfg = ExposureConfig {
            call_sign: -1.0,
            put_sign: 1.0,
            ..ExposureConfig::default()
        };
        let swapped = analyze(&chain, &swapped_cfg).unwrap();
        assert_eq!(totals(&swapped), totals(&base).map(|x| -x));
        assert_ne!(swapped.regime, base.regime);
        for (a, b) in base.per_strike.iter().zip(&swapped.per_strike) {
            assert_eq!(
                (b.call_gex, b.put_gex),
                (-a.call_gex, -a.put_gex),
                "strike {}",
                a.strike
            );
        }
        // Long both: GEX is the sum of the magnitudes of each side.
        let long_cfg = ExposureConfig {
            call_sign: 1.0,
            put_sign: 1.0,
            ..ExposureConfig::default()
        };
        let long = analyze(&chain, &long_cfg).unwrap();
        let magnitudes: f64 = base
            .per_strike
            .iter()
            .map(|s| s.call_gex.abs() + s.put_gex.abs())
            .sum();
        assert!(rel_eq(long.total_gex, magnitudes));
        assert_eq!(long.zero_gamma, None, "all-positive GEX has no flip");
    }

    #[test]
    fn neutral_band_is_inclusive() {
        let chain = mixed_chain();
        let gex = analyze(&chain, &ExposureConfig::default())
            .unwrap()
            .total_gex;
        let at = |band: f64| {
            analyze(
                &chain,
                &ExposureConfig {
                    neutral_band: band,
                    ..ExposureConfig::default()
                },
            )
            .unwrap()
            .regime
        };
        assert_eq!(at(gex.abs()), Regime::Neutral, "|GEX| == band is neutral");
        assert_ne!(at(gex.abs() * (1.0 - 1e-12)), Regime::Neutral);
    }

    #[test]
    fn near_identical_strikes_share_a_row_level_and_straddle() {
        // 450.000000000001 differs from 450.0 by about 2e-15 relative: one
        // strike. 450.001 differs by about 2e-6: a separate strike.
        let near = 450.000_000_000_001;
        let chain = Chain::new(
            450.2,
            0.0,
            0.0,
            vec![
                contract(OptionKind::Call, 450.0, 0.1, 0.2, 10),
                contract(OptionKind::Put, near, 0.1, 0.2, 20),
                contract(OptionKind::Call, 450.001, 0.1, 0.2, 5),
            ],
        )
        .unwrap();
        let rows = per_strike(&chain, &ExposureConfig::default());
        let strikes: Vec<f64> = rows.iter().map(|r| r.strike).collect();
        assert_eq!(strikes, vec![450.0, 450.001]);
        assert_eq!((rows[0].call_oi, rows[0].put_oi), (10, 20));
        // The straddle pairs the 450.0 call with the 450.000000000001 put.
        let em = crate::levels::expected_move(&chain, 0.1).unwrap();
        assert_eq!(em.strike, 450.0);
        let mp = crate::levels::max_pain(&chain, 0.1).unwrap();
        assert!(mp == 450.0 || mp == 450.001, "{mp}");
    }

    #[test]
    fn config_validation() {
        let base = ExposureConfig::default();
        assert!(base.validate().is_ok());
        let cases = [
            ExposureConfig {
                call_sign: f64::NAN,
                ..base
            },
            ExposureConfig {
                min_time: -1.0,
                ..base
            },
            ExposureConfig {
                neutral_band: -1.0,
                ..base
            },
            ExposureConfig {
                flip_grid: SpotGrid {
                    lower: 1.1,
                    upper: 0.9,
                    points: 10,
                },
                ..base
            },
            ExposureConfig {
                flip_grid: SpotGrid {
                    lower: 0.0,
                    upper: 1.1,
                    points: 10,
                },
                ..base
            },
            ExposureConfig {
                flip_grid: SpotGrid {
                    points: 1,
                    ..base.flip_grid
                },
                ..base
            },
        ];
        for cfg in cases {
            assert!(
                matches!(cfg.validate(), Err(GexError::InvalidConfig(_))),
                "{cfg:?}"
            );
        }
    }
}
