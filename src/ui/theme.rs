//! The two palettes and the rule that decides whether color is allowed.

use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Palette {
    #[default]
    Dark,
    Light,
}

impl Palette {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

/// Every color the UI can use. `None` means the terminal default.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    pub user_bg: Color,
    pub band: Color,
    pub band_selected: Color,
    pub composer_bg: Color,
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

pub const DARK: Theme = Theme {
    text: Color::Rgb(0xdd, 0xdd, 0xe4),
    dim: Color::Rgb(0x7c, 0x7f, 0x93),
    subtle: Color::Rgb(0x4c, 0x4f, 0x61),
    accent: Color::Rgb(0x9d, 0x7c, 0xf0),
    success: Color::Rgb(0x74, 0xc9, 0x8d),
    error: Color::Rgb(0xe8, 0x6a, 0x7c),
    warning: Color::Rgb(0xe5, 0xb5, 0x67),
    user_rail: Color::Rgb(0x5a, 0x9d, 0xf5),
    reasoning_rail: Color::Rgb(0xa8, 0x8a, 0xd8),
    user_bg: Color::Rgb(0x2b, 0x2f, 0x45),
    band: Color::Rgb(0x32, 0x35, 0x47),
    band_selected: Color::Rgb(0x40, 0x42, 0x56),
    composer_bg: Color::Rgb(0x2a, 0x2c, 0x3a),
    tool_bg: Color::Rgb(0x26, 0x28, 0x34),
    tool_success_bg: Color::Rgb(0x24, 0x2c, 0x2c),
    tool_error_bg: Color::Rgb(0x33, 0x25, 0x29),
    code_bg: Color::Rgb(0x24, 0x26, 0x32),
    code_border: Color::Rgb(0x3a, 0x3d, 0x4e),
    link: Color::Rgb(0x71, 0xb3, 0xe8),
    heading: Color::Rgb(0xf0, 0xf0, 0xf6),
    diff_add: Color::Rgb(0x74, 0xc9, 0x8d),
    diff_del: Color::Rgb(0xe8, 0x6a, 0x7c),
    selection_bg: Color::Rgb(0x44, 0x49, 0x71),
    selection_fg: Color::Rgb(0xff, 0xff, 0xff),
};

pub const LIGHT: Theme = Theme {
    text: Color::Rgb(0x2c, 0x2e, 0x38),
    dim: Color::Rgb(0x6f, 0x72, 0x84),
    subtle: Color::Rgb(0xa8, 0xaa, 0xb6),
    accent: Color::Rgb(0x6a, 0x46, 0xc8),
    success: Color::Rgb(0x2f, 0x8a, 0x4f),
    error: Color::Rgb(0xc0, 0x39, 0x4b),
    warning: Color::Rgb(0x9a, 0x6b, 0x10),
    user_rail: Color::Rgb(0x2f, 0x6f, 0xd0),
    reasoning_rail: Color::Rgb(0x7a, 0x5c, 0xb0),
    user_bg: Color::Rgb(0xe8, 0xee, 0xfa),
    band: Color::Rgb(0xee, 0xef, 0xf4),
    band_selected: Color::Rgb(0xdd, 0xe0, 0xee),
    composer_bg: Color::Rgb(0xf1, 0xf2, 0xf7),
    tool_bg: Color::Rgb(0xf5, 0xf5, 0xf8),
    tool_success_bg: Color::Rgb(0xef, 0xf7, 0xf1),
    tool_error_bg: Color::Rgb(0xfb, 0xef, 0xf1),
    code_bg: Color::Rgb(0xf3, 0xf3, 0xf8),
    code_border: Color::Rgb(0xd0, 0xd2, 0xdd),
    link: Color::Rgb(0x1f, 0x6f, 0xb8),
    heading: Color::Rgb(0x18, 0x1a, 0x22),
    diff_add: Color::Rgb(0x2f, 0x8a, 0x4f),
    diff_del: Color::Rgb(0xc0, 0x39, 0x4b),
    selection_bg: Color::Rgb(0xd2, 0xd8, 0xf0),
    selection_fg: Color::Rgb(0x14, 0x16, 0x1e),
};

/// The styles the UI asks for by role, so no renderer names a raw color.
#[derive(Clone, Copy, Debug)]
pub struct Styles {
    pub theme: Theme,
    /// False under `NO_COLOR`: keep layout and attributes, drop color.
    pub color: bool,
}

impl Styles {
    pub fn new(palette: Palette, no_color: bool) -> Self {
        let theme = match palette {
            Palette::Dark => DARK,
            Palette::Light => LIGHT,
        };
        Self {
            theme,
            color: !no_color,
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
        self.paint(self.theme.text)
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

    #[test]
    fn parses_the_two_palette_names() {
        assert_eq!(Palette::parse("dark"), Some(Palette::Dark));
        assert_eq!(Palette::parse("light"), Some(Palette::Light));
        assert_eq!(Palette::parse("solarized"), None);
        assert_eq!(Palette::default(), Palette::Dark);
        assert_eq!(Palette::Light.name(), "light");
    }

    #[test]
    fn no_color_drops_every_color_but_keeps_attributes() {
        let styles = Styles::new(Palette::Dark, true);
        for style in [
            styles.text(),
            styles.dim(),
            styles.accent(),
            styles.user(),
            styles.band(true),
            styles.composer(),
            styles.diff_add(),
        ] {
            assert_eq!(style.fg, None);
            assert_eq!(style.bg, None);
        }
        assert!(styles.bold().add_modifier == Modifier::BOLD);
        assert_eq!(styles.link().add_modifier, Modifier::UNDERLINED);
        assert_eq!(styles.selection().add_modifier, Modifier::REVERSED);
    }

    #[test]
    fn color_palettes_differ_and_are_used_when_allowed() {
        let dark = Styles::new(Palette::Dark, false);
        let light = Styles::new(Palette::Light, false);
        assert_eq!(dark.text().fg, Some(DARK.text));
        assert_eq!(light.text().fg, Some(LIGHT.text));
        assert_ne!(DARK.text, LIGHT.text);
        assert_ne!(dark.composer().bg, light.composer().bg);
    }

    #[test]
    fn diff_lines_are_colored_by_their_prefix() {
        let styles = Styles::new(Palette::Dark, false);
        assert_eq!(styles.patch("+added").fg, Some(DARK.diff_add));
        assert_eq!(styles.patch("-removed").fg, Some(DARK.diff_del));
        assert_eq!(styles.patch("@@ hunk @@").fg, Some(DARK.dim));
        assert_eq!(styles.patch(" context").fg, Some(DARK.text));
    }

    #[test]
    fn selected_keeps_the_foreground_roles() {
        let styles = Styles::new(Palette::Dark, false);
        let merged = styles.selected(styles.diff_add());
        assert_eq!(merged.bg, Some(DARK.selection_bg));
        assert_eq!(merged.fg, Some(DARK.selection_fg));
    }
}
