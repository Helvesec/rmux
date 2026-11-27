pub fn source_match_budget_exceeded(messages: &str) -> bool {
    // The source engine reports cumulative and single-glob limits separately.
    messages.lines().any(|line| {
        line.contains("source-file command exceeds 256 matched files or ")
            || (line.contains("source-file glob '")
                && line.ends_with("' matched too many files (maximum 256)"))
    })
}
