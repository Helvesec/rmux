use super::{
    format_expand1, format_skip, is_truthy, resolve_variable, ExpandState, FormatVariables,
    FORMAT_LOOP_LIMIT,
};

/// Evaluates a conditional: `condition,true-value,false-value`.
pub(super) fn format_conditional<V>(state: &mut ExpandState, body: &str, variables: &V) -> String
where
    V: FormatVariables + ?Sized,
{
    match split_conditional_body(body) {
        ConditionalParts::Full {
            condition,
            true_body,
            false_body,
        } => render_conditional_parts(state, condition, true_body, false_body, variables),
        ConditionalParts::MissingFalse | ConditionalParts::MissingTrue => {
            state.stop_expansion = true;
            String::new()
        }
    }
}

fn render_conditional_parts<V>(
    state: &mut ExpandState,
    condition_raw: &str,
    true_body: &str,
    false_body: &str,
    variables: &V,
) -> String
where
    V: FormatVariables + ?Sized,
{
    let mut condition_raw = condition_raw;
    let mut true_body = true_body;
    let mut false_body = false_body;
    let mut chained_conditions = 0;

    loop {
        let condition = condition_value(state, condition_raw, variables);
        if is_truthy(&condition) {
            return expand_conditional_branch(state, true_body, variables);
        }

        if !conditional_chain_name(false_body).is_some_and(|name| name.ends_with("_format")) {
            return expand_conditional_branch(state, false_body, variables);
        }

        let ConditionalParts::Full {
            condition: next_condition,
            true_body: next_true,
            false_body: next_false,
        } = split_conditional_body(false_body)
        else {
            return expand_conditional_branch(state, false_body, variables);
        };

        chained_conditions += 1;
        if chained_conditions >= FORMAT_LOOP_LIMIT {
            return String::new();
        }

        condition_raw = next_condition;
        true_body = next_true;
        false_body = next_false;
    }
}

fn expand_conditional_branch<V>(state: &ExpandState, body: &str, variables: &V) -> String
where
    V: FormatVariables + ?Sized,
{
    let mut nested_state = ExpandState {
        loop_depth: state.loop_depth,
        expand_time: state.expand_time,
        stop_expansion: false,
        preserve_jobs: state.preserve_jobs,
    };
    format_expand1(&mut nested_state, body, variables)
}

enum ConditionalParts<'a> {
    Full {
        condition: &'a str,
        true_body: &'a str,
        false_body: &'a str,
    },
    MissingFalse,
    MissingTrue,
}

fn split_conditional_body(body: &str) -> ConditionalParts<'_> {
    let bytes = body.as_bytes();
    let Some(condition_end) = format_skip(bytes, b",") else {
        return ConditionalParts::MissingTrue;
    };
    let true_start = condition_end + 1;
    let Some(true_len) = format_skip(&bytes[true_start..], b",") else {
        return ConditionalParts::MissingFalse;
    };
    let true_end = true_start + true_len;
    ConditionalParts::Full {
        condition: &body[..condition_end],
        true_body: &body[true_start..true_end],
        false_body: &body[true_end + 1..],
    }
}

fn conditional_chain_name(body: &str) -> Option<&str> {
    let condition_end = format_skip(body.as_bytes(), b",")?;
    Some(&body[..condition_end])
}

fn condition_value<V>(state: &ExpandState, raw: &str, variables: &V) -> String
where
    V: FormatVariables + ?Sized,
{
    let found = resolve_variable(raw, variables);
    if !found.is_empty() {
        return found;
    }

    let mut nested_state = ExpandState {
        loop_depth: state.loop_depth,
        expand_time: state.expand_time,
        stop_expansion: false,
        preserve_jobs: state.preserve_jobs,
    };
    let expanded = format_expand1(&mut nested_state, raw, variables);
    if expanded == raw {
        String::new()
    } else {
        expanded
    }
}

/// tmux 3.4 treats boolean operators as binary. Extra comma-separated text
/// remains part of the right operand, even when newer tmux releases evaluate
/// every operand independently.
pub(super) fn format_bool_op<V>(
    state: &mut ExpandState,
    body: &str,
    and: bool,
    variables: &V,
) -> String
where
    V: FormatVariables + ?Sized,
{
    if body.is_empty() {
        return String::new();
    }

    let Some(split) = format_skip(body.as_bytes(), b",") else {
        return String::new();
    };
    let left = format_expand1(state, &body[..split], variables);
    let right = format_expand1(state, &body[split + 1..], variables);
    let result = if and {
        is_truthy(&left) && is_truthy(&right)
    } else {
        is_truthy(&left) || is_truthy(&right)
    };

    if result { "1" } else { "0" }.to_owned()
}
