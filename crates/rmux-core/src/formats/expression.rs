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
        format!("{value:.precision$}")
    } else {
        let Some(value) = integer_operation(operator, &left, &right) else {
            return String::new();
        };
        value.to_string()
    }
}

fn numeric_operation(operator: &str, left: &str, right: &str) -> Option<f64> {
    let left = parse_number(left)?;
    let right = parse_number(right)?;
    Some(match operator {
        "+" => left + right,
        "-" => left - right,
        "*" => left * right,
        "/" if right == 0.0 => return None,
        "/" => left / right,
        "%" if right == 0.0 => return None,
        "%" => left % right,
        "m" if right == 0.0 => return None,
        "m" => left % right,
        _ => return None,
    })
}

fn integer_operation(operator: &str, left: &str, right: &str) -> Option<i64> {
    let left = parse_integer(left)?;
    let right = parse_integer(right)?;
    Some(match operator {
        "+" => left.saturating_add(right),
        "-" => left.saturating_sub(right),
        "*" => left.saturating_mul(right),
        "/" | "%" | "m" if right == 0 => return None,
        "/" => left.checked_div(right).unwrap_or(i64::MAX),
        "%" | "m" => left.checked_rem(right).unwrap_or(0),
        _ => return None,
    })
}

fn numeric_compare(operator: &str, left: &str, right: &str) -> Option<bool> {
    let left = parse_number(left)?;
    let right = parse_number(right)?;
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

fn parse_number(value: &str) -> Option<f64> {
    value.trim().parse::<f64>().ok()
}

fn parse_integer(value: &str) -> Option<i64> {
    parse_number(value).map(integer_result)
}

fn integer_result(value: f64) -> i64 {
    if !value.is_finite() {
        return 0;
    }
    if value >= i64::MAX as f64 {
        return i64::MAX;
    }
    if value <= i64::MIN as f64 {
        return i64::MIN;
    }
    value as i64
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
