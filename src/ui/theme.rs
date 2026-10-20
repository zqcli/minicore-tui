//! The one theme layer: a `Palette` holds a theme's colors, `Styles` turns them
//! into the role styles the renderers ask for. `NO_COLOR` is a palette with no
//! colors, so every role keeps its attributes and drops its color.

use ratatui::style::{Color, Modifier, Style};

use crate::config::ThemeKind;

/// The colors of one theme; `Color::Reset` means the terminal's own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Palette {
    pub text: Color,
    /// Secondary text: hints, durations, metadata.
    pub dim: Color,
    /// Faint text inside cards and markers.
    pub faint: Color,
    /// Timestamps, card hints and footer separators.
    pub meta: Color,
    pub accent: Color,
    /// The editor, user and pending-card rail; also the footer's model and token counts.
    pub user_rail: Color,
    /// The thinking rail; also the footer's cache and context figures.
    pub thinking_rail: Color,
    pub notice_rail: Color,
    pub status_rail: Color,
    pub composer_bg: Color,
    pub paste_bg: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
    pub border: Color,
    pub success: Color,
    pub failure: Color,
    pub warning: Color,
    pub mint: Color,
    pub amber: Color,
    pub heading: Color,
    pub inline_code: Color,
    pub code_block: Color,
    pub added: Color,
    pub removed: Color,
    pub hunk: Color,
    pub tool_title: Color,
    pub tool_output: Color,
    /// Tool card bands and rails per state: running, done, failed.
    pub card_bg: Color,
    pub card_success_bg: Color,
    pub card_failure_bg: Color,
    pub card_rail: Color,
    pub card_success_rail: Color,
    pub card_failure_rail: Color,
    /// The compaction marker's box.
    pub custom_bg: Color,
}

impl Palette {
    pub fn dark() -> Self {
        Self {
            text: Color::Rgb(0xd4, 0xd4, 0xd4),
            dim: Color::Rgb(0x80, 0x80, 0x80),
            faint: Color::Rgb(0x66, 0x66, 0x66),
            meta: Color::Rgb(127, 132, 156),
            accent: Color::Rgb(0x8a, 0xbe, 0xb7),
            user_rail: Color::Rgb(137, 180, 250),
            thinking_rail: Color::Rgb(203, 166, 247),
            notice_rail: Color::Rgb(148, 226, 213),
            status_rail: Color::Rgb(250, 179, 135),
            composer_bg: Color::Rgb(49, 50, 68),
            paste_bg: Color::Rgb(69, 71, 90),
            selection_bg: Color::Rgb(0x3a, 0x3a, 0x4a),
            selection_fg: Color::Rgb(245, 245, 250),
            border: Color::Rgb(0x5f, 0x87, 0xff),
            success: Color::Rgb(0xb5, 0xbd, 0x68),
            failure: Color::Rgb(0xcc, 0x66, 0x66),
            warning: Color::Rgb(249, 226, 175),
            mint: Color::Rgb(166, 227, 161),
            amber: Color::Rgb(249, 226, 175),
            heading: Color::Rgb(0xf0, 0xc6, 0x74),
            inline_code: Color::Rgb(0x8a, 0xbe, 0xb7),
            code_block: Color::Rgb(0xb5, 0xbd, 0x68),
            added: Color::Rgb(0xb5, 0xbd, 0x68),
            removed: Color::Rgb(0xcc, 0x66, 0x66),
            hunk: Color::Rgb(0x80, 0x80, 0x80),
            tool_title: Color::Rgb(205, 214, 244),
            tool_output: Color::Rgb(166, 173, 200),
            card_bg: Color::Rgb(40, 43, 61),
            card_success_bg: Color::Rgb(41, 49, 46),
            card_failure_bg: Color::Rgb(52, 43, 47),
            card_rail: Color::Rgb(137, 180, 250),
            card_success_rail: Color::Rgb(123, 159, 136),
            card_failure_rail: Color::Rgb(188, 120, 136),
            custom_bg: Color::Rgb(0x2d, 0x28, 0x38),
        }
    }

