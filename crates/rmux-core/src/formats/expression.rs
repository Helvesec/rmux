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
    let Some((left, right)) = format_choose(state, body, variables) else {
        return String::new();
    };

    if is_comparison_operator(operator) {
        return numeric_compare(operator, &left, &right)
            .map(bool_string)
            .unwrap_or_default();
    }

    match expression_float_precision(modifier) {
        ExpressionFloatPrecision::Valid(precision) => {
            let Some(value) = numeric_operation(operator, &left, &right) else {
                return String::new();
            };
            format_float_value(value, precision)
        }
        ExpressionFloatPrecision::Invalid => String::new(),
        ExpressionFloatPrecision::Disabled => {
            let Some(value) = integer_operation(operator, &left, &right) else {
                return String::new();
            };
            value
        }
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
        "m" | "%" | "%%" => {
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
    if !matches!(operator, "+" | "-" | "*" | "/" | "m" | "%" | "%%") {
        return None;
    }

    let left = arithmetic_operand(left)?;
    let right = arithmetic_operand(right)?;
    let value = match operator {
        "+" => left + right,
        "-" => left - right,
        "*" => left * right,
        "/" => left / right,
        // Both spellings reach the operator on glibc; on the darwin oracle
        // BSD strftime consumes a lone '%' upstream and doubles '%%' into
        // '%', so accepting both matches the Linux oracle end-to-end and the
        // darwin oracle's post-strftime behavior.
        "m" | "%" | "%%" => left % right,
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
    let value = parse_number(value)?;
    if value.is_finite() && value > i64::MIN as f64 && value < i64::MAX as f64 {
        return Some((value as i64) as f64);
    }
    Some(i64::MIN as f64)
}

fn comparison_operand(value: &str) -> Option<f64> {
    arithmetic_operand(value)
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
    if value.is_nan()
        || value == f64::INFINITY
        || value == f64::NEG_INFINITY
        || value >= i64::MAX as f64
        || value < i64::MIN as f64
    {
        return i64::MIN.to_string();
    }
    (value as i64).to_string()
}

fn is_comparison_operator(operator: &str) -> bool {
    matches!(operator, "==" | "!=" | ">" | ">=" | "<" | "<=")
}

const MIN_EXPRESSION_FLOAT_PRECISION: i64 = -100;
const MAX_EXPRESSION_FLOAT_PRECISION: i64 = 100;
const DEFAULT_NEGATIVE_FLOAT_PRECISION: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpressionFloatPrecision {
    Disabled,
    Valid(usize),
    Invalid,
}

fn expression_float_precision(modifier: &FormatModifier) -> ExpressionFloatPrecision {
    let options = modifier.argv.get(1).map(String::as_str).unwrap_or_default();
    if !options.contains('f') {
        return ExpressionFloatPrecision::Disabled;
    }
    let Some(raw_precision) = modifier.argv.get(2) else {
        return ExpressionFloatPrecision::Valid(2);
    };
    if raw_precision.is_empty() {
        return ExpressionFloatPrecision::Valid(2);
    }
    let Ok(precision) = raw_precision.parse::<i64>() else {
        return ExpressionFloatPrecision::Invalid;
    };
    if precision < MIN_EXPRESSION_FLOAT_PRECISION {
        return ExpressionFloatPrecision::Invalid;
    }
    if precision < 0 {
        return ExpressionFloatPrecision::Valid(DEFAULT_NEGATIVE_FLOAT_PRECISION);
    }
    if precision > MAX_EXPRESSION_FLOAT_PRECISION {
        return ExpressionFloatPrecision::Invalid;
    }
    ExpressionFloatPrecision::Valid(precision as usize)
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
        // All NaN results render as "-nan": the Linux x86_64 deployment
        // oracle prints "-nan" (glibc + SSE quiet-NaN sign), and normalizing
        // every NaN producer (modulo-by-zero, inf-inf, 0/0, ...) keeps RMUX
        // output identical across CPUs, unlike raw hardware NaN signs.
        return "-nan".to_owned();
    }
    format!("{value:.precision$}")
}
