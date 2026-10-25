//! Zero-gamma (flip) level: the spot at which total GEX changes sign.
//!
//! Total GEX is evaluated on [`SpotGrid`](super::SpotGrid) with each
//! contract's IV held fixed (sticky strike). Values with
//! `|GEX| <= negligible_gex` count as zero: near expiry, gamma underflows to
//! exactly 0 a few percent from every strike, and far-wing tails can change
//! sign at magnitudes like 1e-30 dollars. A crossing is a sign change between
//! consecutive non-negligible values:
//!
//! - between neighbouring grid points, it is refined by bisection;
//! - across a negligible gap, both edges of the gap are refined by bisection
//!   and the crossing is reported at their midpoint.
//!
//! If every value is negligible, or the non-negligible values share one
//! sign, there is no crossing. Two crossings inside one grid cell cancel out
//! and are not seen; a finer grid finds them.

use super::{ExposureConfig, Prepared, input, weight};
use crate::bsm;
use crate::error::{GexError, Result};
use crate::types::Chain;

/// Bisection stops once the bracket is narrower than this fraction of spot.
const BISECT_TOL: f64 = 1e-10;
/// Upper bound on bisection steps; the tolerance is normally reached in ~30.
const BISECT_MAX_STEPS: usize = 200;

/// `|GEX|` at or below this fraction of the chain's peak dollar gamma counts
/// as zero in the flip search. See [`negligible_gex`].
pub const NEGLIGIBLE_GEX_FRACTION: f64 = 1e-9;

/// Where total GEX crosses zero.
#[derive(Debug, Clone, PartialEq)]
pub struct ZeroGamma {
    /// The crossing nearest spot (the lower one on a tie).
    pub level: f64,
    /// Every crossing found on the grid, ascending.
    pub crossings: Vec<f64>,
}

/// Finds the zero-gamma level, or `None` if total GEX keeps one sign over the
/// whole grid (ignoring negligible values) or is negligible everywhere.
///
/// # Errors
///
/// Returns [`GexError::InvalidConfig`] for a bad config, or
/// [`GexError::EmptyChain`] if no contract has a positive IV.
pub fn zero_gamma(chain: &Chain, cfg: &ExposureConfig) -> Result<Option<ZeroGamma>> {
    cfg.validate()?;
    let prep = Prepared::new(chain);
    if prep.usable.is_empty() {
        return Err(GexError::EmptyChain);
    }
    Ok(zero_gamma_prepared(chain, &prep, cfg))
}

/// The magnitude below which total GEX counts as zero in the flip search:
/// [`NEGLIGIBLE_GEX_FRACTION`] times the sum over contracts with `iv > 0` of
/// each one's peak dollar gamma, `|sign · OI · multiplier| · Γ · K² · 0.01`
/// with spot at its own strike. It does not depend on the grid, so any
/// sweep of the same chain uses the same threshold. Does not validate `cfg`.
#[must_use]
pub fn negligible_gex(chain: &Chain, cfg: &ExposureConfig) -> f64 {
    negligible_prepared(chain, &Prepared::new(chain), cfg)
}

fn negligible_prepared(chain: &Chain, prep: &Prepared<'_>, cfg: &ExposureConfig) -> f64 {
    let peak: f64 = prep
        .usable
        .iter()
        .map(|c| {
            let k = c.strike();
            weight(chain, c, cfg).abs() * bsm::gamma(&input(chain, c, k, cfg)) * k * k * 0.01
        })
        .sum();
    NEGLIGIBLE_GEX_FRACTION * peak
}

pub(crate) fn zero_gamma_prepared(
    chain: &Chain,
    prep: &Prepared<'_>,
    cfg: &ExposureConfig,
) -> Option<ZeroGamma> {
    let spot = chain.spot();
    let threshold = negligible_prepared(chain, prep, cfg);
    if threshold <= 0.0 {
        // No open interest (or no gamma) anywhere: nothing can cross.
        return None;
    }
    let search = Search {
        f: |s: f64| prep.total_gex_at(chain, s, cfg),
        threshold,
        tol: BISECT_TOL * spot,
    };
    let grid = grid(spot, cfg);
    // (grid index, is negative) for every non-negligible grid value.
    let significant: Vec<(usize, bool)> = grid
        .iter()
        .enumerate()
        .filter_map(|(j, &s)| {
            let v = (search.f)(s);
            (v.abs() > threshold).then_some((j, v < 0.0))
        })
        .collect();

    let mut crossings = Vec::new();
    for pair in significant.windows(2) {
        let ((i, i_neg), (k, k_neg)) = (pair[0], pair[1]);
        if i_neg == k_neg {
            continue;
        }
        crossings.push(if k == i + 1 {
            search.crossing(grid[i], grid[k], i_neg)
        } else {
            search.gap_middle(grid[i], grid[i + 1], grid[k - 1], grid[k])
        });
    }
    let level = crossings.iter().copied().min_by(|a, b| {
        (a - spot)
            .abs()
            .total_cmp(&(b - spot).abs())
            .then(a.total_cmp(b))
    })?;
    Some(ZeroGamma { level, crossings })
}

