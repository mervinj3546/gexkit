//! Option chain types.
//!
//! Constructors validate their inputs, so the analytics can trust any
//! `OptionContract` or `Chain` they receive.

use crate::error::{GexError, Result};

/// Contracts whose expiries differ by less than this (in years, about 0.03 s)
/// are treated as the same expiry.
pub const EXPIRY_TOLERANCE: f64 = 1e-9;

/// Call or put.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum OptionKind {
    /// Call option.
    Call,
    /// Put option.
    Put,
}

/// A single listed option.
///
/// `expiry` is a year fraction (the caller chooses the day count). `iv` is a
/// decimal; contracts with `iv <= 0` are accepted but skipped by the exposure
/// engine, because providers commonly report 0 when IV is missing.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "ContractRepr"))]
pub struct OptionContract {
    kind: OptionKind,
    strike: f64,
    expiry: f64,
    iv: f64,
    open_interest: u64,
    volume: u64,
    bid: Option<f64>,
    ask: Option<f64>,
}

impl OptionContract {
    /// Creates a contract with no volume and no quote.
    ///
    /// # Errors
    ///
    /// Returns an error if `strike`, `expiry` or `iv` is not finite, if
    /// `strike <= 0`, or if `expiry < 0`.
    pub fn new(
        kind: OptionKind,
        strike: f64,
        expiry: f64,
        iv: f64,
        open_interest: u64,
    ) -> Result<Self> {
        finite("strike", strike)?;
        finite("expiry", expiry)?;
        finite("iv", iv)?;
        if strike <= 0.0 {
            return Err(GexError::InvalidStrike {
                strike,
                index: None,
            });
        }
        if expiry < 0.0 {
            return Err(GexError::NegativeExpiry {
                expiry,
                index: None,
            });
        }
        Ok(Self {
            kind,
            strike,
            expiry,
            iv,
            open_interest,
            volume: 0,
            bid: None,
            ask: None,
        })
    }

    /// Sets the traded volume.
    #[must_use]
    pub fn with_volume(mut self, volume: u64) -> Self {
        self.volume = volume;
        self
    }

    /// Sets the bid and ask. Either side may be missing.
    ///
    /// # Errors
    ///
    /// Returns [`GexError::InvalidQuote`] if a side is negative or not finite,
    /// or if `bid > ask`.
    pub fn with_quote(mut self, bid: Option<f64>, ask: Option<f64>) -> Result<Self> {
        let bad_side = |x: Option<f64>| x.is_some_and(|v| !v.is_finite() || v < 0.0);
        let crossed = matches!((bid, ask), (Some(b), Some(a)) if b > a);
        if bad_side(bid) || bad_side(ask) || crossed {
            return Err(GexError::InvalidQuote {
                bid,
                ask,
                index: None,
            });
        }
        self.bid = bid;
        self.ask = ask;
        Ok(self)
    }

    /// Call or put.
    #[must_use]
    pub fn kind(&self) -> OptionKind {
        self.kind
    }

    /// Strike price.
    #[must_use]
    pub fn strike(&self) -> f64 {
        self.strike
    }

    /// Time to expiry as a year fraction.
    #[must_use]
    pub fn expiry(&self) -> f64 {
        self.expiry
    }

    /// Implied volatility as a decimal.
    #[must_use]
    pub fn iv(&self) -> f64 {
        self.iv
    }

    /// Open interest in contracts.
    #[must_use]
    pub fn open_interest(&self) -> u64 {
        self.open_interest
    }

    /// Traded volume in contracts.
    #[must_use]
    pub fn volume(&self) -> u64 {
        self.volume
    }

    /// Best bid, if quoted.
    #[must_use]
    pub fn bid(&self) -> Option<f64> {
        self.bid
    }

    /// Best ask, if quoted.
    #[must_use]
    pub fn ask(&self) -> Option<f64> {
        self.ask
    }

    /// Mid price when both sides are quoted and the ask is positive.
    #[must_use]
    pub fn mid(&self) -> Option<f64> {
        match (self.bid, self.ask) {
            (Some(b), Some(a)) if a > 0.0 => Some(f64::midpoint(b, a)),
            _ => None,
        }
    }

    /// Canonical order over every field (strike, kind, expiry, iv, open
    /// interest, volume, bid, ask; a missing quote sorts first), used before
    /// summing or picking a listing so results do not depend on input order.
    /// Contracts equal under it are identical.
    pub(crate) fn canonical_cmp(a: &Self, b: &Self) -> std::cmp::Ordering {
        let quote = |x: Option<f64>, y: Option<f64>| match (x, y) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            _ => x.is_some().cmp(&y.is_some()),
        };
        a.strike
            .total_cmp(&b.strike)
            .then(a.kind.cmp(&b.kind))
            .then(a.expiry.total_cmp(&b.expiry))
            .then(a.iv.total_cmp(&b.iv))
            .then(a.open_interest.cmp(&b.open_interest))
            .then(a.volume.cmp(&b.volume))
            .then(quote(a.bid, b.bid))
            .then(quote(a.ask, b.ask))
    }
}

