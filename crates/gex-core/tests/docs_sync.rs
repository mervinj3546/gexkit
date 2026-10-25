//! Facts stated in both the code and docs/CONVENTIONS.md must agree.

use std::path::PathBuf;

fn conventions() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/CONVENTIONS.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn methodology_version_matches_conventions() {
    let want = format!("**Methodology version {}**", gex_core::METHODOLOGY_VERSION);
    assert!(
        conventions().contains(&want),
        "CONVENTIONS.md must state {want}"
    );
}

#[test]
fn tolerances_match_conventions() {
    let doc = conventions();
    for (name, value) in [
        ("EXPIRY_TOLERANCE", gex_core::EXPIRY_TOLERANCE),
        ("STRIKE_TOLERANCE", gex_core::STRIKE_TOLERANCE),
        ("NEGLIGIBLE_GEX_FRACTION", gex_core::NEGLIGIBLE_GEX_FRACTION),
    ] {
        let stated = format!("`{name} = {value:e}`");
        assert!(doc.contains(&stated), "CONVENTIONS.md must state {stated}");
    }
}
