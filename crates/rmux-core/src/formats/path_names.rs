//! `basename(3)` and `dirname(3)` for the `#{b:…}` and `#{d:…}` modifiers.

/// The short name of a path, the way `basename(3)` gives it.
///
/// Reading the suffix after the last slash answered the empty string for a
/// path that ends in one, so `#{b:pane_title}` found nothing for a pane titled
/// `/work/editor/`. The trailing separators are closed off first: `/work/editor/`
/// and `/work/editor///` both give `editor`, `editor///` gives `editor`, and a
/// path made only of separators gives `/` - measured on tmux 3.7b, which calls
/// `basename(3)` here.
///
/// A present empty value gives `.`, as measured on tmux 3.7b. An absent value
/// never reaches this helper: the path lookup preserves that absence.
/// The separator is `/` and nothing else; no backslash, no `std::path`, no rule
/// that depends on the operating system.
pub(super) fn format_basename(value: &str) -> String {
    if value.is_empty() {
        return ".".to_owned();
    }
    if value.chars().all(|character| character == '/') {
        return "/".to_owned();
    }

    let trimmed = value.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(position) => trimmed[position + 1..].to_owned(),
        None => trimmed.to_owned(),
    }
}

/// The folder a path is filed under: everything before its last component,
/// without the separators that close it. `/work/editor///main.rs` and
/// `/work/editor/main.rs` both give `/work/editor`, and so does
/// `/work/editor///main.rs///`, since the separators closing the path itself
/// are not the folder's either. Only the run that CLOSES the folder goes:
/// `/work//editor///main.rs` keeps its interior pair and gives `/work//editor`.
/// A folder that is nothing but separators is the root, so `/main.rs` and
/// `///main.rs` both give `/` - measured on tmux 3.7b, which calls `dirname(3)`
/// here.
///
/// An input made ONLY of separators is the root as well: `///` gives `/`, the
/// answer the engine gave when that case was measured, and a single `/` is
/// already the root. A value of exactly two separators is left as it was: no
/// engine reading stands behind it, so nothing here claims one.
///
/// A value that holds nothing is filed under the current folder, `.`, which is
/// what the engine answered for an option really set to the empty string. That
/// is a value, not an absence: a name nobody answered never reaches this
/// helper at all, because the lookup that feeds it keeps absence, and its
/// answer stays the empty string. What this crate answers for an input with no
/// separator at all is unchanged. The separator is `/` and nothing else; no
/// backslash, no `std::path`, no rule that depends on the operating system, and
/// no normalisation of the rest of the path.
pub(super) fn format_dirname(value: &str) -> String {
    if value.is_empty() {
        return ".".to_owned();
    }
    if value.chars().all(|character| character == '/') {
        // Every character is a separator, so the length is the count of them:
        // three or more are a run that names the root, two are left untouched,
        // and one is already the root.
        return if value.len() >= 3 {
            "/".to_owned()
        } else {
            value.to_owned()
        };
    }

    let trimmed = value.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(position) => {
            let parent = trimmed[..position].trim_end_matches('/');
            if parent.is_empty() {
                "/".to_owned()
            } else {
                parent.to_owned()
            }
        }
        None => ".".to_owned(),
    }
}
