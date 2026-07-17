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

/// tmux 3.7 evaluates each top-level comma-separated boolean operand.
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
        return "0".to_owned();
    }

    let mut rest = body;
    loop {
        let (raw, next) = match format_skip(rest.as_bytes(), b",") {
            Some(split) => (&rest[..split], Some(&rest[split + 1..])),
            None => (rest, None),
        };
        let truthy = is_truthy(&format_expand1(state, raw, variables));
        if and && !truthy {
            return "0".to_owned();
        }
        if !and && truthy {
            return "1".to_owned();
        }
        let Some(next) = next else {
            break;
        };
        rest = next;
    }

    if and { "1" } else { "0" }.to_owned()
}
