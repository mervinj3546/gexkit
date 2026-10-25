//! Core computation for gexkit: option chain types, Black-Scholes-Merton
//! greeks, and dealer exposure analytics.
//!
//! ```
//! use gex_core::{Chain, ExposureConfig, OptionContract, OptionKind, Regime, analyze};
//!
//! let week = 7.0 / 365.0;
//! let chain = Chain::new(100.0, 0.045, 0.0, vec![
//!     OptionContract::new(OptionKind::Call, 105.0, week, 0.20, 1_500)?,
//!     OptionContract::new(OptionKind::Put, 95.0, week, 0.25, 2_000)?,
//! ])?;
//! let report = analyze(&chain, &ExposureConfig::default())?;
//!
//! // Dealers are short the larger, closer-to-the-money put position, so they
//! // are short gamma at 100 and turn long gamma just above it.
//! assert_eq!(report.regime, Regime::NegativeGamma);
//! let flip = report.zero_gamma.expect("GEX changes sign").level;
//! assert!(100.0 < flip && flip < 101.0);
//! assert_eq!(report.walls.call_wall, Some(105.0));
//! assert_eq!(report.walls.put_wall, Some(95.0));
//! # Ok::<(), gex_core::GexError>(())
//! ```
//!
//! See `docs/CONVENTIONS.md` in the repository for units and sign conventions.

pub mod bsm;
pub mod error;
pub mod exposure;
pub mod levels;
pub mod math;
pub mod types;

pub use bsm::{BsmInput, Greeks};
pub use error::{GexError, Result};
pub use exposure::{
    ExposureConfig, ExposureReport, NEGLIGIBLE_GEX_FRACTION, Regime, SpotGrid, StrikeExposure,
    ZeroGamma, analyze, negligible_gex, per_strike, total_gex_at, zero_gamma,
};
pub use levels::{ExpectedMove, PriceSource, Walls, expected_move, max_pain, walls};
pub use math::{erfc, norm_cdf, norm_pdf};
pub use types::{Chain, EXPIRY_TOLERANCE, OptionContract, OptionKind};

/// Compiles and runs the README's code blocks as doctests.
#[cfg(doctest)]
#[doc = include_str!("../../../README.md")]
pub struct ReadmeDoctests;
