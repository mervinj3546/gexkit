//! Property tests for the exposure engine and levels on random chains.

use gex_core::{
    Chain, ExposureConfig, ExposureReport, OptionContract, OptionKind, analyze, expected_move,
    max_pain, total_gex_at,
};
use proptest::prelude::*;

const DAY: f64 = 1.0 / 365.0;

fn contract(spot: f64) -> impl Strategy<Value = OptionContract> {
    (
        prop_oneof![Just(OptionKind::Call), Just(OptionKind::Put)],
        0.7..1.3_f64,
        prop_oneof![Just(0.0), (0.1..60.0_f64).prop_map(|d| d * DAY)],
        prop_oneof![1 => Just(0.0), 9 => 0.05..1.0_f64],
        0..5_000_u64,
        proptest::option::of((0.0..20.0_f64, 0.0..2.0_f64)),
    )
        .prop_map(move |(kind, m, expiry, iv, oi, quote)| {
            // Half-point strikes so several contracts share a strike.
            let strike = (spot * m * 2.0).round() / 2.0;
            let c = OptionContract::new(kind, strike, expiry, iv, oi).expect("valid");
            match quote {
                Some((bid, spread)) => c.with_quote(Some(bid), Some(bid + spread)).expect("valid"),
                None => c,
            }
        })
}

fn contracts(spot: f64) -> impl Strategy<Value = Vec<OptionContract>> {
    prop::collection::vec(contract(spot), 1..40)
}

/// A chain and the same chain with its contracts shuffled.
fn chain_and_shuffled() -> impl Strategy<Value = (Chain, Chain)> {
    (50.0..500.0_f64, -0.02..0.08_f64, 0.0..0.04_f64).prop_flat_map(|(spot, r, q)| {
        contracts(spot).prop_flat_map(move |cs| {
            (Just(cs.clone()), Just(cs).prop_shuffle()).prop_map(move |(a, b)| {
                (
                    Chain::new(spot, r, q, a).expect("valid"),
                    Chain::new(spot, r, q, b).expect("valid"),
                )
            })
        })
    })
}

fn chain() -> impl Strategy<Value = Chain> {
    chain_and_shuffled().prop_map(|(c, _)| c)
}

fn report(chain: &Chain) -> Option<ExposureReport> {
    analyze(chain, &ExposureConfig::default()).ok()
}

/// Total GEX, DEX, vanna and charm of a report.
fn totals(r: &ExposureReport) -> [f64; 4] {
    [r.total_gex, r.total_dex, r.total_vanna, r.total_charm]
}

/// Sum over contracts of each one's absolute contribution to the four
/// totals, found by analysing every contract as a one-contract chain. Rounding
/// error in a total is bounded by a small multiple of this, however much the
/// signed terms cancel.
fn abs_totals(chain: &Chain) -> [f64; 4] {
    let mut out = [0.0; 4];
    for c in chain.contracts() {
        let single = Chain::new(
            chain.spot(),
            chain.rate(),
            chain.div_yield(),
            vec![c.clone()],
        )
        .expect("valid");
        if let Some(r) = report(&single) {
            for (o, t) in out.iter_mut().zip(totals(&r)) {
                *o += t.abs();
            }
        }
    }
    out
}

/// `a` and `b` agree to well within the rounding error of a sum whose
/// absolute terms total `scale`.
fn sum_close(a: f64, b: f64, scale: f64) -> bool {
    (a - b).abs() <= 1e-12 * scale + 1e-300
}

#[allow(clippy::cast_precision_loss)] // open interest is far below 2^53
fn brute_force_pain(chain: &Chain, expiry: f64, p: f64) -> f64 {
    chain
        .contracts_expiring(expiry)
        .map(|c| {
            let intrinsic = match c.kind() {
                OptionKind::Call => (p - c.strike()).max(0.0),
                OptionKind::Put => (c.strike() - p).max(0.0),
            };
            intrinsic * c.open_interest() as f64
        })
        .sum()
}

