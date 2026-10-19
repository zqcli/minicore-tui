//! Role styles for the renderers. The colors themselves live in
//! [`crate::theme::Palette`]; this module only decides which role each renderer
//! asks for, and applies `NO_COLOR` by dropping color while keeping attributes.

use ratatui::style::{Color, Modifier, Style};

use crate::theme::Palette;

/// The named colors a renderer uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub text: Color,
    pub dim: Color,
    pub subtle: Color,
    pub accent: Color,
    pub success: Color,
    pub error: Color,
    pub warning: Color,
    pub user_rail: Color,
    pub reasoning_rail: Color,
    pub composer_bg: Color,
    pub user_bg: Color,
    pub band: Color,
    pub band_selected: Color,
    pub tool_bg: Color,
    pub tool_success_bg: Color,
    pub tool_error_bg: Color,
    pub code_bg: Color,
    pub code_border: Color,
    pub link: Color,
    pub heading: Color,
    pub diff_add: Color,
    pub diff_del: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
}

impl Theme {
    pub fn of(palette: Palette) -> Self {
        let (text, selection_fg) = if palette.no_color {
            (Color::Reset, Color::Reset)
        } else {
            (palette.text, Color::Rgb(0xf5, 0xf7, 0xfa))
        };
        Self {
            text,
            dim: palette.dim,
            subtle: palette.faint,
            accent: palette.accent,
            success: palette.success,
            error: palette.failure,
            warning: palette.warning,
            user_rail: palette.user_rail,
            reasoning_rail: palette.thinking_rail,
            composer_bg: palette.composer_bg,
            user_bg: palette.composer_bg,
            band: palette.composer_bg,
            band_selected: palette.selection_bg,
            tool_bg: palette.card_bg,
            tool_success_bg: palette.card_success_bg,
            tool_error_bg: palette.card_failure_bg,
            code_bg: palette.faint,
            code_border: palette.dim,
            link: palette.accent,
            heading: palette.text,
            diff_add: palette.added,
            diff_del: palette.removed,
            selection_bg: palette.selection_bg,
            selection_fg,
        }
    }
}

/// The styles the UI asks for by role, so no renderer names a raw color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Styles {
    pub theme: Theme,
    /// False under `NO_COLOR`: keep layout and attributes, drop color.
    pub color: bool,
}

impl Styles {
    pub fn new(palette: Palette, no_color: bool) -> Self {
        Self {
            theme: Theme::of(palette),
            color: !no_color && !palette.no_color,
        }
    }

    /// The styles for the app's current theme.
    pub fn of(palette: Palette) -> Self {
        Self::new(palette, false)
    }

    /// A tinted role: the text color on a background band, both dropped when
    /// the terminal asks for no color.
    fn tint(&self, background: Color, foreground: Color) -> Style {
        if self.color {
            Style::new().bg(background).fg(foreground)
        } else {
            Style::new()
        }
    }

    fn paint(&self, color: Color) -> Style {
        if self.color {
            Style::new().fg(color)
        } else {
            Style::new()
        }
    }

    fn fill(&self, color: Color) -> Style {
        if self.color {
            Style::new().bg(color)
        } else {
            Style::new()
        }
    }

    pub fn text(&self) -> Style {
        self.paint(self.theme.text)
    }

    pub fn dim(&self) -> Style {
        self.paint(self.theme.dim)
    }

    pub fn subtle(&self) -> Style {
        self.paint(self.theme.subtle)
    }

    pub fn accent(&self) -> Style {
        self.paint(self.theme.accent)
    }

    pub fn success(&self) -> Style {
        self.paint(self.theme.success)
    }

    pub fn error(&self) -> Style {
        self.paint(self.theme.error)
    }

    pub fn warning(&self) -> Style {
        self.paint(self.theme.warning)
    }

    pub fn link(&self) -> Style {
        self.paint(self.theme.link)
            .add_modifier(Modifier::UNDERLINED)
    }

    pub fn heading(&self) -> Style {
        self.paint(self.theme.heading).add_modifier(Modifier::BOLD)
    }

    pub fn italic(&self) -> Style {
        self.paint(self.theme.text).add_modifier(Modifier::ITALIC)
    }