    pub fn light() -> Self {
        Self {
            text: Color::Rgb(0x2b, 0x2f, 0x38),
            dim: Color::Rgb(0x5c, 0x63, 0x70),
            faint: Color::Rgb(0x8a, 0x91, 0x9e),
            meta: Color::Rgb(0x6c, 0x6f, 0x85),
            accent: Color::Rgb(0x17, 0x92, 0x99),
            user_rail: Color::Rgb(0x1e, 0x66, 0xf5),
            thinking_rail: Color::Rgb(0x88, 0x39, 0xef),
            notice_rail: Color::Rgb(0x17, 0x92, 0x99),
            status_rail: Color::Rgb(0xfe, 0x64, 0x0b),
            composer_bg: Color::Rgb(0xe6, 0xe9, 0xef),
            paste_bg: Color::Rgb(0xcc, 0xd0, 0xda),
            selection_bg: Color::Rgb(0xd0, 0xda, 0xf0),
            selection_fg: Color::Rgb(0x2b, 0x2f, 0x38),
            border: Color::Rgb(0x1e, 0x66, 0xf5),
            success: Color::Rgb(0x40, 0xa0, 0x2b),
            failure: Color::Rgb(0xd2, 0x0f, 0x39),
            warning: Color::Rgb(0xdf, 0x8e, 0x1d),
            mint: Color::Rgb(0x40, 0xa0, 0x2b),
            amber: Color::Rgb(0xdf, 0x8e, 0x1d),
            heading: Color::Rgb(0xdf, 0x8e, 0x1d),
            inline_code: Color::Rgb(0x17, 0x92, 0x99),
            code_block: Color::Rgb(0x40, 0xa0, 0x2b),
            added: Color::Rgb(0x40, 0xa0, 0x2b),
            removed: Color::Rgb(0xd2, 0x0f, 0x39),
            hunk: Color::Rgb(0x6b, 0x72, 0x80),
            tool_title: Color::Rgb(0x4c, 0x4f, 0x69),
            tool_output: Color::Rgb(0x5c, 0x5f, 0x77),
            card_bg: Color::Rgb(0xe8, 0xec, 0xf8),
            card_success_bg: Color::Rgb(0xe6, 0xf2, 0xe6),
            card_failure_bg: Color::Rgb(0xf8, 0xe6, 0xea),
            card_rail: Color::Rgb(0x1e, 0x66, 0xf5),
            card_success_rail: Color::Rgb(0x40, 0xa0, 0x2b),
            card_failure_rail: Color::Rgb(0xd2, 0x0f, 0x39),
            custom_bg: Color::Rgb(0xed, 0xe7, 0xf6),
        }
    }

    /// No colors at all, per `NO_COLOR`.
    pub fn plain() -> Self {
        Self::default()
    }

    /// The palette for `kind`, or the plain one when `NO_COLOR` is set.
    pub fn for_kind(kind: ThemeKind) -> Self {
        if std::env::var_os("NO_COLOR").is_some() {
            return Self::plain();
        }
        match kind {
            ThemeKind::Dark => Self::dark(),
            ThemeKind::Light => Self::light(),
        }
    }
}

fn fg(color: Color) -> Style {
    if color == Color::Reset {
        Style::new()
    } else {
        Style::new().fg(color)
    }
}

fn bg(color: Color) -> Style {
    if color == Color::Reset {
        Style::new()
    } else {
        Style::new().bg(color)
    }
}

/// The styles the UI asks for by role, so no renderer names a raw color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Styles {
    palette: Palette,
}

impl Styles {
    pub fn of(palette: Palette) -> Self {
        Self { palette }
    }

    pub fn text(&self) -> Style {
        fg(self.palette.text)
    }

    pub fn dim(&self) -> Style {
        fg(self.palette.dim)
    }

    pub fn faint(&self) -> Style {
        fg(self.palette.faint)
    }

    pub fn accent(&self) -> Style {
        fg(self.palette.accent)
    }

    pub fn success(&self) -> Style {
        fg(self.palette.success)
    }

    pub fn failure(&self) -> Style {
        fg(self.palette.failure)
    }

    pub fn warning(&self) -> Style {
        fg(self.palette.warning)
    }

    pub fn added(&self) -> Style {
        fg(self.palette.added)
    }

    pub fn removed(&self) -> Style {
        fg(self.palette.removed)
    }

    pub fn hunk(&self) -> Style {
        fg(self.palette.hunk)
    }

    pub fn bold(&self) -> Style {
        self.text().add_modifier(Modifier::BOLD)
    }

    /// Titles of views and sheets.
    pub fn title(&self) -> Style {
        self.accent().add_modifier(Modifier::BOLD)
    }

    pub fn link(&self) -> Style {
        self.accent().add_modifier(Modifier::UNDERLINED)
    }

    pub fn user_rail(&self) -> Style {
        fg(self.palette.user_rail)
    }

