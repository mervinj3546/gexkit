//! The committed golden chains must match what the generator produces now,
//! so a generator change cannot silently drift from the golden outputs.
//!
//! Floats are compared to one rounding step rather than exactly: IVs and
//! quotes pass through `exp`/`ln`, whose last bit varies between platforms,
//! and a value on a rounding boundary may land one step either way.

use gex_core::{Chain, OptionContract};
use std::path::PathBuf;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../golden")
}

fn near(a: Option<f64>, b: Option<f64>, step: f64) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => (x - y).abs() <= step * 1.000_001,
        (None, None) => true,
        _ => false,
    }
}

fn same(a: &OptionContract, b: &OptionContract) -> bool {
    a.kind() == b.kind()
        && a.strike().total_cmp(&b.strike()).is_eq()
        && a.expiry().total_cmp(&b.expiry()).is_eq()
        && near(Some(a.iv()), Some(b.iv()), 1e-6)
        && a.open_interest().abs_diff(b.open_interest()) <= 1
        && a.volume().abs_diff(b.volume()) <= 1
        && near(a.bid(), b.bid(), 0.01)
        && near(a.ask(), b.ask(), 0.01)
}

// Spot and strikes are generated exactly (no rounding), so equality is exact.
#[allow(clippy::float_cmp)]
#[test]
fn committed_chains_match_generator() {
    for (name, generated) in gex_fixtures::presets::golden() {
        let path = golden_dir().join(format!("{name}.chain.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e}; run the gen binary", path.display()));
        let committed: Chain = serde_json::from_str(&text).expect("valid chain JSON");
        assert_eq!(committed.spot(), generated.spot(), "{name} spot");
        assert_eq!(
            committed.contracts().len(),
            generated.contracts().len(),
            "{name} length"
        );
        for (i, (a, b)) in committed
            .contracts()
            .iter()
            .zip(generated.contracts())
            .enumerate()
        {
            assert!(
                same(a, b),
                "{name} contract {i}: committed {a:?} generated {b:?}"
            );
        }
    }
}
