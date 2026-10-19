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
    pub accent: Color,
    pub user_rail: Color,
    pub thinking_rail: Color,
    pub composer_bg: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
    pub success: Color,
    pub failure: Color,
    pub warning: Color,
    pub added: Color,
    pub removed: Color,
    pub hunk: Color,
    /// Tool card bands: a faint tint of the card's state over the page.
    pub card_bg: Color,
    pub card_success_bg: Color,
    pub card_failure_bg: Color,
}

impl Palette {
    pub fn dark() -> Self {
        Self {
            text: Color::Rgb(0xdc, 0xdf, 0xe4),
            dim: Color::Rgb(0x9a, 0x9f, 0xab),
            faint: Color::Rgb(0x6b, 0x70, 0x7b),
            accent: Color::Rgb(0x7a, 0xa2, 0xf7),
            user_rail: Color::Rgb(0x4f, 0x8a, 0xe8),
            thinking_rail: Color::Rgb(0xa7, 0x8b, 0xe8),
            composer_bg: Color::Rgb(0x40, 0x42, 0x56),
            selection_bg: Color::Rgb(0x3a, 0x45, 0x66),
            selection_fg: Color::Rgb(0xf5, 0xf7, 0xfa),
            success: Color::Rgb(0x7a, 0xc0, 0x8a),
            failure: Color::Rgb(0xe0, 0x6c, 0x75),
            warning: Color::Rgb(0xe0, 0xb1, 0x62),
            added: Color::Rgb(0x7a, 0xc0, 0x8a),
            removed: Color::Rgb(0xe0, 0x6c, 0x75),
            hunk: Color::Rgb(0x8a, 0x90, 0xa0),
            card_bg: Color::Rgb(0x26, 0x28, 0x30),
            card_success_bg: Color::Rgb(0x1f, 0x2b, 0x25),
            card_failure_bg: Color::Rgb(0x31, 0x20, 0x24),
        }
    }

    pub fn light() -> Self {
        Self {
            text: Color::Rgb(0x2b, 0x2f, 0x38),
            dim: Color::Rgb(0x5c, 0x63, 0x70),
            faint: Color::Rgb(0x8a, 0x91, 0x9e),
            accent: Color::Rgb(0x2f, 0x5d, 0xb8),
            user_rail: Color::Rgb(0x2f, 0x5d, 0xb8),
            thinking_rail: Color::Rgb(0x6f, 0x4f, 0xb8),
            composer_bg: Color::Rgb(0xe4, 0xe6, 0xee),
            selection_bg: Color::Rgb(0xd0, 0xda, 0xf0),
            selection_fg: Color::Rgb(0xf5, 0xf7, 0xfa),
            success: Color::Rgb(0x1f, 0x7a, 0x3a),
            failure: Color::Rgb(0xb8, 0x2a, 0x35),
            warning: Color::Rgb(0x9a, 0x6a, 0x00),
            added: Color::Rgb(0x1f, 0x7a, 0x3a),
            removed: Color::Rgb(0xb8, 0x2a, 0x35),
            hunk: Color::Rgb(0x6b, 0x72, 0x80),
            card_bg: Color::Rgb(0xee, 0xf0, 0xf4),
            card_success_bg: Color::Rgb(0xe5, 0xf2, 0xe8),
            card_failure_bg: Color::Rgb(0xf8, 0xe6, 0xe8),
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

    pub fn code(&self) -> Style {
        bg(self.palette.faint).patch(self.text())
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
