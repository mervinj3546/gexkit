//! Standard normal density and distribution functions.
//!
//! `erfc` uses W. J. Cody's rational Chebyshev approximations (Math. Comp. 23,
//! 1969, as implemented in his CALERF routine), which are accurate to about
//! 1e-16 relative in double precision and keep full relative accuracy deep in
//! the lower tail, where computing `1 - erf` would cancel to zero.

/// 1 / sqrt(2π).
const INV_SQRT_2PI: f64 = 0.398_942_280_401_432_7;
/// 1 / sqrt(π).
const INV_SQRT_PI: f64 = 0.564_189_583_547_756_3;
/// Above this, `erfc(x)` underflows to 0.
const ERFC_UNDERFLOW: f64 = 26.543;

/// Standard normal probability density φ(x).
#[must_use]
pub fn norm_pdf(x: f64) -> f64 {
    INV_SQRT_2PI * (-0.5 * x * x).exp()
}

/// Standard normal cumulative distribution Φ(x).
///
/// Relative error is about 1e-15 near the centre. In the lower tail it grows
/// like `x² · 1e-16` (about 1e-14 at x = -8, 5e-13 at x = -37) because the
/// rounding of `x / sqrt(2)` is amplified by the slope of `ln Φ`.
#[must_use]
pub fn norm_cdf(x: f64) -> f64 {
    0.5 * erfc(-x * std::f64::consts::FRAC_1_SQRT_2)
}

/// Complementary error function erfc(x) = 1 - erf(x).
#[must_use]
pub fn erfc(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    let y = x.abs();
    if y <= 0.468_75 {
        return 1.0 - erf_small(x);
    }
    let tail = if y <= 4.0 {
        exp_neg_sq(y) * erfc_mid(y)
    } else if y < ERFC_UNDERFLOW {
        exp_neg_sq(y) * erfc_large(y)
    } else {
        0.0
    };
    if x < 0.0 { 2.0 - tail } else { tail }
}

/// erf(x) for |x| <= 0.46875.
fn erf_small(x: f64) -> f64 {
    const A: [f64; 5] = [
        3.161_123_743_870_565_6,
        1.138_641_541_510_501_6e2,
        3.774_852_376_853_02e2,
        3.209_377_589_138_469_5e3,
        1.857_777_061_846_031_5e-1,
    ];
    const B: [f64; 4] = [
        2.360_129_095_234_412_1e1,
        2.440_246_379_344_441_7e2,
        1.282_616_526_077_372_3e3,
        2.844_236_833_439_171e3,
    ];
    let z = x * x;
    let mut num = A[4] * z;
    let mut den = z;
    for i in 0..3 {
        num = (num + A[i]) * z;
        den = (den + B[i]) * z;
    }
    x * (num + A[3]) / (den + B[3])
}

/// erfc(y) · exp(y²) for 0.46875 < y <= 4.
fn erfc_mid(y: f64) -> f64 {
    const C: [f64; 9] = [
        5.641_884_969_886_701e-1,
        8.883_149_794_388_376,
        6.611_919_063_714_163e1,
        2.986_351_381_974_001e2,
        8.819_522_212_417_69e2,
        1.712_047_612_634_070_6e3,
        2.051_078_377_826_071_5e3,
        1.230_339_354_797_997_2e3,
        2.153_115_354_744_038_5e-8,
    ];
    const D: [f64; 8] = [
        1.574_492_611_070_983_5e1,
        1.176_939_508_913_125e2,
        5.371_811_018_620_099e2,
        1.621_389_574_566_690_2e3,
        3.290_799_235_733_459_6e3,
        4.362_619_090_143_247e3,
        3.439_367_674_143_721_6e3,
        1.230_339_354_803_749_4e3,
    ];
    let mut num = C[8] * y;
    let mut den = y;
    for i in 0..7 {
        num = (num + C[i]) * y;
        den = (den + D[i]) * y;
    }
    (num + C[7]) / (den + D[7])
}

/// erfc(y) · exp(y²) for y > 4.
fn erfc_large(y: f64) -> f64 {
    const P: [f64; 6] = [
        3.053_266_349_612_323_4e-1,
        3.603_448_999_498_044_4e-1,
        1.257_817_261_112_292_5e-1,
        1.608_378_514_874_228e-2,
        6.587_491_615_298_378e-4,
        1.631_538_713_730_209_8e-2,
    ];
    const Q: [f64; 5] = [
        2.568_520_192_289_822,
        1.872_952_849_923_467_3,
        5.279_051_029_514_284e-1,
        6.051_834_131_244_132e-2,
        2.335_204_976_268_691_8e-3,
    ];
    let z = 1.0 / (y * y);
    let mut num = P[5] * z;
    let mut den = z;
    for i in 0..4 {
        num = (num + P[i]) * z;
        den = (den + Q[i]) * z;
    }
    let r = z * (num + P[4]) / (den + Q[4]);
    (INV_SQRT_PI - r) / y
}

/// exp(-y²) without the rounding error of squaring `y` directly: `y` is split
/// into a 4-bit-fraction head and a remainder, as in Cody's routine.
fn exp_neg_sq(y: f64) -> f64 {
    let head = (y * 16.0).trunc() / 16.0;
    let del = (y - head) * (y + head);
    (-head * head).exp() * (-del).exp()
}