/// An option chain on one underlying, possibly spanning several expiries.
///
/// `rate` and `div_yield` are continuously compounded decimals and may be
/// negative. `multiplier` is the contract size (default 100).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "ChainRepr"))]
pub struct Chain {
    spot: f64,
    rate: f64,
    div_yield: f64,
    multiplier: f64,
    contracts: Vec<OptionContract>,
}

impl Chain {
    /// Default contract multiplier.
    pub const DEFAULT_MULTIPLIER: f64 = 100.0;

    /// Creates a chain with the default multiplier.
    ///
    /// # Errors
    ///
    /// Returns an error if `spot` is not finite and positive, or if `rate` or
    /// `div_yield` is not finite.
    pub fn new(
        spot: f64,
        rate: f64,
        div_yield: f64,
        contracts: Vec<OptionContract>,
    ) -> Result<Self> {
        if !spot.is_finite() || spot <= 0.0 {
            return Err(GexError::InvalidSpot(spot));
        }
        finite("rate", rate)?;
        finite("div_yield", div_yield)?;
        Ok(Self {
            spot,
            rate,
            div_yield,
            multiplier: Self::DEFAULT_MULTIPLIER,
            contracts,
        })
    }

    /// Sets the contract multiplier.
    ///
    /// # Errors
    ///
    /// Returns [`GexError::InvalidMultiplier`] unless `multiplier` is finite
    /// and positive.
    pub fn with_multiplier(mut self, multiplier: f64) -> Result<Self> {
        if !multiplier.is_finite() || multiplier <= 0.0 {
            return Err(GexError::InvalidMultiplier(multiplier));
        }
        self.multiplier = multiplier;
        Ok(self)
    }

    /// Underlying spot price.
    #[must_use]
    pub fn spot(&self) -> f64 {
        self.spot
    }

    /// Risk-free rate, continuously compounded.
    #[must_use]
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Dividend yield, continuously compounded.
    #[must_use]
    pub fn div_yield(&self) -> f64 {
        self.div_yield
    }

    /// Contract multiplier.
    #[must_use]
    pub fn multiplier(&self) -> f64 {
        self.multiplier
    }

    /// All contracts, in input order.
    #[must_use]
    pub fn contracts(&self) -> &[OptionContract] {
        &self.contracts
    }

    /// Distinct expiries in ascending order, merging values within
    /// [`EXPIRY_TOLERANCE`]. Each group is represented by its smallest value.
    #[must_use]
    pub fn expiries(&self) -> Vec<f64> {
        let mut all: Vec<f64> = self.contracts.iter().map(OptionContract::expiry).collect();
        all.sort_by(f64::total_cmp);
        all.dedup_by(|later, first| *later - *first < EXPIRY_TOLERANCE);
        all
    }

    /// Contracts whose expiry is within [`EXPIRY_TOLERANCE`] of `expiry`.
    pub fn contracts_expiring(&self, expiry: f64) -> impl Iterator<Item = &OptionContract> {
        self.contracts
            .iter()
            .filter(move |c| (c.expiry - expiry).abs() < EXPIRY_TOLERANCE)
    }
}

fn finite(field: &'static str, x: f64) -> Result<()> {
    if x.is_finite() {
        Ok(())
    } else {
        Err(GexError::NonFinite { field, index: None })
    }
}

/// Unvalidated wire form of [`OptionContract`].
#[cfg(feature = "serde")]
#[derive(serde::Deserialize)]
struct ContractRepr {
    kind: OptionKind,
    strike: f64,
    expiry: f64,
    iv: f64,
    open_interest: u64,
    #[serde(default)]
    volume: u64,
    #[serde(default)]
    bid: Option<f64>,
    #[serde(default)]
    ask: Option<f64>,
}

#[cfg(feature = "serde")]
impl TryFrom<ContractRepr> for OptionContract {
    type Error = GexError;

    fn try_from(r: ContractRepr) -> Result<Self> {
        Self::new(r.kind, r.strike, r.expiry, r.iv, r.open_interest)?
            .with_volume(r.volume)
            .with_quote(r.bid, r.ask)
    }
}

/// Unvalidated wire form of [`Chain`]. Contracts are validated one by one so
/// errors can report their index.
#[cfg(feature = "serde")]
#[derive(serde::Deserialize)]
struct ChainRepr {
    spot: f64,
    rate: f64,
    div_yield: f64,
    #[serde(default = "default_multiplier")]
    multiplier: f64,
    contracts: Vec<ContractRepr>,
}

#[cfg(feature = "serde")]
fn default_multiplier() -> f64 {
    Chain::DEFAULT_MULTIPLIER
}

