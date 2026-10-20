//! The filter and the scrolling every picker applies to its rows.

use std::ops::Range;

/// Case-insensitive subsequence match: every needle character must appear in
/// the haystack in order, but not necessarily adjacently.
pub fn fuzzy_match(haystack: &str, needle: &str) -> bool {
    let mut haystack = haystack.chars().flat_map(char::to_lowercase);
    needle
        .chars()
        .flat_map(char::to_lowercase)
        .all(|expected| haystack.any(|actual| actual == expected))
}

/// The `visible` rows of a `len`-row list drawn around `selected`, which sits
/// in the middle unless the list ends first.
pub fn window(selected: usize, len: usize, visible: usize) -> Range<usize> {
    let start = selected
        .saturating_sub(visible / 2)
        .min(len.saturating_sub(visible));
    start..(start + visible).min(len)
}

/// `selected` moved by `delta`: single steps wrap around the ends, pages stop at them.
pub fn step(selected: usize, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let moved = selected as isize + delta;
    if delta.abs() == 1 {
        moved.rem_euclid(len as isize) as usize
    } else {
        moved.clamp(0, len as isize - 1) as usize
    }
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

    #[test]
    fn the_window_centres_the_selection_until_an_end() {
        assert_eq!(window(0, 20, 5), 0..5);
        assert_eq!(window(2, 20, 5), 0..5);
        assert_eq!(window(3, 20, 5), 1..6);
        assert_eq!(window(10, 20, 5), 8..13);
        assert_eq!(window(19, 20, 5), 15..20);
        assert_eq!(window(1, 3, 5), 0..3);
        assert_eq!(window(0, 0, 5), 0..0);
    }

    #[test]
    fn single_steps_wrap_and_pages_clamp() {
        assert_eq!(step(0, -1, 4), 3);
        assert_eq!(step(3, 1, 4), 0);
        assert_eq!(step(1, 1, 4), 2);
        assert_eq!(step(2, 10, 4), 3);
        assert_eq!(step(2, -10, 4), 0);
        assert_eq!(step(0, 1, 0), 0);
    }
}
