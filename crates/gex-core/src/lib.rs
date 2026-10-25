//! Core computation for gexkit: option chain types, Black-Scholes-Merton
//! greeks, and dealer exposure analytics.
//!
//! See `docs/CONVENTIONS.md` in the repository for units and sign conventions.

pub mod bsm;
pub mod error;
pub mod exposure;
pub mod math;
pub mod types;

pub use bsm::{BsmInput, Greeks};
pub use error::{GexError, Result};
pub use exposure::{
    ExposureConfig, ExposureReport, Regime, SpotGrid, StrikeExposure, ZeroGamma, analyze,
    zero_gamma,
};
pub use math::{erfc, norm_cdf, norm_pdf};
pub use types::{Chain, EXPIRY_TOLERANCE, OptionContract, OptionKind};
