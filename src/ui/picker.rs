//! The filter every picker applies to its rows.

/// Case-insensitive subsequence match: every needle character must appear in
/// the haystack in order, but not necessarily adjacently.
pub fn fuzzy_match(haystack: &str, needle: &str) -> bool {
    let mut haystack = haystack.chars().flat_map(char::to_lowercase);
    needle
        .chars()
        .flat_map(char::to_lowercase)
        .all(|expected| haystack.any(|actual| actual == expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_matching_is_a_case_insensitive_subsequence() {
        assert!(fuzzy_match("reasoning", "rsn"));
        assert!(fuzzy_match("Reasoning", "RSN"));
        assert!(fuzzy_match("compact", ""));
        assert!(!fuzzy_match("compact", "zzz"));
        assert!(!fuzzy_match("compact", "tmoc"));
    }
}