/// Grid spots from `lower · spot` to `upper · spot`, both ends included.
#[allow(clippy::cast_precision_loss)] // grid sizes are far below 2^53
fn grid(spot: f64, cfg: &ExposureConfig) -> Vec<f64> {
    let g = cfg.flip_grid;
    let last = (g.points - 1) as f64;
    (0..g.points)
        .map(|j| spot * (g.lower + (g.upper - g.lower) * (j as f64) / last))
        .collect()
}

/// Total GEX as a function of spot, with the negligible threshold and the
/// bisection tolerance.
struct Search<F: Fn(f64) -> f64> {
    f: F,
    threshold: f64,
    tol: f64,
}

impl<F: Fn(f64) -> f64> Search<F> {
    fn negligible(&self, s: f64) -> bool {
        (self.f)(s).abs() <= self.threshold
    }

    /// Crossing in `[lo, hi]`, whose ends are non-negligible with opposite
    /// signs (`lo_neg` is the sign at `lo`). If bisection lands in a
    /// negligible gap, the gap's middle is the crossing.
    fn crossing(&self, mut lo: f64, mut hi: f64, lo_neg: bool) -> f64 {
        for _ in 0..BISECT_MAX_STEPS {
            if hi - lo <= self.tol {
                break;
            }
            let mid = f64::midpoint(lo, hi);
            let v = (self.f)(mid);
            if v.abs() <= self.threshold {
                return self.gap_middle(lo, mid, mid, hi);
            }
            if (v < 0.0) == lo_neg {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        f64::midpoint(lo, hi)
    }

    /// Middle of a negligible gap whose left edge lies in `[a0, a1]`
    /// (`a0` non-negligible, `a1` negligible) and right edge in `[b0, b1]`
    /// (`b0` negligible, `b1` non-negligible).
    fn gap_middle(&self, a0: f64, a1: f64, b0: f64, b1: f64) -> f64 {
        f64::midpoint(self.edge(a0, a1, true), self.edge(b0, b1, false))
    }

    /// Boundary between negligible and non-negligible values in `[lo, hi]`;
    /// `lo_significant` says which side `lo` is on.
    fn edge(&self, mut lo: f64, mut hi: f64, lo_significant: bool) -> f64 {
        for _ in 0..BISECT_MAX_STEPS {
            if hi - lo <= self.tol {
                break;
            }
            let mid = f64::midpoint(lo, hi);
            // `mid` is on `lo`'s side when it is significant iff `lo` is.
            if self.negligible(mid) == lo_significant {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        f64::midpoint(lo, hi)
    }
}

#[cfg(test)]
// These tests compare exact values on purpose: identical code paths, exact
// limits such as 0 and 1, and values passed through unchanged.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::exposure::{SpotGrid, total_gex_at};
    use crate::types::{OptionContract, OptionKind};

    fn chain(contracts: &[(OptionKind, f64, u64)]) -> Chain {
        let cs = contracts
            .iter()
            .map(|&(k, strike, oi)| OptionContract::new(k, strike, 0.05, 0.2, oi).unwrap())
            .collect();
        Chain::new(100.0, 0.0, 0.0, cs).unwrap()
    }

    #[test]
    fn finds_flip_between_put_and_call_strikes() {
        let ch = chain(&[
            (OptionKind::Put, 95.0, 1_000),
            (OptionKind::Call, 105.0, 1_000),
        ]);
        let cfg = ExposureConfig::default();
        let z = zero_gamma(&ch, &cfg).unwrap().expect("crossing");
        assert!(95.0 < z.level && z.level < 105.0, "{z:?}");
        let scale = total_gex_at(&ch, 105.0, &cfg).abs();
        assert!(total_gex_at(&ch, z.level, &cfg).abs() < 1e-6 * scale);
        let h = 1e-3;
        assert!(total_gex_at(&ch, z.level - h, &cfg) < 0.0);
        assert!(total_gex_at(&ch, z.level + h, &cfg) > 0.0);
    }

    #[test]
    fn no_crossing_when_one_signed() {
        let ch = chain(&[(OptionKind::Call, 100.0, 10), (OptionKind::Call, 110.0, 10)]);
        assert_eq!(zero_gamma(&ch, &ExposureConfig::default()).unwrap(), None);
    }

    #[test]
    fn reports_every_crossing_and_nearest_level() {
        // Put wall at 92, calls at 100, puts again at 108: two crossings.
        let ch = chain(&[
            (OptionKind::Put, 92.0, 1_000),
            (OptionKind::Call, 100.0, 1_000),
            (OptionKind::Put, 108.0, 1_000),
        ]);
        let cfg = ExposureConfig {
            flip_grid: SpotGrid {
                lower: 0.85,
                upper: 1.15,
                points: 301,
            },
            ..ExposureConfig::default()
        };
        let z = zero_gamma(&ch, &cfg).unwrap().expect("crossings");
        assert_eq!(z.crossings.len(), 2, "{z:?}");
        assert!(z.crossings[0] < 100.0 && z.crossings[1] > 100.0);
        assert!(z.crossings.contains(&z.level));
    }

    /// Chain expiring in `minutes`: gamma underflows to exactly 0 a few
    /// percent from each strike, which used to produce fake crossings.
    fn expiring_in(minutes: f64, contracts: &[(OptionKind, f64, u64)]) -> Chain {
        let t = minutes / (60.0 * 24.0 * 365.0);
        let cs = contracts
            .iter()
            .map(|&(k, strike, oi)| OptionContract::new(k, strike, t, 0.2, oi).unwrap())
            .collect();
        Chain::new(100.0, 0.0, 0.0, cs).unwrap()
    }

    #[test]
    fn one_sided_zero_dte_chains_have_no_flip() {
        let cfg = ExposureConfig::default();
        for kind in [OptionKind::Call, OptionKind::Put] {
            let ch = expiring_in(60.0, &[(kind, 100.0, 1_000)]);
            assert_eq!(zero_gamma(&ch, &cfg).unwrap(), None, "{kind:?}-only");
        }
    }

    #[test]
    fn zero_open_interest_and_far_wings_have_no_flip() {
        let cfg = ExposureConfig::default();
        let no_oi = chain(&[(OptionKind::Call, 100.0, 0), (OptionKind::Put, 100.0, 0)]);
        assert_eq!(zero_gamma(&no_oi, &cfg).unwrap(), None);
        // Strikes far outside the ±10% grid contribute nothing on it.
        let wings = expiring_in(
            60.0,
            &[
                (OptionKind::Put, 50.0, 1_000),
                (OptionKind::Call, 200.0, 1_000),
            ],
        );
        assert_eq!(zero_gamma(&wings, &cfg).unwrap(), None);
    }

    #[test]
    fn dead_zone_between_put_and_call_gives_one_crossing_in_its_middle() {
        // Ten minutes out, GEX is negative around 95, positive around 105 and
        // exactly 0 in between: one crossing, in the middle of the dead zone.
        let ch = expiring_in(
            10.0,
            &[
                (OptionKind::Put, 95.0, 1_000),
                (OptionKind::Call, 105.0, 1_000),
            ],
        );
        let cfg = ExposureConfig::default();
        assert_eq!(
            total_gex_at(&ch, 100.0, &cfg),
            0.0,
            "test needs a real dead zone"
        );
        let z = zero_gamma(&ch, &cfg).unwrap().expect("crossing");
        assert_eq!(z.crossings.len(), 1, "{z:?}");
        // The zone's edges sit symmetrically in log-moneyness about 100.
        assert!((z.level - 100.0).abs() < 0.1, "{z:?}");
    }

    #[test]
    fn far_wing_noise_is_not_a_crossing() {
        // Big put at 97 and call at 103, plus one put contract at 115, just
        // off the grid. Near 110 that put's gamma tail (~1e-89 dollars)
        // outweighs the call's even smaller tail, so GEX turns negative
        // again: a genuine sign change, but far below the threshold.
        let ch = expiring_in(
            60.0,
            &[
                (OptionKind::Put, 97.0, 1_000_000),
                (OptionKind::Call, 103.0, 1_000_000),
                (OptionKind::Put, 115.0, 1),
            ],
        );
        let cfg = ExposureConfig::default();
        let top = total_gex_at(&ch, 110.0, &cfg);
        assert!(
            top < 0.0 && top.abs() < negligible_gex(&ch, &cfg),
            "test needs wing noise: {top}"
        );
        let z = zero_gamma(&ch, &cfg).unwrap().expect("crossing");
        assert_eq!(z.crossings.len(), 1, "{z:?}");
        assert!(97.0 < z.level && z.level < 103.0, "{z:?}");
    }

    #[test]
    fn grid_includes_both_ends() {
        let g = grid(200.0, &ExposureConfig::default());
        assert_eq!(g.len(), 201);
        assert!((g[0] - 180.0).abs() < 1e-12 && (g[200] - 220.0).abs() < 1e-12);
    }
}
