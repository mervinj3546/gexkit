//! Compares gex-core with the independent Python reference
//! (`fixtures/reference/gex_reference.py`) on the committed golden chains.
//!
//! Tolerances:
//! - per-contract greeks: `|a - b| <= 1e-12 + 1e-9 · |b|`
//! - per-strike and total exposures: `|a - b| <= 1e-9 · Σ|terms|`, the sum of
//!   absolute contributions, so cancelling terms do not inflate the error
//! - walls, max pain, straddle strike, regime, skipped count: exact
//! - zero-gamma crossings: `1e-6 · spot`
//! - expected move straddle: `1e-12 + 1e-9 · |b|`

use gex_core::bsm::{self, BsmInput};
use gex_core::{
    Chain, ExposureConfig, GexError, PriceSource, Regime, analyze, expected_move, max_pain,
};
use serde_json::Value;
use std::path::PathBuf;

const NAMES: [&str; 4] = ["small_0dte", "multi_expiry", "negative_gamma", "edge_cases"];

fn load(name: &str) -> (Chain, Value) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/golden");
    let read = |file: String| {
        let path = dir.join(file);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    };
    let chain = serde_json::from_str(&read(format!("{name}.chain.json"))).expect("chain");
    let expected = serde_json::from_str(&read(format!("{name}.expected.json"))).expect("expected");
    (chain, expected)
}

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"))
}

fn opt_f(v: &Value) -> Option<f64> {
    if v.is_null() { None } else { Some(f(v)) }
}

#[track_caller]
fn check_rel(what: &str, got: f64, want: f64) {
    let tol = 1e-12 + 1e-9 * want.abs();
    assert!(
        (got - want).abs() <= tol,
        "{what}: got {got}, want {want}, diff {}",
        (got - want).abs()
    );
}

#[track_caller]
fn check_scaled(what: &str, got: f64, want: f64, scale: f64) {
    let tol = 1e-9 * scale + 1e-300;
    assert!(
        (got - want).abs() <= tol,
        "{what}: got {got}, want {want}, scale {scale}"
    );
}

#[test]
fn per_contract_greeks() {
    for name in NAMES {
        let (chain, exp) = load(name);
        let want = exp["contracts"].as_array().expect("contracts");
        assert_eq!(want.len(), chain.contracts().len(), "{name}");
        for (i, (c, w)) in chain.contracts().iter().zip(want).enumerate() {
            let g = bsm::greeks(&BsmInput {
                kind: c.kind(),
                spot: chain.spot(),
                strike: c.strike(),
                t: c.expiry(),
                rate: chain.rate(),
                div_yield: chain.div_yield(),
                sigma: c.iv(),
            });
            let fields = [
                ("price", g.price),
                ("delta", g.delta),
                ("gamma", g.gamma),
                ("vega", g.vega),
                ("vanna", g.vanna),
                ("charm", g.charm),
            ];
            for (field, got) in fields {
                check_rel(&format!("{name}[{i}].{field}"), got, f(&w[field]));
            }
        }
    }
}

