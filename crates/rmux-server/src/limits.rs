/// Maximum repeat count accepted for user-supplied repeated key/copy-mode work.
pub(crate) const MAX_COMMAND_REPEAT_COUNT: usize = 1000;

/// Normalizes an optional repeat count into the bounded execution budget.
pub(crate) fn bounded_repeat_count(count: Option<usize>) -> usize {
    clamp_repeat_count(count.unwrap_or(1))
}

/// Clamps an already parsed repeat count into the bounded execution budget.
pub(crate) fn clamp_repeat_count(count: usize) -> usize {
    count.clamp(1, MAX_COMMAND_REPEAT_COUNT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_count_is_bounded() {
        assert_eq!(bounded_repeat_count(None), 1);
        assert_eq!(bounded_repeat_count(Some(0)), 1);
        assert_eq!(bounded_repeat_count(Some(42)), 42);
        assert_eq!(
            bounded_repeat_count(Some(MAX_COMMAND_REPEAT_COUNT + 1)),
            MAX_COMMAND_REPEAT_COUNT
        );
    }
}
