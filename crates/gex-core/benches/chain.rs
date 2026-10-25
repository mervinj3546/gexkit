//! Benchmarks on a full 0DTE index chain: 1,501 strikes, 3,002 contracts.

use criterion::{Criterion, criterion_group, criterion_main};
use gex_core::bsm::{self, BsmInput};
use gex_core::{
    ExposureConfig, OptionKind, analyze, expected_move, max_pain, per_strike, zero_gamma,
};
use gex_fixtures::presets;
use std::hint::black_box;

fn benches(c: &mut Criterion) {
    let chain = gex_fixtures::generate(&presets::bench_0dte());
    let cfg = ExposureConfig::default();
    let expiry = chain.expiries()[0];

    let atm = BsmInput {
        kind: OptionKind::Call,
        spot: chain.spot(),
        strike: 5_805.0,
        t: gex_fixtures::ZERO_DTE,
        rate: chain.rate(),
        div_yield: chain.div_yield(),
        sigma: 0.14,
    };
    c.bench_function("greeks_single", |b| b.iter(|| bsm::greeks(black_box(&atm))));
    c.bench_function("per_strike_3000", |b| {
        b.iter(|| per_strike(black_box(&chain), &cfg));
    });
    c.bench_function("zero_gamma_3000", |b| {
        b.iter(|| zero_gamma(black_box(&chain), &cfg));
    });
    c.bench_function("max_pain_3000", |b| {
        b.iter(|| max_pain(black_box(&chain), expiry));
    });
    c.bench_function("expected_move_3000", |b| {
        b.iter(|| expected_move(black_box(&chain), expiry));
    });
    c.bench_function("analyze_3000", |b| {
        b.iter(|| analyze(black_box(&chain), &cfg));
    });
}

criterion_group!(chain_benches, benches);
criterion_main!(chain_benches);