#[cfg(feature = "serde")]
impl TryFrom<ChainRepr> for Chain {
    type Error = GexError;

    fn try_from(r: ChainRepr) -> Result<Self> {
        let contracts = r
            .contracts
            .into_iter()
            .enumerate()
            .map(|(i, c)| OptionContract::try_from(c).map_err(|e| e.with_index(i)))
            .collect::<Result<Vec<_>>>()?;
        Chain::new(r.spot, r.rate, r.div_yield, contracts)?.with_multiplier(r.multiplier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(strike: f64, expiry: f64) -> OptionContract {
        OptionContract::new(OptionKind::Call, strike, expiry, 0.2, 10).unwrap()
    }

    #[test]
    fn contract_rejects_bad_fields() {
        let k = OptionKind::Put;
        assert_eq!(
            OptionContract::new(k, 0.0, 0.1, 0.2, 1),
            Err(GexError::InvalidStrike {
                strike: 0.0,
                index: None
            })
        );
        assert_eq!(
            OptionContract::new(k, 100.0, -0.1, 0.2, 1),
            Err(GexError::NegativeExpiry {
                expiry: -0.1,
                index: None
            })
        );
        assert_eq!(
            OptionContract::new(k, f64::NAN, 0.1, 0.2, 1),
            Err(GexError::NonFinite {
                field: "strike",
                index: None
            })
        );
        assert_eq!(
            OptionContract::new(k, 100.0, 0.1, f64::INFINITY, 1),
            Err(GexError::NonFinite {
                field: "iv",
                index: None
            })
        );
    }

    #[test]
    fn contract_accepts_zero_expiry_and_zero_iv() {
        let c = OptionContract::new(OptionKind::Call, 100.0, 0.0, 0.0, 5).unwrap();
        assert_eq!((c.expiry(), c.iv()), (0.0, 0.0));
    }

    #[test]
    fn quote_validation_and_mid() {
        let c = call(100.0, 0.1);
        assert!(c.clone().with_quote(Some(2.0), Some(1.0)).is_err());
        assert!(c.clone().with_quote(Some(-0.1), None).is_err());
        assert!(c.clone().with_quote(None, Some(f64::NAN)).is_err());

        let q = c.clone().with_quote(Some(1.0), Some(1.5)).unwrap();
        assert_eq!(q.mid(), Some(1.25));
        assert_eq!(c.clone().with_quote(Some(1.0), None).unwrap().mid(), None);
        assert_eq!(c.with_quote(Some(0.0), Some(0.0)).unwrap().mid(), None);
    }

    #[test]
    fn chain_validation() {
        assert_eq!(
            Chain::new(0.0, 0.0, 0.0, vec![]),
            Err(GexError::InvalidSpot(0.0))
        );
        assert_eq!(
            Chain::new(100.0, f64::NAN, 0.0, vec![]),
            Err(GexError::NonFinite {
                field: "rate",
                index: None
            })
        );
        let ch = Chain::new(100.0, -0.01, 0.0, vec![]).unwrap();
        assert_eq!(ch.multiplier(), Chain::DEFAULT_MULTIPLIER);
        assert_eq!(
            ch.clone().with_multiplier(-1.0),
            Err(GexError::InvalidMultiplier(-1.0))
        );
        assert_eq!(ch.with_multiplier(10.0).unwrap().multiplier(), 10.0);
    }

    #[test]
    fn expiries_are_sorted_and_merged_within_tolerance() {
        let ch = Chain::new(
            100.0,
            0.0,
            0.0,
            vec![
                call(100.0, 0.5),
                call(100.0, 0.1),
                call(105.0, 0.1 + 1e-12),
                call(90.0, 0.0),
            ],
        )
        .unwrap();
        assert_eq!(ch.expiries(), vec![0.0, 0.1, 0.5]);
        assert_eq!(ch.contracts_expiring(0.1).count(), 2);
        assert_eq!(ch.contracts_expiring(0.3).count(), 0);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn deserialize_validates_and_reports_index() {
        let ok = r#"{"spot":100,"rate":0.05,"div_yield":0,"contracts":[
            {"kind":"call","strike":100,"expiry":0.1,"iv":0.2,"open_interest":5,"bid":1,"ask":1.2}]}"#;
        let ch: Chain = serde_json::from_str(ok).unwrap();
        assert_eq!(ch.multiplier(), 100.0);
        assert_eq!(ch.contracts()[0].mid(), Some(1.1));

        let bad = r#"{"spot":100,"rate":0,"div_yield":0,"contracts":[
            {"kind":"put","strike":100,"expiry":0.1,"iv":0.2,"open_interest":5},
            {"kind":"put","strike":-5,"expiry":0.1,"iv":0.2,"open_interest":5}]}"#;
        let err = serde_json::from_str::<Chain>(bad).unwrap_err().to_string();
        assert!(err.contains("at contract 1"), "{err}");
    }
}
