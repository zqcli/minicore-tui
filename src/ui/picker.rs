//! The generic filterable list behind every picker: command palette, sessions,
//! models, reasoning levels and the `@file` picker.
//!
//! A picker owns the filter text, the selection and the scroll offset; it
//! filters a caller-supplied item list by fuzzy subsequence match and returns
//! the selected item's key. Rendering lives in [`super::panel`], keyboard and
//! mouse handling in the app layer, so this module stays pure and testable.

/// One picker row: the filter/sort key, the text drawn in the list and the
/// dim annotation on the right (a model, a relative age, a directory).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub key: String,
    pub label: String,
    pub detail: String,
}

impl Item {
    pub fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            detail: String::new(),
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

/// Case-insensitive subsequence match: every needle character must appear in
/// the haystack in order, but not necessarily adjacently.
pub fn fuzzy_match(haystack: &str, needle: &str) -> bool {
    let mut haystack = haystack.chars().flat_map(char::to_lowercase);
    needle
        .chars()
        .flat_map(char::to_lowercase)
        .all(|expected| haystack.any(|actual| actual == expected))
}

/// Filter rank of one item against a query, lower is better. An empty query
/// keeps the caller's order, which is how sessions get "current workspace
/// first" and commands keep their table order.
fn rank(item: &Item, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    if item.label.to_lowercase().starts_with(&query.to_lowercase()) {
        return Some(0);
    }
    if fuzzy_match(&item.label, query) {
        return Some(1);
    }
    fuzzy_match(&item.detail, query).then_some(2)
}

/// A filterable list with a selection kept inside the visible window.
#[derive(Clone, Debug, Default)]
pub struct Picker {
    pub query: String,
    pub selected: usize,
    /// Index of the first visible row.
    pub offset: usize,
    items: Vec<Item>,
    visible: Vec<usize>,
}

impl Picker {
    pub fn new(items: Vec<Item>) -> Self {
        let mut picker = Self {
            items,
            ..Self::default()
        };
        picker.refilter();
        picker
    }

    /// Replaces the item list, keeping the selection when the same key is
    /// still visible (a session list refresh, a file scan finishing).
    pub fn set_items(&mut self, items: Vec<Item>) {
        let previous = self.current().map(|item| item.key.clone());
        self.items = items;
        self.refilter();
        if let Some(key) = previous {
            self.select_key(&key);
        }
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// The filtered items, in display order.
    pub fn visible(&self) -> impl Iterator<Item = &Item> + '_ {
        self.visible.iter().map(|index| &self.items[*index])
    }

    pub fn visible_len(&self) -> usize {
        self.visible.len()
    }

    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    pub fn current(&self) -> Option<&Item> {
        self.visible
            .get(self.selected)
            .map(|index| &self.items[*index])
    }

    pub fn current_key(&self) -> Option<&str> {
        self.current().map(|item| item.key.as_str())
    }

    /// Position of `key` among the visible rows.
    pub fn position_of(&self, key: &str) -> Option<usize> {
        self.visible
            .iter()
            .position(|index| self.items[*index].key == key)
    }

    /// Selects `key` when it is visible; used to preselect the current model or
    /// reasoning level.
    pub fn select_key(&mut self, key: &str) {
        if let Some(index) = self.position_of(key) {
            self.selected = index;
            self.clamp();
        }
    }

    /// Appends a character to the filter and refilters.
    pub fn push(&mut self, ch: char) {
        self.query.push(ch);
        self.refilter();
    }

    /// Removes the last filter character; the selection resets because the row
    /// set changed under it.
    pub fn pop(&mut self) -> bool {
        if self.query.pop().is_some() {
            self.refilter();
            true
        } else {
            false
        }
    }

    pub fn clear_query(&mut self) {
        if !self.query.is_empty() {
            self.query.clear();
            self.refilter();
        }
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let last = self.visible.len() - 1;
        let next = (self.selected as isize + delta).clamp(0, last as isize);
        self.selected = next as usize;
        self.clamp();
    }

    pub fn first(&mut self) {
        self.selected = 0;
        self.clamp();
    }

    pub fn last(&mut self) {
        self.selected = self.visible.len().saturating_sub(1);
        self.clamp();
    }

    /// Moves the selection one page up or down.
    pub fn page(&mut self, delta: isize, rows: usize) {
        if rows == 0 {
            return;
        }
        self.move_by(delta * rows as isize);
    }

    /// Keeps the selected row inside a window of `rows` rows.
    pub fn scroll_to(&mut self, rows: usize) {
        if rows == 0 {
            return;
        }
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + rows {
            self.offset = self.selected + 1 - rows;
        }
        let max_offset = self.visible.len().saturating_sub(rows);
        self.offset = self.offset.min(max_offset);
    }

    /// The visible row range for a window of `rows` rows, as `start..end`
    /// indices into the filtered list.
    pub fn window(&self, rows: usize) -> std::ops::Range<usize> {
        let start = self.offset.min(self.visible.len());
        let end = (start + rows).min(self.visible.len());
        start..end
    }

