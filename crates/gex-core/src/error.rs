//! Error type for `gex-core`.

use std::fmt;

/// Errors produced when validating inputs or running analytics.
///
/// Contract-level variants carry `index: Option<usize>`: the position of the
/// offending contract when it was validated as part of a chain, `None` when it
/// was validated on its own.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum GexError {
    /// A numeric field was NaN or infinite.
    #[error("{field} is not finite{}", At(*index))]
    NonFinite {
        /// Name of the field.
        field: &'static str,
        /// Contract index, if known.
        index: Option<usize>,
    },
    /// Spot must be finite and positive.
    #[error("spot must be positive, got {0}")]
    InvalidSpot(f64),
    /// Strike must be positive.
    #[error("strike must be positive, got {strike}{}", At(*index))]
    InvalidStrike {
        /// The rejected strike.
        strike: f64,
        /// Contract index, if known.
        index: Option<usize>,
    },
    /// Expiry (year fraction) must not be negative.
    #[error("expiry must not be negative, got {expiry}{}", At(*index))]
    NegativeExpiry {
        /// The rejected expiry.
        expiry: f64,
        /// Contract index, if known.
        index: Option<usize>,
    },
    /// Bid and ask must be non-negative with bid <= ask.
    #[error("invalid quote bid={bid:?} ask={ask:?}{}", At(*index))]
    InvalidQuote {
        /// The rejected bid.
        bid: Option<f64>,
        /// The rejected ask.
        ask: Option<f64>,
        /// Contract index, if known.
        index: Option<usize>,
    },
    /// Contract multiplier must be finite and positive.
    #[error("multiplier must be positive, got {0}")]
    InvalidMultiplier(f64),
    /// The chain has no contracts at all.
    #[error("chain has no contracts")]
    EmptyChain,
    /// Every contract has `iv <= 0`, so none can be priced; `skipped` is
    /// how many were set aside. Usually the data source supplied no IV.
    #[error("no contract has a positive implied volatility ({skipped} skipped with iv <= 0)")]
    NoUsableContracts {
        /// Contracts set aside for `iv <= 0`.
        skipped: usize,
    },
    /// No contract in the chain has the requested expiry.
    #[error("no contracts expire at {0}")]
    ExpiryNotFound(f64),
    /// No strike at the requested expiry has both a call and a put.
    #[error("no strike at expiry {0} has both a call and a put")]
    NoStraddle(f64),
    /// An `ExposureConfig` field is out of range.
    #[error("invalid exposure config: {0}")]
    InvalidConfig(&'static str),
}

impl GexError {
    /// Attaches a contract index to contract-level variants; other variants
    /// are returned unchanged.
    #[must_use]
    pub fn with_index(self, i: usize) -> Self {
        match self {
            Self::NonFinite { field, .. } => Self::NonFinite {
                field,
                index: Some(i),
            },
            Self::InvalidStrike { strike, .. } => Self::InvalidStrike {
                strike,
                index: Some(i),
            },
            Self::NegativeExpiry { expiry, .. } => Self::NegativeExpiry {
                expiry,
                index: Some(i),
            },
            Self::InvalidQuote { bid, ask, .. } => Self::InvalidQuote {
                bid,
                ask,
                index: Some(i),
            },
            other => other,
        }
    }
}

/// Shorthand for `Result<T, GexError>`.
pub type Result<T> = core::result::Result<T, GexError>;

/// Formats an optional contract index as ` at contract {i}`.
struct At(Option<usize>);

impl fmt::Display for At {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(i) => write!(f, " at contract {i}"),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_index_when_known() {
        let e = GexError::InvalidStrike {
            strike: -1.0,
            index: None,
        };
        assert_eq!(e.to_string(), "strike must be positive, got -1");
        assert_eq!(
            e.with_index(7).to_string(),
            "strike must be positive, got -1 at contract 7"
        );
    }

    #[test]
    fn with_index_leaves_chain_level_variants_alone() {
        assert_eq!(GexError::EmptyChain.with_index(3), GexError::EmptyChain);
    }
}
