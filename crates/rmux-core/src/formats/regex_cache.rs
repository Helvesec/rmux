use std::cell::RefCell;
use std::sync::Arc;

use regex::{Regex, RegexBuilder};

const REGEX_CACHE_CAPACITY: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegexCacheKey {
    pattern: String,
    case_insensitive: bool,
}

thread_local! {
    static REGEX_CACHE: RefCell<Vec<(RegexCacheKey, Arc<Regex>)>> = const {
        RefCell::new(Vec::new())
    };
}

pub(super) fn cached_regex(
    pattern: &str,
    case_insensitive: bool,
) -> Result<Arc<Regex>, regex::Error> {
    REGEX_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(index) = cache
            .iter()
            .position(|(key, _)| key.pattern == pattern && key.case_insensitive == case_insensitive)
        {
            let entry = cache.remove(index);
            let regex = entry.1.clone();
            cache.insert(0, entry);
            return Ok(regex);
        }

        let regex = Arc::new(
            RegexBuilder::new(pattern)
                .case_insensitive(case_insensitive)
                .build()?,
        );
        cache.insert(
            0,
            (
                RegexCacheKey {
                    pattern: pattern.to_owned(),
                    case_insensitive,
                },
                regex.clone(),
            ),
        );
        cache.truncate(REGEX_CACHE_CAPACITY);
        Ok(regex)
    })
}

#[cfg(test)]
mod tests {
    use super::cached_regex;

    #[test]
    fn cached_regex_reuses_compiled_patterns() {
        let first = cached_regex("needle", false).expect("valid regex");
        let second = cached_regex("needle", false).expect("valid regex");
        assert!(std::sync::Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn cached_regex_keeps_case_mode_distinct() {
        let sensitive = cached_regex("needle", false).expect("valid regex");
        let insensitive = cached_regex("needle", true).expect("valid regex");
        assert!(!std::sync::Arc::ptr_eq(&sensitive, &insensitive));
    }
}