    fn clamp(&mut self) {
        if self.visible.is_empty() {
            self.selected = 0;
            self.offset = 0;
            return;
        }
        self.selected = self.selected.min(self.visible.len() - 1);
        self.offset = self.offset.min(self.selected);
    }

    fn refilter(&mut self) {
        let query = self.query.trim().to_owned();
        let mut ranked: Vec<(u8, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| rank(item, &query).map(|rank| (rank, index)))
            .collect();
        // Stable by rank, original order inside a rank.
        ranked.sort_by_key(|(rank, index)| (*rank, *index));
        self.visible = ranked.into_iter().map(|(_, index)| index).collect();
        self.selected = 0;
        self.offset = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<Item> {
        vec![
            Item::new("new", "new").with_detail("Start a new session"),
            Item::new("sessions", "sessions").with_detail("Open or delete a session"),
            Item::new("reasoning", "reasoning").with_detail("Switch the reasoning level"),
            Item::new("rename", "rename").with_detail("Rename this session"),
        ]
    }

    #[test]
    fn fuzzy_matching_is_a_case_insensitive_subsequence() {
        assert!(fuzzy_match("reasoning", "rsn"));
        assert!(fuzzy_match("Reasoning", "RSN"));
        assert!(fuzzy_match("compact", ""));
        assert!(!fuzzy_match("compact", "zzz"));
        assert!(!fuzzy_match("compact", "tmoc"));
    }

    #[test]
    fn an_empty_query_keeps_the_caller_order() {
        let picker = Picker::new(items());
        let labels: Vec<&str> = picker.visible().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, ["new", "sessions", "reasoning", "rename"]);
        assert_eq!(picker.current_key(), Some("new"));
    }

    #[test]
    fn filters_rank_prefix_matches_before_inner_matches() {
        let mut picker = Picker::new(items());
        for ch in "re".chars() {
            picker.push(ch);
        }
        let labels: Vec<&str> = picker.visible().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, ["reasoning", "rename", "new", "sessions"]);
        assert_eq!(picker.current_key(), Some("reasoning"));
    }

    #[test]
    fn detail_text_is_searchable_but_ranks_last() {
        let mut picker = Picker::new(items());
        for ch in "delete".chars() {
            picker.push(ch);
        }
        assert_eq!(picker.current_key(), Some("sessions"));
    }

    #[test]
    fn a_query_that_matches_nothing_leaves_no_selection() {
        let mut picker = Picker::new(items());
        for ch in "zzzz".chars() {
            picker.push(ch);
        }
        assert!(picker.is_empty());
        assert_eq!(picker.current_key(), None);
        picker.move_by(1);
        assert_eq!(picker.current_key(), None);
        while picker.pop() {}
        assert_eq!(picker.visible_len(), items().len());
    }

    #[test]
    fn mutation_resets_the_selection_to_the_first_row() {
        let mut picker = Picker::new(items());
        picker.move_by(2);
        assert_eq!(picker.current_key(), Some("reasoning"));
        picker.push('r');
        assert_eq!(picker.current_key(), Some("reasoning"));
        picker.clear_query();
        assert_eq!(picker.current_key(), Some("new"));
    }

    #[test]
    fn movement_clamps_at_both_ends() {
        let mut picker = Picker::new(items());
        picker.move_by(-5);
        assert_eq!(picker.selected, 0);
        picker.move_by(99);
        assert_eq!(picker.selected, 3);
        picker.first();
        assert_eq!(picker.selected, 0);
        picker.last();
        assert_eq!(picker.selected, 3);
    }

    #[test]
    fn the_window_follows_the_selection() {
        let mut picker = Picker::new(items());
        picker.scroll_to(2);
        assert_eq!(picker.window(2), 0..2);

        picker.move_by(2);
        picker.scroll_to(2);
        assert_eq!(
            picker.window(2),
            1..3,
            "the selection is the last visible row"
        );

        picker.move_by(-2);
        picker.scroll_to(2);
        assert_eq!(picker.window(2), 0..2);
    }

    #[test]
    fn paging_moves_by_the_visible_height() {
        let mut picker = Picker::new(items());
        picker.page(1, 2);
        assert_eq!(picker.selected, 2);
        picker.page(-1, 2);
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn set_items_keeps_the_same_key_selected_when_it_survives() {
        let mut picker = Picker::new(items());
        picker.select_key("reasoning");
        picker.set_items(vec![
            Item::new("new", "new"),
            Item::new("reasoning", "reasoning"),
        ]);
        assert_eq!(picker.current_key(), Some("reasoning"));
    }

    #[test]
    fn set_items_falls_back_to_the_first_row_when_the_key_is_gone() {
        let mut picker = Picker::new(items());
        picker.select_key("rename");
        picker.set_items(vec![Item::new("new", "new")]);
        assert_eq!(picker.current_key(), Some("new"));
    }

    #[test]
    fn select_key_ignores_keys_that_are_not_visible() {
        let mut picker = Picker::new(items());
        picker.select_key("absent");
        assert_eq!(picker.selected, 0);
    }
}
