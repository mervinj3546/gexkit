//! Black-Scholes-Merton price and greeks with a continuous dividend yield.
//!
//! Units: `vega` and `vanna` are per 1.00 of volatility, `charm` is per year
//! and is defined as `-∂Δ/∂T` (the delta drift as time passes). The exposure
//! engine rescales these to its reporting units.
//!
//! When `sigma <= 0` or `t <= 0` the functions return the limits instead of
//! NaN: the discounted intrinsic price, a step delta (half the step exactly at
//! the money-forward), and zero for every other greek.

use crate::math::{norm_cdf, norm_pdf};
use crate::types::OptionKind;

/// Inputs to the pricing model. The functions do not validate these; build
/// them from a validated [`crate::Chain`] or check them yourself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BsmInput {
    /// Call or put.
    pub kind: OptionKind,
    /// Underlying price.
    pub spot: f64,
    /// Strike price.
    pub strike: f64,
    /// Time to expiry as a year fraction.
    pub t: f64,
    /// Risk-free rate, continuously compounded.
    pub rate: f64,
    /// Dividend yield, continuously compounded.
    pub div_yield: f64,
    /// Volatility as a decimal.
    pub sigma: f64,
}

/// Price and greeks of one option.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Greeks {
    /// Option value.
    pub price: f64,
    /// ∂V/∂S.
    pub delta: f64,
    /// ∂²V/∂S².
    pub gamma: f64,
    /// ∂V/∂σ, per 1.00 of volatility.
    pub vega: f64,
    /// ∂Δ/∂σ, per 1.00 of volatility.
    pub vanna: f64,
    /// −∂Δ/∂T, per year.
    pub charm: f64,
}

/// Quantities shared by every greek in the non-degenerate case.
struct Core {
    d1: f64,
    d2: f64,
    sqrt_t: f64,
    /// e^{-qT}
    dq: f64,
}

impl Core {
    /// `None` when the model degenerates (σ√T not positive, or d1 not finite).
    fn new(i: &BsmInput) -> Option<Self> {
        if !(i.sigma > 0.0 && i.t > 0.0) {
            return None;
        }
        let sqrt_t = i.t.sqrt();
        let vol = i.sigma * sqrt_t;
        let d1 = ((i.spot / i.strike).ln()
            + (i.rate - i.div_yield + 0.5 * i.sigma * i.sigma) * i.t)
            / vol;
        if !(vol > 0.0 && d1.is_finite()) {
            return None;
        }
        Some(Self {
            d1,
            d2: d1 - vol,
            sqrt_t,
            dq: (-i.div_yield * i.t).exp(),
        })
    }
}

/// Price and all greeks, sharing the common terms.
#[must_use]
pub fn greeks(i: &BsmInput) -> Greeks {
    let Some(c) = Core::new(i) else {
        return Greeks {
            price: degenerate_price(i),
            delta: degenerate_delta(i),
            ..Greeks::default()
        };
    };
    let pdf = norm_pdf(c.d1);
    Greeks {
        price: price_core(i, &c),
        delta: delta_core(i, &c),
        gamma: c.dq * pdf / (i.spot * i.sigma * c.sqrt_t),
        vega: i.spot * c.dq * pdf * c.sqrt_t,
        vanna: -c.dq * pdf * c.d2 / i.sigma,
        charm: charm_core(i, &c, pdf),
    }
}

/// Option value.
#[must_use]
pub fn price(i: &BsmInput) -> f64 {
    Core::new(i).map_or_else(|| degenerate_price(i), |c| price_core(i, &c))
}

/// ∂V/∂S.
#[must_use]
pub fn delta(i: &BsmInput) -> f64 {
    Core::new(i).map_or_else(|| degenerate_delta(i), |c| delta_core(i, &c))
}

/// ∂²V/∂S². Identical for calls and puts.
#[must_use]
pub fn gamma(i: &BsmInput) -> f64 {
    Core::new(i).map_or(0.0, |c| {
        c.dq * norm_pdf(c.d1) / (i.spot * i.sigma * c.sqrt_t)
    })
}

/// ∂V/∂σ per 1.00 of volatility. Identical for calls and puts.
#[must_use]
pub fn vega(i: &BsmInput) -> f64 {
    Core::new(i).map_or(0.0, |c| i.spot * c.dq * norm_pdf(c.d1) * c.sqrt_t)
}

/// ∂Δ/∂σ per 1.00 of volatility. Identical for calls and puts.
#[must_use]
pub fn vanna(i: &BsmInput) -> f64 {
    Core::new(i).map_or(0.0, |c| -c.dq * norm_pdf(c.d1) * c.d2 / i.sigma)
}

/// −∂Δ/∂T per year.
#[must_use]
pub fn charm(i: &BsmInput) -> f64 {
    Core::new(i).map_or(0.0, |c| charm_core(i, &c, norm_pdf(c.d1)))
}

fn price_core(i: &BsmInput, c: &Core) -> f64 {
    let fwd_s = i.spot * c.dq;
    let disc_k = i.strike * (-i.rate * i.t).exp();
    match i.kind {
        OptionKind::Call => fwd_s * norm_cdf(c.d1) - disc_k * norm_cdf(c.d2),
        OptionKind::Put => disc_k * norm_cdf(-c.d2) - fwd_s * norm_cdf(-c.d1),
    }
}

fn delta_core(i: &BsmInput, c: &Core) -> f64 {
    match i.kind {
        OptionKind::Call => c.dq * norm_cdf(c.d1),
        OptionKind::Put => -c.dq * norm_cdf(-c.d1),
    }
}

