//! Color themes: the built-in dark and light palettes from the development
//! spec. No external theme files; themes are selected by CLI flag only.

use ratatui::style::Color;

use crate::protocol::Reasoning;

/// Selects one of the built-in color palettes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThemeKind {
    #[default]
    Dark,
    Light,
}

/// The complete color set used by the TUI renderers, per the v0.1 spec:
/// base palette, borders, message and tool cards, markdown, and the
/// per-reasoning-level thinking colors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Theme {
    pub page_bg: Color,
    pub text: Color,
    pub muted: Color,
    pub dim: Color,
    pub accent: Color,
    pub border: Color,
    pub border_accent: Color,
    pub border_muted: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub selected_bg: Color,
    pub user_message_bg: Color,
    /// Card surfaces for summaries, notices, and neutral tool cards.
    pub card_bg: Color,
    pub tool_pending_bg: Color,
    pub tool_success_bg: Color,
    pub tool_error_bg: Color,
    pub md_heading: Color,
    pub md_link: Color,
    pub md_link_url: Color,
    pub md_code: Color,
    pub md_code_block: Color,
    pub md_code_border: Color,
    pub md_quote: Color,
    pub md_list_bullet: Color,
    pub thinking_disabled: Color,
    pub thinking_auto: Color,
    pub thinking_low: Color,
    pub thinking_medium: Color,
    pub thinking_high: Color,
}

impl Theme {
    pub fn for_kind(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::Dark => Self::dark(),
            ThemeKind::Light => Self::light(),
        }
    }

    /// The spec dark palette. Border fields map to the spec grays: `border`
    /// is gray, `border_muted` is dimGray, `border_accent` is accent, and
    /// `thinking_disabled` is darkGray.
    pub fn dark() -> Self {
        Self {
            page_bg: rgb(0x18, 0x18, 0x1e),
            text: rgb(0xd4, 0xd4, 0xd4),
            muted: rgb(0x80, 0x80, 0x80),
            dim: rgb(0x66, 0x66, 0x66),
            accent: rgb(0x8a, 0xbe, 0xb7),
            border: rgb(0x80, 0x80, 0x80),
            border_accent: rgb(0x8a, 0xbe, 0xb7),
            border_muted: rgb(0x66, 0x66, 0x66),
            success: rgb(0xb5, 0xbd, 0x68),
            warning: rgb(0xff, 0xff, 0x00),
            error: rgb(0xcc, 0x66, 0x66),
            selected_bg: rgb(0x3a, 0x3a, 0x4a),
            user_message_bg: rgb(0x34, 0x35, 0x41),
            card_bg: rgb(0x1e, 0x1e, 0x24),
            tool_pending_bg: rgb(0x28, 0x28, 0x32),
            tool_success_bg: rgb(0x28, 0x32, 0x28),
            tool_error_bg: rgb(0x3c, 0x28, 0x28),
            md_heading: rgb(0xf0, 0xc6, 0x74),
            md_link: rgb(0x81, 0xa2, 0xbe),
            md_link_url: rgb(0x66, 0x66, 0x66),
            md_code: rgb(0x8a, 0xbe, 0xb7),
            md_code_block: rgb(0xb5, 0xbd, 0x68),
            md_code_border: rgb(0x80, 0x80, 0x80),
            md_quote: rgb(0x80, 0x80, 0x80),
            md_list_bullet: rgb(0x8a, 0xbe, 0xb7),
            thinking_disabled: rgb(0x50, 0x50, 0x50),
            thinking_auto: rgb(0x8a, 0xbe, 0xb7),
            thinking_low: rgb(0x5f, 0x87, 0xaf),
            thinking_medium: rgb(0x81, 0xa2, 0xbe),
            thinking_high: rgb(0xb2, 0x94, 0xbb),
        }
    }

    /// The spec light base palette. The accent-border, markdown, and thinking
    /// fields are not specified for light; they are derived from the defined
    /// light colors (accent, border, warning, success, muted, dim) instead.
    pub fn light() -> Self {
        Self {
            page_bg: rgb(0xf8, 0xf8, 0xf8),
            text: rgb(0x1f, 0x23, 0x28),
            muted: rgb(0x6c, 0x6c, 0x6c),
            dim: rgb(0x76, 0x76, 0x76),
            accent: rgb(0x5a, 0x80, 0x80),
            border: rgb(0x54, 0x7d, 0xa7),
            border_accent: rgb(0x5a, 0x80, 0x80),
            border_muted: rgb(0xb0, 0xb0, 0xb0),
            success: rgb(0x58, 0x84, 0x58),
            warning: rgb(0x9a, 0x73, 0x26),
            error: rgb(0xaa, 0x55, 0x55),
            selected_bg: rgb(0xd0, 0xd0, 0xe0),
            user_message_bg: rgb(0xe8, 0xe8, 0xe8),
            // card and markdown-surface colors are documented derivations of
            // the defined light colors (spec 16.3): white card surface,
            // dim for link URLs, borderMuted for code borders, accent for
            // list bullets.
            card_bg: rgb(0xff, 0xff, 0xff),
            tool_pending_bg: rgb(0xe8, 0xe8, 0xf0),
            tool_success_bg: rgb(0xe8, 0xf0, 0xe8),
            tool_error_bg: rgb(0xf0, 0xe8, 0xe8),
            md_heading: rgb(0x9a, 0x73, 0x26),
            md_link: rgb(0x54, 0x7d, 0xa7),
            md_link_url: rgb(0x76, 0x76, 0x76),
            md_code: rgb(0x3a, 0x66, 0x66),
            md_code_block: rgb(0x58, 0x84, 0x58),
            md_code_border: rgb(0xb0, 0xb0, 0xb0),
            md_quote: rgb(0x6c, 0x6c, 0x6c),
            md_list_bullet: rgb(0x5a, 0x80, 0x80),
            thinking_disabled: rgb(0x76, 0x76, 0x76),
            thinking_auto: rgb(0x5a, 0x80, 0x80),
            thinking_low: rgb(0x4a, 0x74, 0x9c),
            thinking_medium: rgb(0x5f, 0x80, 0x99),
            thinking_high: rgb(0x8a, 0x6d, 0xa8),
        }
    }
    /// The dot-composed thinking color for a reasoning level (spec 15.7).
    pub fn reasoning_color(&self, reasoning: Reasoning) -> Color {
        match reasoning {
            Reasoning::Disabled => self.thinking_disabled,
            Reasoning::Auto => self.thinking_auto,
            Reasoning::Low => self.thinking_low,
            Reasoning::Medium => self.thinking_medium,
            Reasoning::High => self.thinking_high,
        }
    }
}