proptest! {
    #[test]
    fn input_order_does_not_matter((a, b) in chain_and_shuffled()) {
        // Canonical ordering makes results bit-identical, not just close.
        prop_assert_eq!(report(&a), report(&b));
        for e in a.expiries() {
            prop_assert_eq!(max_pain(&a, e), max_pain(&b, e));
            prop_assert_eq!(expected_move(&a, e), expected_move(&b, e));
        }
    }

    #[test]
    fn exposure_scales_with_open_interest(ch in chain(), k in 2..50_u64) {
        let scaled: Vec<OptionContract> = ch
            .contracts()
            .iter()
            .map(|c| {
                OptionContract::new(c.kind(), c.strike(), c.expiry(), c.iv(), c.open_interest() * k)
                    .expect("valid")
            })
            .collect();
        let scaled = Chain::new(ch.spot(), ch.rate(), ch.div_yield(), scaled).expect("valid");
        if let (Some(r1), Some(rk)) = (report(&ch), report(&scaled)) {
            #[allow(clippy::cast_precision_loss)]
            let k = k as f64;
            let scale = abs_totals(&scaled);
            for ((x, y), s) in totals(&r1).into_iter().zip(totals(&rk)).zip(scale) {
                prop_assert!(sum_close(x * k, y, s), "{x} * {k} vs {y} (scale {s})");
            }
            prop_assert_eq!(r1.walls, rk.walls);
        }
    }

    #[test]
    fn exposure_is_additive(a in chain(), extra in contracts(100.0)) {
        let b = Chain::new(a.spot(), a.rate(), a.div_yield(), extra).expect("valid");
        let mut all = a.contracts().to_vec();
        all.extend_from_slice(b.contracts());
        let ab = Chain::new(a.spot(), a.rate(), a.div_yield(), all).expect("valid");
        if let (Some(ra), Some(rb), Some(rab)) = (report(&a), report(&b), report(&ab)) {
            let parts = totals(&ra).into_iter().zip(totals(&rb)).map(|(x, y)| x + y);
            for ((sum, whole), s) in parts.zip(totals(&rab)).zip(abs_totals(&ab)) {
                prop_assert!(sum_close(sum, whole, s), "{sum} vs {whole} (scale {s})");
            }
        }
    }

    #[test]
    fn zero_gamma_crossings_are_sign_changes(ch in chain()) {
        let cfg = ExposureConfig::default();
        let Some(r) = report(&ch) else { return Ok(()) };
        let Some(z) = r.zero_gamma else { return Ok(()) };
        let (lo, hi) = (ch.spot() * cfg.flip_grid.lower, ch.spot() * cfg.flip_grid.upper);
        prop_assert!(z.crossings.windows(2).all(|w| w[0] < w[1]), "{:?}", z.crossings);
        prop_assert!(z.crossings.contains(&z.level));
        let h = 1e-6 * ch.spot();
        for &x in &z.crossings {
            prop_assert!((lo..=hi).contains(&x));
            let below = total_gex_at(&ch, x - h, &cfg);
            let above = total_gex_at(&ch, x + h, &cfg);
            // Either a strict sign change, or GEX is exactly zero here.
            prop_assert!(below * above <= 0.0 || total_gex_at(&ch, x, &cfg) == 0.0,
                "no sign change at {x}: {below} / {above}");
        }
    }

    #[test]
    fn max_pain_minimises_brute_force(ch in chain()) {
        for e in ch.expiries() {
            let mp = max_pain(&ch, e).expect("expiry exists");
            let strikes: Vec<f64> = ch.contracts_expiring(e).map(OptionContract::strike).collect();
            prop_assert!(strikes.contains(&mp));
            let at_mp = brute_force_pain(&ch, e, mp);
            for &k in &strikes {
                prop_assert!(at_mp <= brute_force_pain(&ch, e, k) * (1.0 + 1e-12), "{mp} vs {k}");
            }
        }
    }

    // Walls are selected from these exact values, so equality is exact.
    #[allow(clippy::float_cmp)]
    #[test]
    fn walls_are_listed_strikes_with_largest_gex(ch in chain()) {
        let Some(r) = report(&ch) else { return Ok(()) };
        let max_abs = |f: fn(&gex_core::StrikeExposure) -> f64| {
            r.per_strike.iter().map(|s| f(s).abs()).fold(0.0, f64::max)
        };
        for (wall, f) in [
            (r.walls.call_wall, (|s: &gex_core::StrikeExposure| s.call_gex) as fn(&_) -> f64),
            (r.walls.put_wall, |s: &gex_core::StrikeExposure| s.put_gex),
        ] {
            if let Some(k) = wall {
                let row = r.per_strike.iter().find(|s| s.strike == k);
                prop_assert!(row.is_some(), "wall {k} not a strike");
                prop_assert!(f(row.expect("checked")).abs() == max_abs(f));
            } else {
                prop_assert!(max_abs(f) == 0.0);
            }
        }
    }

    #[test]
    fn report_is_finite(ch in chain()) {
        if let Some(r) = report(&ch) {
            for v in [r.total_gex, r.total_dex, r.total_vanna, r.total_charm] {
                prop_assert!(v.is_finite(), "{r:?}");
            }
            for s in &r.per_strike {
                for v in [s.gex, s.dex, s.vanna, s.charm] {
                    prop_assert!(v.is_finite(), "{s:?}");
                }
            }
        }
    }
}
