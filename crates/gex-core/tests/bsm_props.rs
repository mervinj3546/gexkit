//! Property tests for the Black-Scholes-Merton greeks.

use gex_core::OptionKind;
use gex_core::bsm::{BsmInput, delta, greeks, price};
use proptest::prelude::*;

/// Inputs in a well-conditioned region, where finite differences are reliable.
fn input() -> impl Strategy<Value = BsmInput> {
    (
        prop_oneof![Just(OptionKind::Call), Just(OptionKind::Put)],
        50.0..150.0_f64,
        0.7..1.3_f64,
        0.02..2.0_f64,
        -0.02..0.1_f64,
        0.0..0.05_f64,
        0.05..1.0_f64,
    )
        .prop_map(
            |(kind, spot, moneyness, t, rate, div_yield, sigma)| BsmInput {
                kind,
                spot,
                strike: spot * moneyness,
                t,
                rate,
                div_yield,
                sigma,
            },
        )
}

/// Any inputs a validated chain can produce, including degenerate ones.
fn any_input() -> impl Strategy<Value = BsmInput> {
    (
        prop_oneof![Just(OptionKind::Call), Just(OptionKind::Put)],
        1e-3..1e6_f64,
        1e-3..1e6_f64,
        prop_oneof![Just(0.0), 0.0..1e-6_f64, 0.0..30.0_f64],
        -0.1..0.3_f64,
        -0.05..0.2_f64,
        prop_oneof![Just(0.0), 0.0..1e-6_f64, 0.0..5.0_f64],
    )
        .prop_map(|(kind, spot, strike, t, rate, div_yield, sigma)| BsmInput {
            kind,
            spot,
            strike,
            t,
            rate,
            div_yield,
            sigma,
        })
}

fn with_kind(i: BsmInput, kind: OptionKind) -> BsmInput {
    BsmInput { kind, ..i }
}

/// Central difference of `f` with respect to the field selected by `bump`.
fn central(i: BsmInput, h: f64, bump: fn(&mut BsmInput, f64), f: fn(&BsmInput) -> f64) -> f64 {
    let mut up = i;
    let mut dn = i;
    bump(&mut up, h);
    bump(&mut dn, -h);
    (f(&up) - f(&dn)) / (2.0 * h)
}

fn close(fd: f64, analytic: f64, tol: f64) -> bool {
    (fd - analytic).abs() <= tol * (1.0 + analytic.abs())
}

proptest! {
    #[test]
    fn put_call_parity(i in input()) {
        let c = price(&with_kind(i, OptionKind::Call));
        let p = price(&with_kind(i, OptionKind::Put));
        let fwd = i.spot * (-i.div_yield * i.t).exp() - i.strike * (-i.rate * i.t).exp();
        prop_assert!((c - p - fwd).abs() <= 1e-10 * i.spot, "c={c} p={p} fwd={fwd}");
    }

    // Gamma, vega and vanna share one code path for calls and puts, so they
    // must be bit-identical.
    #[allow(clippy::float_cmp)]
    #[test]
    fn delta_parity_and_shared_greeks(i in input()) {
        let c = greeks(&with_kind(i, OptionKind::Call));
        let p = greeks(&with_kind(i, OptionKind::Put));
        prop_assert!((c.delta - p.delta - (-i.div_yield * i.t).exp()).abs() < 1e-14);
        prop_assert!(c.gamma >= 0.0 && c.vega >= 0.0);
        prop_assert_eq!(c.gamma, p.gamma);
        prop_assert_eq!(c.vega, p.vega);
        prop_assert_eq!(c.vanna, p.vanna);
    }

    #[test]
    fn price_bounds(i in input()) {
        let s = i.spot * (-i.div_yield * i.t).exp();
        let k = i.strike * (-i.rate * i.t).exp();
        let eps = 1e-10 * i.spot;
        let c = price(&with_kind(i, OptionKind::Call));
        let p = price(&with_kind(i, OptionKind::Put));
        prop_assert!(c >= (s - k).max(0.0) - eps && c <= s + eps, "call {c}");
        prop_assert!(p >= (k - s).max(0.0) - eps && p <= k + eps, "put {p}");
    }

    #[test]
    fn call_price_monotone(i in input(), bump in 1e-3..0.2_f64) {
        let c = with_kind(i, OptionKind::Call);
        let base = price(&c);
        let eps = 1e-12 * i.spot;
        let higher_spot = price(&BsmInput { spot: c.spot * (1.0 + bump), ..c });
        let higher_strike = price(&BsmInput { strike: c.strike * (1.0 + bump), ..c });
        let higher_vol = price(&BsmInput { sigma: c.sigma + bump, ..c });
        prop_assert!(higher_spot >= base - eps, "spot up: {higher_spot} < {base}");
        prop_assert!(higher_strike <= base + eps, "strike up: {higher_strike} > {base}");
        prop_assert!(higher_vol >= base - eps, "vol up: {higher_vol} < {base}");
    }

    #[test]
    fn greeks_match_finite_differences(i in input()) {
        let g = greeks(&i);
        // Bump sizes scale with each input's natural width so truncation
        // error stays small even for low-vol, short-dated options.
        let hs = 1e-3 * i.spot * i.sigma * i.t.sqrt();
        let hv = 1e-4 * i.sigma;
        let ht = 1e-4 * i.t;
        let fd_delta = central(i, hs, |x, h| x.spot += h, price);
        let fd_gamma = central(i, hs, |x, h| x.spot += h, delta);
        let fd_vega = central(i, hv, |x, h| x.sigma += h, price);
        let fd_vanna = central(i, hv, |x, h| x.sigma += h, delta);
        let fd_charm = -central(i, ht, |x, h| x.t += h, delta);
        prop_assert!(close(fd_delta, g.delta, 1e-6), "delta {fd_delta} vs {}", g.delta);
        prop_assert!(close(fd_gamma, g.gamma, 1e-5), "gamma {fd_gamma} vs {}", g.gamma);
        prop_assert!(close(fd_vega, g.vega, 1e-6), "vega {fd_vega} vs {}", g.vega);
        prop_assert!(close(fd_vanna, g.vanna, 1e-5), "vanna {fd_vanna} vs {}", g.vanna);
        prop_assert!(close(fd_charm, g.charm, 1e-4), "charm {fd_charm} vs {}", g.charm);
    }

    #[test]
    fn never_nan(i in any_input()) {
        let g = greeks(&i);
        for v in [g.price, g.delta, g.gamma, g.vega, g.vanna, g.charm] {
            prop_assert!(!v.is_nan(), "{i:?} -> {g:?}");
        }
    }
}