    pub fn reasoning_rail(&self) -> Style {
        fg(self.palette.thinking_rail)
    }

    /// Inline code: colored, no band.
    pub fn code(&self) -> Style {
        fg(self.palette.inline_code)
    }

    pub fn code_block(&self) -> Style {
        fg(self.palette.code_block)
    }

    pub fn heading(&self) -> Style {
        fg(self.palette.heading).add_modifier(Modifier::BOLD)
    }

    pub fn meta(&self) -> Style {
        fg(self.palette.meta)
    }

    pub fn mint(&self) -> Style {
        fg(self.palette.mint)
    }

    pub fn amber(&self) -> Style {
        fg(self.palette.amber)
    }

    pub fn border(&self) -> Style {
        fg(self.palette.border)
    }

    pub fn notice_rail(&self) -> Style {
        fg(self.palette.notice_rail)
    }

    pub fn status_rail(&self) -> Style {
        fg(self.palette.status_rail)
    }

    pub fn tool_title(&self) -> Style {
        fg(self.palette.tool_title)
    }

    pub fn tool_output(&self) -> Style {
        fg(self.palette.tool_output)
    }

    /// A `[paste #N …]` chip inside the composer.
    pub fn paste_chip(&self) -> Style {
        bg(self.palette.paste_bg)
            .patch(fg(self.palette.user_rail))
            .add_modifier(Modifier::BOLD)
    }

    pub fn composer(&self) -> Style {
        bg(self.palette.composer_bg)
    }

    /// The tool card band for a state that is neither done nor failed.
    pub fn card(&self) -> Style {
        bg(self.palette.card_bg)
    }

    pub fn card_success(&self) -> Style {
        bg(self.palette.card_success_bg)
    }

    pub fn card_failure(&self) -> Style {
        bg(self.palette.card_failure_bg)
    }

    pub fn card_rail(&self) -> Style {
        fg(self.palette.card_rail)
    }

    pub fn card_success_rail(&self) -> Style {
        fg(self.palette.card_success_rail)
    }

    pub fn card_failure_rail(&self) -> Style {
        fg(self.palette.card_failure_rail)
    }

    pub fn custom_message(&self) -> Style {
        bg(self.palette.custom_bg)
    }

    /// Mouse selection; reverse video when the palette has no colors.
    pub fn selection(&self) -> Style {
        if self.palette.selection_bg == Color::Reset {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            bg(self.palette.selection_bg).patch(fg(self.palette.selection_fg))
        }
    }

    /// The band behind the highlighted row of a list.
    pub fn highlight(&self) -> Style {
        bg(self.palette.selection_bg)
    }

    /// A unified-diff line, colored by its first character.
    pub fn patch(&self, line: &str) -> Style {
        match line.chars().next() {
            Some('+') => self.added(),
            Some('-') => self.removed(),
            Some('@') => self.dim(),
            _ => self.text(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plain_palette_drops_every_color_but_keeps_attributes() {
        let styles = Styles::of(Palette::plain());
        for style in [
            styles.text(),
            styles.dim(),
            styles.accent(),
            styles.user_rail(),
            styles.composer(),
            styles.card(),
            styles.card_failure(),
            styles.highlight(),
            styles.selection(),
            styles.added(),
            styles.code(),
            styles.patch("+added"),
        ] {
            assert_eq!(style.fg, None, "{style:?}");
            assert_eq!(style.bg, None, "{style:?}");
        }
        assert!(styles.bold().add_modifier.contains(Modifier::BOLD));
        assert!(styles.title().add_modifier.contains(Modifier::BOLD));
        assert!(styles.selection().add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn palettes_keep_their_own_roles() {
        let dark = Styles::of(Palette::dark());
        let light = Styles::of(Palette::light());
        assert_ne!(dark.text().fg, light.text().fg);
        assert_ne!(dark.composer().bg, light.composer().bg);
        assert_eq!(dark.user_rail().fg, Some(Palette::dark().user_rail));
    }

    #[test]
    fn diff_lines_are_colored_by_their_prefix() {
        let palette = Palette::dark();
        let styles = Styles::of(palette);
        assert_eq!(styles.patch("+added").fg, Some(palette.added));
        assert_eq!(styles.patch("-removed").fg, Some(palette.removed));
        assert_eq!(styles.patch("@@ hunk @@").fg, Some(palette.dim));
        assert_eq!(styles.patch(" context").fg, Some(palette.text));
    }
}
