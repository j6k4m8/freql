//! Decimal presentation of exact report values; never used for computation.
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{Signed, Zero};

use crate::types::parse_rational;

/// Round to six decimal places, with ties away from zero. Keep exact integers
/// and short decimals unmarked; use ≈ only when the displayed number changes.
pub(crate) fn approximate_value(value: &str) -> Option<String> {
    let (number, unit) = value.split_once(' ').unwrap_or((value, ""));
    let exact = if let Some((numerator, denominator)) = number.split_once('/') {
        let numerator = numerator.parse::<BigInt>().ok()?;
        let denominator = denominator.parse::<BigInt>().ok()?;
        if denominator.is_zero() {
            return None;
        }
        BigRational::new(numerator, denominator)
    } else {
        parse_rational(number)?
    };
    let scale = BigInt::from(1_000_000_u32);
    let numerator = exact.numer().abs() * &scale;
    let mut rounded = &numerator / exact.denom();
    if (&numerator % exact.denom()) * 2 >= *exact.denom() {
        rounded += 1;
    }
    if exact.is_negative() {
        rounded = -rounded;
    }
    let changed = BigRational::new(rounded.clone(), scale) != exact;
    let digits = rounded.abs().to_string();
    let padded = format!("{digits:0>7}");
    let split = padded.len() - 6;
    let fraction = padded[split..].trim_end_matches('0');
    let number = if fraction.is_empty() {
        padded[..split].to_owned()
    } else {
        format!("{}.{}", &padded[..split], fraction)
    };
    Some(format!(
        "{}{}{}{}{}",
        if changed { "≈" } else { "" },
        if rounded.is_negative() && !rounded.is_zero() {
            "-"
        } else {
            ""
        },
        number,
        if unit.is_empty() { "" } else { " " },
        unit,
    ))
}

#[cfg(test)]
mod tests {
    use super::approximate_value;

    #[test]
    fn decimal_rounding_is_exact_signed_and_marks_only_changed_values() {
        for (source, expected) in [
            ("2/3 Hour", "≈0.666667 Hour"),
            ("-2/3 Hour", "≈-0.666667 Hour"),
            ("30 Minute", "30 Minute"),
            ("1/8 Hour", "0.125 Hour"),
            ("0.123456 Hour", "0.123456 Hour"),
            ("0.1234565 Hour", "≈0.123457 Hour"),
            ("-0.1234565 Hour", "≈-0.123457 Hour"),
            ("0.9999995", "≈1"),
            ("-0.0000001", "≈0"),
            ("0.0000005", "≈0.000001"),
            (
                "1000000000000000000000000000000.1234567 Byte",
                "≈1000000000000000000000000000000.123457 Byte",
            ),
            ("2/3 Meter / Second", "≈0.666667 Meter / Second"),
        ] {
            assert_eq!(
                approximate_value(source).as_deref(),
                Some(expected),
                "{source}"
            );
        }
        assert!(approximate_value("makespan(x, y)").is_none());
    }
}
