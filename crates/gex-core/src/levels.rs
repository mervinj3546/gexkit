//! Price levels derived from a chain: GEX walls, max pain, expected move.
//!
//! Ties are broken by distance to spot, then by the lower strike. Max pain
//! values within [`TIE_TOLERANCE`] (relative) count as ties so the choice
//! does not hinge on summation rounding.

use crate::bsm::{self, BsmInput};
use crate::error::{GexError, Result};
use crate::exposure::StrikeExposure;
use crate::types::{Chain, OptionContract, OptionKind};

/// Relative difference below which two max pain values are treated as equal.
pub const TIE_TOLERANCE: f64 = 1e-12;

/// Strikes with the largest call and put GEX.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Walls {
    /// Strike with the largest `|call_gex|`, if any call contributes.
    pub call_wall: Option<f64>,
    /// Strike with the largest `|put_gex|`, if any put contributes.
    pub put_wall: Option<f64>,
}

/// Where a straddle leg's price came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PriceSource {
    /// Mid of the quoted bid and ask.
    Quote,
    /// Black-Scholes-Merton price from the contract's IV.
    Model,
}

/// ATM straddle and the range it implies around spot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExpectedMove {
    /// Expiry the straddle was taken from.
    pub expiry: f64,
    /// Straddle strike: the strike nearest spot with both a call and a put.
    pub strike: f64,
    /// Call price plus put price.
    pub straddle: f64,
    /// `spot - straddle`.
    pub lower: f64,
    /// `spot + straddle`.
    pub upper: f64,
    /// Source of the call price.
    pub call_source: PriceSource,
    /// Source of the put price.
    pub put_source: PriceSource,
}

/// Call and put walls. With the default dealer signs these are the strikes
/// with the largest positive call GEX and the most negative put GEX.
#[must_use]
pub fn walls(per_strike: &[StrikeExposure], spot: f64) -> Walls {
    let pick = |value: fn(&StrikeExposure) -> f64| {
        per_strike
            .iter()
            .filter(|s| value(s) != 0.0)
            .min_by(|a, b| {
                value(b)
                    .abs()
                    .total_cmp(&value(a).abs())
                    .then_with(|| nearer(a.strike, b.strike, spot))
            })
            .map(|s| s.strike)
    };
    Walls {
        call_wall: pick(|s| s.call_gex),
        put_wall: pick(|s| s.put_gex),
    }
}

/// Settlement strike that minimises the total intrinsic value paid to option
/// holders at `expiry`. Candidates are the strikes listed for that expiry.
///
/// # Errors
///
/// Returns [`GexError::ExpiryNotFound`] if no contract has that expiry.
pub fn max_pain(chain: &Chain, expiry: f64) -> Result<f64> {
    let contracts: Vec<&OptionContract> = chain.contracts_expiring(expiry).collect();
    if contracts.is_empty() {
        return Err(GexError::ExpiryNotFound(expiry));
    }
    let strikes = distinct_strikes(&contracts);
    let pain: Vec<f64> = strikes.iter().map(|&p| pain_at(&contracts, p)).collect();
    let min = pain.iter().copied().fold(f64::INFINITY, f64::min);
    let best = strikes
        .iter()
        .zip(&pain)
        .filter(|&(_, &v)| v - min <= TIE_TOLERANCE * min.abs())
        .map(|(&k, _)| k)
        .min_by(|a, b| nearer(*a, *b, chain.spot()));
    // Non-empty contracts always give a candidate; keep the error path anyway
    // rather than panic.
    best.ok_or(GexError::ExpiryNotFound(expiry))
}

/// ATM straddle at `expiry`, using quoted mids where available and model
/// prices otherwise.
///
/// # Errors
///
/// Returns [`GexError::ExpiryNotFound`] if no contract has that expiry, or
/// [`GexError::NoStraddle`] if no strike at that expiry has both a call and
/// a put.
pub fn expected_move(chain: &Chain, expiry: f64) -> Result<ExpectedMove> {
    let mut contracts: Vec<&OptionContract> = chain.contracts_expiring(expiry).collect();
    if contracts.is_empty() {
        return Err(GexError::ExpiryNotFound(expiry));
    }
    // Canonical order groups each strike's contracts together and makes the
    // first call and first put at a strike the same every time.
    contracts.sort_by(|a, b| OptionContract::canonical_cmp(a, b));
    let spot = chain.spot();
    let (strike, call, put) = contracts
        .chunk_by(|a, b| a.strike().total_cmp(&b.strike()).is_eq())
        .filter_map(|group| {
            let first = |kind| group.iter().copied().find(|c| c.kind() == kind);
            Some((
                group.first()?.strike(),
                first(OptionKind::Call)?,
                first(OptionKind::Put)?,
            ))
        })
        .min_by(|a, b| nearer(a.0, b.0, spot))
        .ok_or(GexError::NoStraddle(expiry))?;
    let (call_price, call_source) = leg_price(chain, call);
    let (put_price, put_source) = leg_price(chain, put);
    let straddle = call_price + put_price;
    Ok(ExpectedMove {
        expiry: call.expiry(),
        strike,
        straddle,
        lower: spot - straddle,
        upper: spot + straddle,
        call_source,
        put_source,
    })
}