// Strikes and expiries are read from the same JSON as the reference used,
// so they must match exactly.
#[allow(clippy::float_cmp)]
#[test]
fn exposures_and_levels() {
    for name in NAMES {
        let (chain, exp) = load(name);
        let r = analyze(&chain, &ExposureConfig::default()).expect("analyze");

        let rows = exp["per_strike"].as_array().expect("per_strike");
        assert_eq!(r.per_strike.len(), rows.len(), "{name} strikes");
        for (got, want) in r.per_strike.iter().zip(rows) {
            let k = got.strike;
            assert_eq!(k, f(&want["strike"]), "{name} strike order");
            assert_eq!(
                got.call_oi,
                want["call_oi"].as_u64().expect("u64"),
                "{name} {k} call_oi"
            );
            assert_eq!(
                got.put_oi,
                want["put_oi"].as_u64().expect("u64"),
                "{name} {k} put_oi"
            );
            let fields = [
                ("call_gex", got.call_gex),
                ("put_gex", got.put_gex),
                ("gex", got.gex),
                ("dex", got.dex),
                ("vanna", got.vanna),
                ("charm", got.charm),
            ];
            for (field, v) in fields {
                let scale = f(&want[format!("{field}_abs")]);
                check_scaled(&format!("{name} {k} {field}"), v, f(&want[field]), scale);
            }
        }

        let totals = [
            ("gex", r.total_gex),
            ("dex", r.total_dex),
            ("vanna", r.total_vanna),
            ("charm", r.total_charm),
        ];
        for (field, v) in totals {
            let key = format!("total_{field}");
            let scale = f(&exp[format!("{key}_abs")]);
            check_scaled(&format!("{name} {key}"), v, f(&exp[&key]), scale);
        }

        let regime = match r.regime {
            Regime::PositiveGamma => "PositiveGamma",
            Regime::NegativeGamma => "NegativeGamma",
            Regime::Neutral => "Neutral",
        };
        assert_eq!(regime, exp["regime"], "{name} regime");
        assert_eq!(
            r.skipped as u64,
            exp["skipped"].as_u64().expect("u64"),
            "{name} skipped"
        );
        assert_eq!(
            r.walls.call_wall,
            opt_f(&exp["walls"]["call_wall"]),
            "{name} call wall"
        );
        assert_eq!(
            r.walls.put_wall,
            opt_f(&exp["walls"]["put_wall"]),
            "{name} put wall"
        );

        let tol = 1e-6 * chain.spot();
        match (&r.zero_gamma, &exp["zero_gamma"]) {
            (None, Value::Null) => {}
            (Some(z), want) if !want.is_null() => {
                let crossings = want["crossings"].as_array().expect("crossings");
                assert_eq!(z.crossings.len(), crossings.len(), "{name} crossing count");
                for (g, w) in z.crossings.iter().zip(crossings) {
                    assert!((g - f(w)).abs() <= tol, "{name} crossing {g} vs {w}");
                }
                assert!(
                    (z.level - f(&want["level"])).abs() <= tol,
                    "{name} flip level"
                );
            }
            (got, want) => panic!("{name} zero gamma: got {got:?}, want {want}"),
        }
    }
}

// Strikes and expiries are read from the same JSON as the reference used,
// so they must match exactly.
#[allow(clippy::float_cmp)]
#[test]
fn per_expiry_levels() {
    for name in NAMES {
        let (chain, exp) = load(name);
        let want = exp["expiries"].as_array().expect("expiries");
        assert_eq!(chain.expiries().len(), want.len(), "{name} expiry count");
        for (e, w) in chain.expiries().into_iter().zip(want) {
            assert_eq!(e, f(&w["expiry"]), "{name} expiry");
            assert_eq!(
                max_pain(&chain, e),
                Ok(f(&w["max_pain"])),
                "{name} max pain at {e}"
            );

            let em = &w["expected_move"];
            match expected_move(&chain, e) {
                Err(GexError::NoStraddle(_)) => {
                    assert_eq!(em["error"], "NoStraddle", "{name} at {e}");
                }
                Err(other) => panic!("{name} at {e}: {other}"),
                Ok(m) => {
                    assert_eq!(m.strike, f(&em["strike"]), "{name} straddle strike at {e}");
                    check_rel(
                        &format!("{name} straddle at {e}"),
                        m.straddle,
                        f(&em["straddle"]),
                    );
                    check_rel(&format!("{name} lower at {e}"), m.lower, f(&em["lower"]));
                    check_rel(&format!("{name} upper at {e}"), m.upper, f(&em["upper"]));
                    let src = |s: PriceSource| {
                        if s == PriceSource::Quote {
                            "Quote"
                        } else {
                            "Model"
                        }
                    };
                    assert_eq!(
                        src(m.call_source),
                        em["call_source"],
                        "{name} call source at {e}"
                    );
                    assert_eq!(
                        src(m.put_source),
                        em["put_source"],
                        "{name} put source at {e}"
                    );
                }
            }
        }
    }
}
