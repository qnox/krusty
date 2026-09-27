//! Kotlin/JVM's `Float.toString` and `Double.toString` text for constants the compiler renders
//! itself: a `const val` template or a folded string concatenation.

use super::KtStringBuf;

pub(crate) fn push_f32(value: f32, output: &mut KtStringBuf) -> Option<()> {
    if value.is_nan() {
        output.push_str("NaN");
    } else if value == f32::INFINITY {
        output.push_str("Infinity");
    } else if value == f32::NEG_INFINITY {
        output.push_str("-Infinity");
    } else if value == 0.0 {
        output.push_str(if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        });
    } else {
        let magnitude = value.abs();
        let decimal = select_float_decimal(
            9,
            |fractional_digits| format!("{:.*e}", fractional_digits, magnitude),
            |candidate| candidate.parse::<f32>().ok() == Some(magnitude),
        )?;
        push_selected_float_decimal(value.is_sign_negative(), decimal, output);
    }
    Some(())
}

pub(crate) fn push_f64(value: f64, output: &mut KtStringBuf) -> Option<()> {
    if value.is_nan() {
        output.push_str("NaN");
    } else if value == f64::INFINITY {
        output.push_str("Infinity");
    } else if value == f64::NEG_INFINITY {
        output.push_str("-Infinity");
    } else if value == 0.0 {
        output.push_str(if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        });
    } else {
        let magnitude = value.abs();
        let decimal = select_float_decimal(
            17,
            |fractional_digits| format!("{:.*e}", fractional_digits, magnitude),
            |candidate| candidate.parse::<f64>().ok() == Some(magnitude),
        )?;
        push_selected_float_decimal(value.is_sign_negative(), decimal, output);
    }
    Some(())
}

/// The decimal selected by Kotlin/JVM's `Float.toString`/`Double.toString` contract.
///
/// `scientific_exponent` is the exponent when `digits` is written with one digit before the point.
struct SelectedFloatDecimal {
    digits: String,
    scientific_exponent: i32,
}

/// Select the language-defined decimal without tying constant evaluation to a host JVM.
///
/// The Java/Kotlin contract first finds the minimum significant-digit count whose decimal rounds
/// back to the binary value, then chooses the closest such decimal (ties to an even significand).
/// When that minimum is one digit, the closest one- *or two*-digit decimal is chosen. Rust's
/// precision-controlled scientific formatter supplies exactly that nearest, ties-to-even decimal;
/// parsing it back proves membership in the binary value's rounding interval. The search is
/// bounded by the IEEE-754 maximum round-trip widths (9 for `f32`, 17 for `f64`).
fn select_float_decimal(
    max_digits: usize,
    mut render: impl FnMut(usize) -> String,
    mut roundtrips: impl FnMut(&str) -> bool,
) -> Option<SelectedFloatDecimal> {
    let minimum_digits = (1..=max_digits).find(|digits| {
        let candidate = render(*digits - 1);
        roundtrips(&candidate)
    })?;
    let selected_digits = if minimum_digits == 1 {
        2
    } else {
        minimum_digits
    };
    parse_selected_float_decimal(&render(selected_digits - 1))
}

fn parse_selected_float_decimal(rendered: &str) -> Option<SelectedFloatDecimal> {
    let (significand, exponent) = rendered.split_once(['e', 'E'])?;
    let scientific_exponent = exponent.parse().ok()?;
    let mut digits: String = significand
        .chars()
        .filter(|character| *character != '.')
        .collect();
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    Some(SelectedFloatDecimal {
        digits,
        scientific_exponent,
    })
}

fn push_selected_float_decimal(
    negative: bool,
    decimal: SelectedFloatDecimal,
    output: &mut KtStringBuf,
) {
    if negative {
        output.push_str("-");
    }
    let SelectedFloatDecimal {
        digits,
        scientific_exponent,
    } = decimal;
    if (-3..0).contains(&scientific_exponent) {
        output.push_str("0.");
        for _ in 0..(-scientific_exponent - 1) {
            output.push_str("0");
        }
        output.push_str(&digits);
    } else if (0..7).contains(&scientific_exponent) {
        let integer_digits = scientific_exponent as usize + 1;
        if integer_digits < digits.len() {
            output.push_str(&digits[..integer_digits]);
            output.push_str(".");
            output.push_str(&digits[integer_digits..]);
        } else {
            output.push_str(&digits);
            for _ in digits.len()..integer_digits {
                output.push_str("0");
            }
            output.push_str(".0");
        }
    } else {
        output.push_str(&digits[..1]);
        output.push_str(".");
        output.push_str(if digits.len() == 1 { "0" } else { &digits[1..] });
        output.push_str("E");
        output.push_str(&scientific_exponent.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered_f32(value: f32) -> String {
        let mut output = KtStringBuf::new();
        push_f32(value, &mut output).unwrap();
        output.finish().to_lossy()
    }

    fn rendered_f64(value: f64) -> String {
        let mut output = KtStringBuf::new();
        push_f64(value, &mut output).unwrap();
        output.finish().to_lossy()
    }

    #[test]
    fn floating_constant_strings_follow_kotlin_decimal_selection() {
        assert_eq!(rendered_f32(0.0), "0.0");
        assert_eq!(rendered_f32(-0.0), "-0.0");
        assert_eq!(rendered_f32(1.0e7), "1.0E7");
        assert_eq!(rendered_f32(1.0e-4), "1.0E-4");
        assert_eq!(rendered_f32(f32::from_bits(1)), "1.4E-45");
        assert_eq!(rendered_f64(1.0e7), "1.0E7");
        assert_eq!(rendered_f64(1.0e-4), "1.0E-4");
        assert_eq!(rendered_f64(f64::from_bits(1)), "4.9E-324");
        assert_eq!(rendered_f64(f64::INFINITY), "Infinity");
        assert_eq!(rendered_f64(f64::NEG_INFINITY), "-Infinity");
        assert_eq!(rendered_f64(f64::NAN), "NaN");
    }
}