fn charm_core(i: &BsmInput, c: &Core, pdf: f64) -> f64 {
    // (2(r-q)T - d2·σ√T) / (2T·σ√T), split so the denominator cannot
    // underflow for tiny T. When φ(d1) has underflowed the term is 0 even if
    // d2/(2T) overflows, since φ decays far faster.
    let vol = i.sigma * c.sqrt_t;
    let common = if pdf > 0.0 {
        c.dq * pdf * ((i.rate - i.div_yield) / vol - c.d2 / (2.0 * i.t))
    } else {
        0.0
    };
    match i.kind {
        OptionKind::Call => i.div_yield * c.dq * norm_cdf(c.d1) - common,
        OptionKind::Put => -i.div_yield * c.dq * norm_cdf(-c.d1) - common,
    }
}

/// Discounted spot and strike, with negative time treated as expired.
fn discounted(i: &BsmInput) -> (f64, f64, f64) {
    let t = i.t.max(0.0);
    let dq = (-i.div_yield * t).exp();
    (i.spot * dq, i.strike * (-i.rate * t).exp(), dq)
}

fn degenerate_price(i: &BsmInput) -> f64 {
    let (s, k, _) = discounted(i);
    match i.kind {
        OptionKind::Call => (s - k).max(0.0),
        OptionKind::Put => (k - s).max(0.0),
    }
}

fn degenerate_delta(i: &BsmInput) -> f64 {
    let (s, k, dq) = discounted(i);
    let step = match s.partial_cmp(&k) {
        Some(std::cmp::Ordering::Greater) => 1.0,
        Some(std::cmp::Ordering::Equal) => 0.5,
        _ => 0.0,
    };
    match i.kind {
        OptionKind::Call => dq * step,
        OptionKind::Put => -dq * (1.0 - step),
    }
}

#[cfg(test)]
// These tests compare exact values on purpose: identical code paths, exact
// limits such as 0 and 1, and values passed through unchanged.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn input(kind: OptionKind, spot: f64, strike: f64, t: f64, rate: f64, sigma: f64) -> BsmInput {
        BsmInput {
            kind,
            spot,
            strike,
            t,
            rate,
            div_yield: 0.0,
            sigma,
        }
    }

    #[test]
    fn hull_example_prices() {
        // Hull, Options, Futures and Other Derivatives, Example 15.6.
        let c = input(OptionKind::Call, 42.0, 40.0, 0.5, 0.1, 0.2);
        let p = BsmInput {
            kind: OptionKind::Put,
            ..c
        };
        assert!((price(&c) - 4.7594).abs() < 5e-5, "{}", price(&c));
        assert!((price(&p) - 0.8086).abs() < 5e-5, "{}", price(&p));
    }

    #[test]
    fn hull_example_greeks() {
        // Hull, Examples 19.1-19.6: S=49, K=50, r=5%, sigma=20%, T=20 weeks.
        let c = input(OptionKind::Call, 49.0, 50.0, 0.3846, 0.05, 0.2);
        let g = greeks(&c);
        assert!((g.delta - 0.522).abs() < 5e-4, "delta {}", g.delta);
        assert!((g.gamma - 0.066).abs() < 5e-4, "gamma {}", g.gamma);
        assert!((g.vega - 12.1).abs() < 0.05, "vega {}", g.vega);
    }

    #[test]
    fn greeks_matches_individual_functions() {
        for kind in [OptionKind::Call, OptionKind::Put] {
            let i = BsmInput {
                kind,
                spot: 100.0,
                strike: 95.0,
                t: 0.25,
                rate: 0.03,
                div_yield: 0.01,
                sigma: 0.3,
            };
            let g = greeks(&i);
            assert_eq!(g.price, price(&i));
            assert_eq!(g.delta, delta(&i));
            assert_eq!(g.gamma, gamma(&i));
            assert_eq!(g.vega, vega(&i));
            assert_eq!(g.vanna, vanna(&i));
            assert_eq!(g.charm, charm(&i));
        }
    }

    #[test]
    fn expired_and_zero_vol_return_limits() {
        let itm_call = input(OptionKind::Call, 105.0, 100.0, 0.0, 0.05, 0.2);
        assert_eq!(
            greeks(&itm_call),
            Greeks {
                price: 5.0,
                delta: 1.0,
                ..Greeks::default()
            }
        );

        let otm_put = BsmInput {
            kind: OptionKind::Put,
            ..itm_call
        };
        assert_eq!(greeks(&otm_put), Greeks::default());

        let atm_put = input(OptionKind::Put, 100.0, 100.0, -0.1, 0.05, 0.2);
        assert_eq!(
            greeks(&atm_put),
            Greeks {
                delta: -0.5,
                ..Greeks::default()
            }
        );

        // Zero vol with time left: discounted forward intrinsic.
        let zv = BsmInput {
            kind: OptionKind::Call,
            spot: 100.0,
            strike: 100.0,
            t: 1.0,
            rate: 0.05,
            div_yield: 0.0,
            sigma: 0.0,
        };
        let g = greeks(&zv);
        assert!((g.price - (100.0 - 100.0 * (-0.05_f64).exp())).abs() < 1e-12);
        assert_eq!((g.delta, g.gamma, g.vega), (1.0, 0.0, 0.0));
    }

    #[test]
    fn tiny_time_does_not_produce_nan() {
        for t in [1e-12, 1e-30, 1e-300, f64::MIN_POSITIVE] {
            for strike in [99.0, 100.0, 101.0] {
                for kind in [OptionKind::Call, OptionKind::Put] {
                    let g = greeks(&input(kind, 100.0, strike, t, 0.05, 0.2));
                    for v in [g.price, g.delta, g.gamma, g.vega, g.vanna, g.charm] {
                        assert!(!v.is_nan(), "{kind:?} K={strike} t={t}: {g:?}");
                    }
                }
            }
        }
    }
}