fn rgb(red: u8, green: u8, blue: u8) -> Color {
    Color::Rgb(red, green, blue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_kind_is_dark_and_for_kind_roundtrips() {
        assert_eq!(ThemeKind::default(), ThemeKind::Dark);
        assert_eq!(Theme::for_kind(ThemeKind::Dark), Theme::dark());
        assert_eq!(Theme::for_kind(ThemeKind::Light), Theme::light());
    }

    #[test]
    fn dark_palette_matches_spec() {
        let theme = Theme::dark();
        assert_eq!(theme.page_bg, rgb(0x18, 0x18, 0x1e));
        assert_eq!(theme.text, rgb(0xd4, 0xd4, 0xd4));
        assert_eq!(theme.muted, rgb(0x80, 0x80, 0x80));
        assert_eq!(theme.dim, rgb(0x66, 0x66, 0x66));
        assert_eq!(theme.accent, rgb(0x8a, 0xbe, 0xb7));
        assert_eq!(theme.border, rgb(0x80, 0x80, 0x80));
        assert_eq!(theme.border_accent, rgb(0x8a, 0xbe, 0xb7));
        assert_eq!(theme.border_muted, rgb(0x66, 0x66, 0x66));
        assert_eq!(theme.success, rgb(0xb5, 0xbd, 0x68));
        assert_eq!(theme.warning, rgb(0xff, 0xff, 0x00));
        assert_eq!(theme.error, rgb(0xcc, 0x66, 0x66));
        assert_eq!(theme.selected_bg, rgb(0x3a, 0x3a, 0x4a));
        assert_eq!(theme.user_message_bg, rgb(0x34, 0x35, 0x41));
        assert_eq!(theme.card_bg, rgb(0x1e, 0x1e, 0x24));
        assert_eq!(theme.tool_pending_bg, rgb(0x28, 0x28, 0x32));
        assert_eq!(theme.tool_success_bg, rgb(0x28, 0x32, 0x28));
        assert_eq!(theme.tool_error_bg, rgb(0x3c, 0x28, 0x28));
        assert_eq!(theme.md_heading, rgb(0xf0, 0xc6, 0x74));
        assert_eq!(theme.md_link, rgb(0x81, 0xa2, 0xbe));
        assert_eq!(theme.md_link_url, rgb(0x66, 0x66, 0x66));
        assert_eq!(theme.md_code, rgb(0x8a, 0xbe, 0xb7));
        assert_eq!(theme.md_code_block, rgb(0xb5, 0xbd, 0x68));
        assert_eq!(theme.md_code_border, rgb(0x80, 0x80, 0x80));
        assert_eq!(theme.md_quote, rgb(0x80, 0x80, 0x80));
        assert_eq!(theme.md_list_bullet, rgb(0x8a, 0xbe, 0xb7));
        assert_eq!(theme.thinking_disabled, rgb(0x50, 0x50, 0x50));
        assert_eq!(theme.thinking_auto, rgb(0x8a, 0xbe, 0xb7));
        assert_eq!(theme.thinking_low, rgb(0x5f, 0x87, 0xaf));
        assert_eq!(theme.thinking_medium, rgb(0x81, 0xa2, 0xbe));
        assert_eq!(theme.thinking_high, rgb(0xb2, 0x94, 0xbb));
    }

    #[test]
    fn light_base_palette_matches_spec() {
        let theme = Theme::light();
        assert_eq!(theme.page_bg, rgb(0xf8, 0xf8, 0xf8));
        assert_eq!(theme.text, rgb(0x1f, 0x23, 0x28));
        assert_eq!(theme.muted, rgb(0x6c, 0x6c, 0x6c));
        assert_eq!(theme.dim, rgb(0x76, 0x76, 0x76));
        assert_eq!(theme.accent, rgb(0x5a, 0x80, 0x80));
        assert_eq!(theme.border, rgb(0x54, 0x7d, 0xa7));
        assert_eq!(theme.border_muted, rgb(0xb0, 0xb0, 0xb0));
        assert_eq!(theme.success, rgb(0x58, 0x84, 0x58));
        assert_eq!(theme.error, rgb(0xaa, 0x55, 0x55));
        assert_eq!(theme.warning, rgb(0x9a, 0x73, 0x26));
        assert_eq!(theme.selected_bg, rgb(0xd0, 0xd0, 0xe0));
        assert_eq!(theme.user_message_bg, rgb(0xe8, 0xe8, 0xe8));
        assert_eq!(theme.card_bg, rgb(0xff, 0xff, 0xff));
        assert_eq!(theme.tool_pending_bg, rgb(0xe8, 0xe8, 0xf0));
        assert_eq!(theme.tool_success_bg, rgb(0xe8, 0xf0, 0xe8));
        assert_eq!(theme.tool_error_bg, rgb(0xf0, 0xe8, 0xe8));
        assert_eq!(theme.md_link_url, rgb(0x76, 0x76, 0x76));
        assert_eq!(theme.md_code_border, rgb(0xb0, 0xb0, 0xb0));
        assert_eq!(theme.md_list_bullet, rgb(0x5a, 0x80, 0x80));
    }

    #[test]
    fn reasoning_color_maps_to_the_level_palette() {
        let theme = Theme::dark();
        assert_eq!(
            theme.reasoning_color(Reasoning::Disabled),
            rgb(0x50, 0x50, 0x50)
        );
        assert_eq!(
            theme.reasoning_color(Reasoning::Auto),
            rgb(0x8a, 0xbe, 0xb7)
        );
        assert_eq!(theme.reasoning_color(Reasoning::Low), rgb(0x5f, 0x87, 0xaf));
        assert_eq!(
            theme.reasoning_color(Reasoning::Medium),
            rgb(0x81, 0xa2, 0xbe)
        );
        assert_eq!(
            theme.reasoning_color(Reasoning::High),
            rgb(0xb2, 0x94, 0xbb)
        );
    }
}