#[cfg(test)]
// These tests compare exact values on purpose: identical code paths, exact
// limits such as 0 and 1, and values passed through unchanged.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    /// Relative difference, with an absolute floor for values near zero.
    fn rel(a: f64, b: f64) -> f64 {
        (a - b).abs() / b.abs().max(f64::MIN_POSITIVE)
    }

    // (x, Φ(x), φ(x)) from Python: 0.5 * math.erfc(-x / sqrt(2)) and
    // exp(-x² / 2) / sqrt(2π).
    const CDF_PDF: [(f64, f64, f64); 19] = [
        (
            -37.0,
            5.725_571_222_525_139_4e-300,
            2.120_006_551_524_606e-298,
        ),
        (
            -30.0,
            4.906_713_927_148_763_4e-198,
            1.473_646_134_878_547_6e-196,
        ),
        (-20.0, 2.753_624_118_606_332e-89, 5.520_948_362_159_764e-88),
        (-10.0, 7.619_853_024_160_595e-24, 7.694_598_626_706_42e-23),
        (-8.0, 6.220_960_574_271_819e-16, 5.052_271_083_536_893e-15),
        (-5.0, 2.866_515_718_791_945_5e-7, 1.486_719_514_734_297_9e-6),
        (
            -3.0,
            0.001_349_898_031_630_095_9,
            0.004_431_848_411_938_007_5,
        ),
        (-2.0, 0.022_750_131_948_179_216, 0.053_990_966_513_188_06),
        (-1.0, 0.158_655_253_931_457_07, 0.241_970_724_519_143_37),
        (-0.5, 0.308_537_538_725_986_94, 0.352_065_326_764_299_5),
        (-0.3, 0.382_088_577_811_047_4, 0.381_387_815_460_524_14),
        (0.0, 0.5, 0.398_942_280_401_432_7),
        (0.3, 0.617_911_422_188_952_6, 0.381_387_815_460_524_14),
        (0.5, 0.691_462_461_274_013, 0.352_065_326_764_299_5),
        (1.0, 0.841_344_746_068_542_8, 0.241_970_724_519_143_37),
        (2.0, 0.977_249_868_051_820_9, 0.053_990_966_513_188_06),
        (3.0, 0.998_650_101_968_369_9, 0.004_431_848_411_938_007_5),
        (5.0, 0.999_999_713_348_428_2, 1.486_719_514_734_297_9e-6),
        (8.0, 0.999_999_999_999_999_3, 5.052_271_083_536_893e-15),
    ];

    // (x, erfc(x)) from Python's math.erfc, covering every branch boundary.
    const ERFC: [(f64, f64); 15] = [
        (0.0, 1.0),
        (0.1, 0.887_537_083_981_715_2),
        (0.468_75, 0.507_386_526_782_062),
        (0.5, 0.479_500_122_186_953_4),
        (1.0, 0.157_299_207_050_285_1),
        (2.0, 0.004_677_734_981_047_265),
        (3.9, 3.479_224_859_723_176_5e-8),
        (4.0, 1.541_725_790_028_001_7e-8),
        (4.1, 6.700_027_654_084_917_5e-9),
        (6.0, 2.151_973_671_249_891_3e-17),
        (10.0, 2.088_487_583_762_545e-45),
        (26.0, 5.663_192_408_856_143e-296),
        (-0.2, 1.222_702_589_210_478_6),
        (-1.5, 1.966_105_146_475_310_8),
        (-5.0, 1.999_999_999_998_462_6),
    ];

    #[test]
    fn erfc_matches_reference() {
        for (x, want) in ERFC {
            assert!(
                rel(erfc(x), want) < 1e-14,
                "erfc({x}) = {} want {want}",
                erfc(x)
            );
        }
    }

    #[test]
    fn cdf_and_pdf_match_reference() {
        for (x, cdf, pdf) in CDF_PDF {
            // Rounding of x/√2 differs from the reference; its effect grows as x².
            let tol = 4e-16 * x.mul_add(x, 1.0);
            assert!(
                rel(norm_cdf(x), cdf) < tol,
                "cdf({x}) = {} want {cdf}",
                norm_cdf(x)
            );
            assert!(
                rel(norm_pdf(x), pdf) < 1e-14,
                "pdf({x}) = {} want {pdf}",
                norm_pdf(x)
            );
        }
    }

    #[test]
    fn cdf_limits_and_symmetry() {
        assert_eq!(norm_cdf(-40.0), 0.0);
        assert_eq!(norm_cdf(40.0), 1.0);
        assert_eq!(norm_cdf(f64::NEG_INFINITY), 0.0);
        assert_eq!(norm_cdf(f64::INFINITY), 1.0);
        assert!(norm_cdf(f64::NAN).is_nan());
        for i in -60..=60 {
            let x = f64::from(i) * 0.1;
            assert!((norm_cdf(x) + norm_cdf(-x) - 1.0).abs() < 2e-16);
        }
    }

    #[test]
    fn cdf_is_monotone_across_branch_boundaries() {
        let mut prev = 0.0;
        for i in -12_000..=12_000 {
            let x = f64::from(i) * 1e-3;
            let p = norm_cdf(x);
            assert!(p >= prev, "cdf not monotone at {x}");
            prev = p;
        }
    }
}