    pub fn bold(&self) -> Style {
        self.paint(self.theme.text).add_modifier(Modifier::BOLD)
    }

    pub fn strikethrough(&self) -> Style {
        self.paint(self.theme.dim)
            .add_modifier(Modifier::CROSSED_OUT)
    }

    pub fn user_rail(&self) -> Style {
        self.paint(self.theme.user_rail)
    }

    pub fn user(&self) -> Style {
        self.fill(self.theme.user_bg)
    }

    pub fn reasoning(&self) -> Style {
        self.paint(self.theme.reasoning_rail)
    }

    pub fn code(&self) -> Style {
        self.tint(self.theme.code_bg, self.theme.text)
    }

    /// An inline code span: the same role, so a NO_COLOR terminal keeps the
    /// code delimiter and drops only the tint.
    pub fn inline_code(&self) -> Style {
        self.code()
    }

    pub fn code_border(&self) -> Style {
        self.paint(self.theme.code_border)
    }

    pub fn selection(&self) -> Style {
        if self.color {
            Style::new()
                .bg(self.theme.selection_bg)
                .fg(self.theme.selection_fg)
        } else {
            Style::new().add_modifier(Modifier::REVERSED)
        }
    }

    pub fn band(&self, selected: bool) -> Style {
        self.fill(if selected {
            self.theme.band_selected
        } else {
            self.theme.band
        })
    }

    pub fn composer(&self) -> Style {
        self.fill(self.theme.composer_bg)
    }

    pub fn diff_add(&self) -> Style {
        self.paint(self.theme.diff_add)
    }

    pub fn diff_del(&self) -> Style {
        self.paint(self.theme.diff_del)
    }

    pub fn patch(&self, line: &str) -> Style {
        match line.chars().next() {
            Some('+') => self.diff_add(),
            Some('-') => self.diff_del(),
            Some('@') => self.dim(),
            _ => self.text(),
        }
    }

    /// Applies the selection background on top of an existing style.
    pub fn selected(&self, style: Style) -> Style {
        let selection = self.selection();
        let mut merged = style.bg(selection.bg.unwrap_or(Color::Reset));
        if let Some(fg) = selection.fg {
            merged = merged.fg(fg);
        }
        merged
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Palette, ThemeKind};

    fn dark() -> Palette {
        Palette::for_kind(ThemeKind::Dark)
    }

    fn plain() -> Palette {
        Palette::plain()
    }

    #[test]
    fn no_color_drops_every_color_but_keeps_attributes() {
        let styles = Styles::new(plain(), true);
        for style in [
            styles.text(),
            styles.dim(),
            styles.accent(),
            styles.user(),
            styles.band(true),
            styles.composer(),
            styles.selection(),
            styles.diff_add(),
            styles.code(),
            styles.inline_code(),
        ] {
            assert_eq!(style.fg, None, "{style:?}");
            assert_eq!(style.bg, None, "{style:?}");
        }
        assert!(styles.bold().add_modifier.contains(Modifier::BOLD));
        assert!(styles.italic().add_modifier.contains(Modifier::ITALIC));
        assert!(styles.selection().add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn palettes_keep_their_own_roles() {
        let dark_styles = Styles::new(dark(), false);
        let light_styles = Styles::new(Palette::for_kind(ThemeKind::Light), false);
        assert_ne!(dark_styles.text().fg, light_styles.text().fg);
        assert_ne!(dark_styles.composer().bg, light_styles.composer().bg);
        assert_eq!(dark_styles.theme.user_rail, dark().user_rail);
    }

    #[test]
    fn diff_lines_are_colored_by_their_prefix() {
        let palette = dark();
        let styles = Styles::new(palette, false);
        assert_eq!(styles.patch("+added").fg, Some(palette.added));
        assert_eq!(styles.patch("-removed").fg, Some(palette.removed));
        assert_eq!(styles.patch("@@ hunk @@").fg, Some(palette.dim));
        assert_eq!(styles.patch(" context").fg, Some(palette.text));
    }

    #[test]
    fn selected_keeps_the_selection_background() {
        let palette = dark();
        let styles = Styles::new(palette, false);
        let merged = styles.selected(styles.diff_add());
        assert_eq!(merged.bg, Some(palette.selection_bg));
    }
}
