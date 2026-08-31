//! Minimal composer state for Phase 3 (development spec 21). Phase 5 swaps
//! the internals for `tui-textarea`; every other module only reads these two
//! fields and never touches an editor directly.

/// The message input area. `cursor` is a char boundary within `text`; the
/// renderer computes the terminal column from it via `unicode-width`.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct Composer {
    pub text: String,
    pub cursor: usize,
}