/// Total intrinsic value, in option points times open interest, if the
/// underlying settles at `p`.
#[allow(clippy::cast_precision_loss)] // open interest is far below 2^53
fn pain_at(contracts: &[&OptionContract], p: f64) -> f64 {
    contracts
        .iter()
        .map(|c| {
            let intrinsic = match c.kind() {
                OptionKind::Call => (p - c.strike()).max(0.0),
                OptionKind::Put => (c.strike() - p).max(0.0),
            };
            intrinsic * c.open_interest() as f64
        })
        .sum()
}

fn leg_price(chain: &Chain, c: &OptionContract) -> (f64, PriceSource) {
    if let Some(mid) = c.mid() {
        return (mid, PriceSource::Quote);
    }
    let input = BsmInput {
        kind: c.kind(),
        spot: chain.spot(),
        strike: c.strike(),
        t: c.expiry(),
        rate: chain.rate(),
        div_yield: chain.div_yield(),
        sigma: c.iv(),
    };
    (bsm::price(&input), PriceSource::Model)
}

fn distinct_strikes(contracts: &[&OptionContract]) -> Vec<f64> {
    let mut strikes: Vec<f64> = contracts.iter().map(|c| c.strike()).collect();
    strikes.sort_by(f64::total_cmp);
    strikes.dedup_by(|a, b| a.total_cmp(b).is_eq());
    strikes
}

/// Orders strikes by distance to spot, then lower first.
fn nearer(a: f64, b: f64, spot: f64) -> std::cmp::Ordering {
    (a - spot)
        .abs()
        .total_cmp(&(b - spot).abs())
        .then(a.total_cmp(&b))
}

#[cfg(test)]
// These tests compare exact values on purpose: identical code paths, exact
// limits such as 0 and 1, and values passed through unchanged.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn c(kind: OptionKind, strike: f64, oi: u64) -> OptionContract {
        OptionContract::new(kind, strike, 0.1, 0.2, oi).unwrap()
    }

    fn row(strike: f64, call_gex: f64, put_gex: f64) -> StrikeExposure {
        StrikeExposure {
            strike,
            call_gex,
            put_gex,
            gex: call_gex + put_gex,
            ..StrikeExposure::default()
        }
    }

    #[test]
    fn walls_pick_largest_magnitude_with_ties_nearest_spot() {
        let rows = [
            row(90.0, 0.0, -5.0),
            row(100.0, 3.0, -1.0),
            row(110.0, 7.0, 0.0),
            row(120.0, 7.0, 0.0),
        ];
        assert_eq!(
            walls(&rows, 112.0),
            Walls {
                call_wall: Some(110.0),
                put_wall: Some(90.0)
            }
        );
        assert_eq!(walls(&rows, 118.0).call_wall, Some(120.0));
        assert_eq!(
            walls(&rows, 115.0).call_wall,
            Some(110.0),
            "equidistant: lower strike"
        );
        assert_eq!(walls(&[], 100.0), Walls::default());
    }

    #[test]
    fn max_pain_matches_hand_computation() {
        // Pain at 95: puts 100,105 -> 5*10 + 10*10 = 150; calls none.
        // Pain at 100: calls 95 -> 5*20 = 100; puts 105 -> 5*10 = 50 => 150.
        // Pain at 105: calls 95,100 -> 10*20 + 5*30 = 350.
        let chain = Chain::new(
            101.0,
            0.0,
            0.0,
            vec![
                c(OptionKind::Call, 95.0, 20),
                c(OptionKind::Call, 100.0, 30),
                c(OptionKind::Put, 100.0, 10),
                c(OptionKind::Put, 105.0, 10),
            ],
        )
        .unwrap();
        // 95 and 100 tie at 150; 100 is nearer spot.
        assert_eq!(max_pain(&chain, 0.1), Ok(100.0));
        assert_eq!(max_pain(&chain, 0.2), Err(GexError::ExpiryNotFound(0.2)));
    }

    #[test]
    fn expected_move_prefers_quotes_and_falls_back_to_model() {
        let quoted_call = c(OptionKind::Call, 100.0, 1)
            .with_quote(Some(2.0), Some(2.2))
            .unwrap();
        let unquoted_put = c(OptionKind::Put, 100.0, 1);
        let chain = Chain::new(
            100.4,
            0.0,
            0.0,
            vec![
                quoted_call,
                unquoted_put.clone(),
                c(OptionKind::Call, 101.0, 1),
            ],
        )
        .unwrap();
        let em = expected_move(&chain, 0.1).unwrap();
        assert_eq!(em.strike, 100.0, "101 has no put");
        assert_eq!(
            (em.call_source, em.put_source),
            (PriceSource::Quote, PriceSource::Model)
        );
        let put_model = leg_price(&chain, &unquoted_put).0;
        assert!((em.straddle - (2.1 + put_model)).abs() < 1e-12);
        assert!((em.upper - em.lower - 2.0 * em.straddle).abs() < 1e-12);
    }

    #[test]
    fn expected_move_errors() {
        let chain = Chain::new(100.0, 0.0, 0.0, vec![c(OptionKind::Call, 100.0, 1)]).unwrap();
        assert_eq!(expected_move(&chain, 0.1), Err(GexError::NoStraddle(0.1)));
        assert_eq!(
            expected_move(&chain, 0.5),
            Err(GexError::ExpiryNotFound(0.5))
        );
    }
}
