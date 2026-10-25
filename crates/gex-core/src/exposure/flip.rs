//! Zero-gamma (flip) level: the spot at which total GEX changes sign.
//!
//! Total GEX is evaluated on [`SpotGrid`](super::SpotGrid) with each
//! contract's IV held fixed (sticky strike). Every sign change between
//! neighbouring grid points is refined by bisection. Two crossings inside one
//! grid cell cancel out and are not seen; a finer grid finds them.

use super::{ExposureConfig, Prepared};
use crate::error::{GexError, Result};
use crate::types::Chain;

/// Bisection stops once the bracket is narrower than this fraction of spot.
const BISECT_TOL: f64 = 1e-10;
/// Upper bound on bisection steps; the tolerance is normally reached in ~30.
const BISECT_MAX_STEPS: usize = 200;

/// Where total GEX crosses zero.
#[derive(Debug, Clone, PartialEq)]
pub struct ZeroGamma {
    /// The crossing nearest spot (the lower one on a tie).
    pub level: f64,
    /// Every crossing found on the grid, ascending.
    pub crossings: Vec<f64>,
}

/// Finds the zero-gamma level, or `None` if total GEX keeps one sign over the
/// whole grid.
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

pub(crate) fn zero_gamma_prepared(
    chain: &Chain,
    prep: &Prepared<'_>,
    cfg: &ExposureConfig,
) -> Option<ZeroGamma> {
    let spot = chain.spot();
    let f = |s: f64| prep.total_gex_at(chain, s, cfg);
    let grid = grid(spot, cfg);
    let values: Vec<f64> = grid.iter().map(|&s| f(s)).collect();

    let mut crossings = Vec::new();
    for j in 0..grid.len() {
        if values[j] == 0.0 {
            crossings.push(grid[j]);
        } else if j + 1 < grid.len() && values[j] * values[j + 1] < 0.0 {
            crossings.push(bisect(f, grid[j], grid[j + 1], values[j], spot));
        }
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

/// Root of `f` in `[lo, hi]`, given `f(lo) = f_lo` and a sign change.
fn bisect(f: impl Fn(f64) -> f64, mut lo: f64, mut hi: f64, mut f_lo: f64, spot: f64) -> f64 {
    for _ in 0..BISECT_MAX_STEPS {
        if hi - lo <= BISECT_TOL * spot {
            break;
        }
        let mid = f64::midpoint(lo, hi);
        let f_mid = f(mid);
        if f_mid == 0.0 {
            return mid;
        }
        if (f_mid < 0.0) == (f_lo < 0.0) {
            lo = mid;
            f_lo = f_mid;
        } else {
            hi = mid;
        }
    }
    f64::midpoint(lo, hi)
}

#[cfg(test)]
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

    #[test]
    fn grid_includes_both_ends() {
        let g = grid(200.0, &ExposureConfig::default());
        assert_eq!(g.len(), 201);
        assert!((g[0] - 180.0).abs() < 1e-12 && (g[200] - 220.0).abs() < 1e-12);
    }
}
