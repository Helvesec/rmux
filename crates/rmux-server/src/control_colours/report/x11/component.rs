//! Numeric prefixes used by the bounded X11 CMY/CMYK report grammar.

pub(super) fn parse(input: &mut &str) -> Option<f64> {
    *input = input.trim_start_matches(super::c_space);
    let unsigned = input.strip_prefix(['+', '-']).unwrap_or(input);
    let value = if unsigned.starts_with("0x") || unsigned.starts_with("0X") {
        hexadecimal(input).or_else(|| decimal(input))?
    } else {
        decimal(input)?
    };
    (value.is_finite() && (0.0..=1.0).contains(&value)).then_some(value)
}

fn decimal(input: &mut &str) -> Option<f64> {
    // Preserve sscanf's accepted numeric prefix of the last component.
    let end = input
        .bytes()
        .take_while(|b| b.is_ascii_digit() || b"+-.eE".contains(b))
        .count();
    for length in (1..=end).rev() {
        if let Ok(value) = input[..length].parse::<f64>() {
            *input = &input[length..];
            return Some(value);
        }
    }
    None
}

fn hexadecimal(input: &mut &str) -> Option<f64> {
    let bytes = input.as_bytes();
    let negative = bytes.first() == Some(&b'-');
    let mut at = usize::from(matches!(bytes.first(), Some(b'+' | b'-'))) + 2;
    let mut significand = 0u64;
    let mut retained_digits = 0;
    let mut discarded_digits = 0;
    let mut sticky = false;
    let mut digits = 0;
    let mut fractional_digits = 0;
    let mut point = false;
    while let Some(&byte) = bytes.get(at) {
        if byte == b'.' && !point {
            point = true;
        } else if let Some(digit) = char::from(byte).to_digit(16) {
            // Keep 57..60 significant bits and a sticky bit. Round only after
            // the exponent determines the final normal/subnormal precision.
            if retained_digits == 15 {
                discarded_digits += 1;
                sticky |= digit != 0;
            } else if significand != 0 || digit != 0 {
                significand = (significand << 4) | u64::from(digit);
                retained_digits += 1;
            }
            digits += 1;
            fractional_digits += i32::from(point);
        } else {
            break;
        }
        at += 1;
    }
    if digits == 0 {
        return None;
    }
    let mut exponent = 0i32;
    if matches!(bytes.get(at), Some(b'p' | b'P')) {
        let mut end = at + 1;
        let negative_exponent = bytes.get(end) == Some(&b'-');
        end += usize::from(matches!(bytes.get(end), Some(b'+' | b'-')));
        let start = end;
        while let Some(digit) = bytes.get(end).filter(|b| b.is_ascii_digit()) {
            exponent = exponent
                .saturating_mul(10)
                .saturating_add(i32::from(digit - b'0'));
            end += 1;
        }
        if end != start {
            at = end;
            if negative_exponent {
                exponent = -exponent;
            }
        }
    }
    exponent = exponent.saturating_add(4 * (discarded_digits - fractional_digits));
    let value = binary_value(significand, exponent, sticky);
    *input = &input[at..];
    Some(if negative { -value } else { value })
}

/// Convert the retained integer times 2^exponent directly to binary64 bits.
/// Subnormals round to multiples of 2^-1074, not to 53 significant bits first.
fn binary_value(significand: u64, exponent: i32, sticky: bool) -> f64 {
    if significand == 0 {
        return 0.0;
    }
    let bits = u64::BITS - significand.leading_zeros();
    let mut top = exponent.saturating_add(bits as i32 - 1);
    if top < -1022 {
        let shift = (-1074i32).saturating_sub(exponent);
        let units = if shift > 0 {
            round_right(significand, shift as u32, sticky)
        } else {
            significand << -shift
        };
        // A carry into bit 52 is exactly the smallest normal value.
        return f64::from_bits(units);
    }
    if top > 1023 {
        return f64::INFINITY;
    }
    let mut rounded = if bits > 53 {
        round_right(significand, bits - 53, sticky)
    } else {
        significand << (53 - bits)
    };
    if rounded == 1 << 53 {
        rounded >>= 1;
        top += 1;
    }
    // A carry past exponent 1023 encodes infinity (with a zero fraction).
    f64::from_bits(((top + 1023) as u64) << 52 | (rounded & ((1 << 52) - 1)))
}

fn round_right(value: u64, shift: u32, sticky: bool) -> u64 {
    // The bounded scanner retains at most 60 bits, so these shifts underflow
    // below half a unit even when discarded digits were nonzero.
    if shift >= 64 {
        return 0;
    }
    let whole = value >> shift;
    let remainder = value & ((1 << shift) - 1);
    let half = 1 << (shift - 1);
    whole + u64::from(remainder > half || (remainder == half && (sticky || whole & 1 != 0)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn hexadecimal_subnormals_round_once_before_checking_the_domain() {
        for (unsigned, bits) in [
            ("0x0p99999999999999999999", 0),
            ("0x1p-99999999999999999999", 0),
            ("0x1p-1076", 0),
            ("0x1p-1075", 0),
            ("0x1.0000000000000001p-1075", 1),
            ("0x1.00000000000008p-1075", 1),
            ("0x1p-1074", 1),
            ("0x3p-1075", 2),
            ("0x5p-1075", 2),
            ("0x5.0000000000000001p-1075", 3),
            ("0x0.fffffffffffffp-1022", 0x000fffffffffffff),
            ("0x0.fffffffffffff7ffffffffffffp-1022", 0x000fffffffffffff),
            ("0x0.fffffffffffff8p-1022", 0x0010000000000000),
            ("0x1p-1022", 0x0010000000000000),
        ] {
            for negative in [false, true] {
                let text = format!("{}{unsigned}", if negative { "-" } else { "" });
                let expected_bits = bits | (u64::from(negative) << 63);
                let mut rest = text.as_str();
                assert_eq!(
                    super::hexadecimal(&mut rest).unwrap().to_bits(),
                    expected_bits,
                    "{text}"
                );
                assert!(rest.is_empty());
                let mut rest = text.as_str();
                let expected = if negative && bits != 0 {
                    None
                } else {
                    Some(expected_bits)
                };
                assert_eq!(
                    super::parse(&mut rest).map(f64::to_bits),
                    expected,
                    "{text}"
                );
            }
        }
    }

    #[test]
    fn hex_rounding_and_extreme_exponents_keep_the_component_range() {
        for (input, expected) in [
            ("0x1.00000000000008000000000000000000p-1", Some(0.5)),
            (
                "0x1.00000000000008000000000000000001p-1",
                Some(f64::from_bits(0x3fe0000000000001)),
            ),
            ("0x1.00000000000008000000000000000001", None),
            ("0x0p99999999999999999999", Some(0.0)),
            ("0x1p-99999999999999999999", Some(0.0)),
            ("0x1p99999999999999999999", None),
        ] {
            let mut rest = input;
            assert_eq!(super::parse(&mut rest), expected, "{input}");
        }
    }
}
