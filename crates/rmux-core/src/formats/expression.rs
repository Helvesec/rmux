use super::{format_choose, ExpandState, FormatModifier, FormatVariables};

pub(super) fn format_expression<V>(
    state: &mut ExpandState,
    body: &str,
    modifier: &FormatModifier,
    variables: &V,
) -> String
where
    V: FormatVariables + ?Sized,
{
    let Some(operator) = modifier.argv.first().map(String::as_str) else {
        return String::new();
    };
    let float_precision = expression_float_precision(modifier);
    let Some((left, right)) = format_choose(state, body, variables) else {
        return String::new();
    };

    if is_comparison_operator(operator) {
        return numeric_compare(operator, &left, &right)
            .map(bool_string)
            .unwrap_or_default();
    }

    if let Some(precision) = float_precision {
        let Some(value) = numeric_operation(operator, &left, &right) else {
            return String::new();
        };
        format_float_value(value, precision)
    } else {
        let Some(value) = integer_operation(operator, &left, &right) else {
            return String::new();
        };
        value
    }
}

fn numeric_operation(operator: &str, left: &str, right: &str) -> Option<f64> {
    let left = parse_number(left)?;
    let right = parse_number(right)?;
    Some(match operator {
        "+" => left + right,
        "-" => left - right,
        "*" => left * right,
        "/" => left / right,
        // Oracle probe 2026-07-08: '%' is not a tmux e-operator (renders
        // empty); only 'm' is. Float modulo by zero renders "nan".
        "m" => {
            if right == 0.0 {
                f64::NAN
            } else {
                left % right
            }
        }
        _ => return None,
    })
}

fn integer_operation(operator: &str, left: &str, right: &str) -> Option<String> {
    if !matches!(operator, "+" | "-" | "*" | "/" | "m") {
        return None;
    }

    let left = arithmetic_operand(left)?;
    let right = arithmetic_operand(right)?;
    let value = match operator {
        "+" => left + right,
        "-" => left - right,
        "*" => left * right,
        "/" => left / right,
        // Oracle probe 2026-07-08: tmux 3.7b renders integer modulo by zero
        // as 0 (division by zero saturates instead).
        "m" => {
            if right == 0.0 {
                0.0
            } else {
                left % right
            }
        }
        _ => return None,
    };
    Some(integer_result(value))
}

fn numeric_compare(operator: &str, left: &str, right: &str) -> Option<bool> {
    let left = comparison_operand(left)?;
    let right = comparison_operand(right)?;
    Some(match operator {
        "==" => left == right,
        "!=" => left != right,
        ">" => left > right,
        ">=" => left >= right,
        "<" => left < right,
        "<=" => left <= right,
        _ => return None,
    })
}

fn arithmetic_operand(value: &str) -> Option<f64> {
    // The pinned tmux 3.7b oracle (arm64) rounds operands through a
    // saturating long long cast: NaN -> 0, +/-inf and out-of-range values
    // saturate (AArch64 fcvtzs semantics). Rust `as` casts match exactly.
    let value = parse_number(value)?;
    Some((value as i64) as f64)
}

fn comparison_operand(value: &str) -> Option<f64> {
    let value = parse_number(value)?;
    Some((value as i64) as f64)
}

fn parse_number(value: &str) -> Option<f64> {
    let value = value.trim();
    if value.is_empty() {
        return Some(0.0);
    }
    if value.eq_ignore_ascii_case("nan") {
        return Some(f64::NAN);
    }
    if value.eq_ignore_ascii_case("inf") || value.eq_ignore_ascii_case("+inf") {
        return Some(f64::INFINITY);
    }
    if value.eq_ignore_ascii_case("-inf") {
        return Some(f64::NEG_INFINITY);
    }
    value
        .parse::<f64>()
        .ok()
        .or_else(|| parse_prefixed_integer(value).map(|integer| integer as f64))
}

fn parse_prefixed_integer(value: &str) -> Option<i64> {
    let (negative, digits) = match value.as_bytes().first().copied() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let (base, digits) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
        .map(|digits| (16, digits))?;
    if digits.is_empty() {
        return None;
    }
    let unsigned = i128::from_str_radix(digits, base).ok()?;
    let signed = if negative { -unsigned } else { unsigned };
    Some(signed.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
}

fn integer_result(value: f64) -> String {
    // The oracle prints the result after the same saturating long long
    // round-trip as the operands, so e.g. an overflowing multiply renders
    // as 9223372036854775808 ((double)LLONG_MAX), not as a MIN sentinel.
    format!("{:.0}", (value as i64) as f64)
}

fn is_comparison_operator(operator: &str) -> bool {
    matches!(operator, "==" | "!=" | ">" | ">=" | "<" | "<=")
}

fn expression_float_precision(modifier: &FormatModifier) -> Option<usize> {
    let options = modifier.argv.get(1).map(String::as_str).unwrap_or_default();
    if !options.contains('f') {
        return None;
    }
    Some(
        modifier
            .argv
            .get(2)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(2),
    )
}

fn bool_string(value: bool) -> String {
    if value {
        "1".to_owned()
    } else {
        "0".to_owned()
    }
}

fn format_float_value(value: f64, precision: usize) -> String {
    if value.is_nan() {
        // The pinned tmux 3.7b oracle (arm64 libc) prints NaN unsigned.
        return "nan".to_owned();
    }
    format!("{value:.precision$}")
}
