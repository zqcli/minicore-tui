//! Two flat palettes. Colors stay close to the reference screenshot: a dark
//! page, a slightly lifted composer band, a purple thinking rail and a blue
//! user rail. `NO_COLOR` keeps attributes but drops color.

use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeKind {
    #[default]
    Dark,
    Light,
}

impl ThemeKind {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

/// One resolved set of UI colors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// Ordinary text.
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
    pub no_color: bool,
}

pub const DARK_BG: Color = Color::Reset;

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
            success: Color::Rgb(0x7a, 0xc0, 0x8a),
            failure: Color::Rgb(0xe0, 0x6c, 0x75),
            warning: Color::Rgb(0xe0, 0xb1, 0x62),
            added: Color::Rgb(0x7a, 0xc0, 0x8a),
            removed: Color::Rgb(0xe0, 0x6c, 0x75),
            hunk: Color::Rgb(0x8a, 0x90, 0xa0),
            card_bg: Color::Rgb(0x26, 0x28, 0x30),
            card_success_bg: Color::Rgb(0x1f, 0x2b, 0x25),
            card_failure_bg: Color::Rgb(0x31, 0x20, 0x24),
            no_color: false,
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
            success: Color::Rgb(0x1f, 0x7a, 0x3a),
            failure: Color::Rgb(0xb8, 0x2a, 0x35),
            warning: Color::Rgb(0x9a, 0x6a, 0x00),
            added: Color::Rgb(0x1f, 0x7a, 0x3a),
            removed: Color::Rgb(0xb8, 0x2a, 0x35),
            hunk: Color::Rgb(0x6b, 0x72, 0x80),
            card_bg: Color::Rgb(0xee, 0xf0, 0xf4),
            card_success_bg: Color::Rgb(0xe5, 0xf2, 0xe8),
            card_failure_bg: Color::Rgb(0xf8, 0xe6, 0xe8),
            no_color: false,
        }
    }

    /// A palette that keeps attributes but drops color, per `NO_COLOR`.
    pub fn plain() -> Self {
        let mut palette = Self::dark();
        palette.no_color = true;
        for color in [
            &mut palette.text,
            &mut palette.dim,
            &mut palette.faint,
            &mut palette.accent,
            &mut palette.user_rail,
            &mut palette.thinking_rail,
            &mut palette.composer_bg,
            &mut palette.selection_bg,
            &mut palette.success,
            &mut palette.failure,
            &mut palette.warning,
            &mut palette.added,
            &mut palette.removed,
            &mut palette.hunk,
            &mut palette.card_bg,
            &mut palette.card_success_bg,
            &mut palette.card_failure_bg,
        ] {
            *color = Color::Reset;
        }
        palette
    }

    pub fn for_kind(kind: ThemeKind) -> Self {
        if std::env::var_os("NO_COLOR").is_some() {
            return Self::plain();
        }
        match kind {
            ThemeKind::Dark => Self::dark(),
            ThemeKind::Light => Self::light(),
        }
    }

    fn paint(self, color: Color) -> Style {
        if self.no_color {
            Style::default()
        } else {
            Style::default().fg(color)
        }
    }

    pub fn text(self) -> Style {
        self.paint(self.text)
    }

    pub fn dim(self) -> Style {
        self.paint(self.dim)
    }

    pub fn faint(self) -> Style {
        self.paint(self.faint)
    }

    pub fn accent(self) -> Style {
        self.paint(self.accent)
    }

    pub fn success(self) -> Style {
        self.paint(self.success)
    }

    pub fn failure(self) -> Style {
        self.paint(self.failure)
    }

    pub fn warning(self) -> Style {
        self.paint(self.warning)
    }

    pub fn added(self) -> Style {
        self.paint(self.added)
    }

    pub fn removed(self) -> Style {
        self.paint(self.removed)
    }

    pub fn hunk(self) -> Style {
        self.paint(self.hunk)
    }

    pub fn italic_dim(self) -> Style {
        self.dim().add_modifier(Modifier::ITALIC)
    }

    pub fn bold(self) -> Style {
        self.text().add_modifier(Modifier::BOLD)
    }

    pub fn selected(self) -> Style {
        let style = Style::default().add_modifier(Modifier::BOLD);
        if self.no_color {
            style
        } else {
            style.fg(self.text).bg(self.selection_bg)
        }
    }

    pub fn composer(self) -> Style {
        if self.no_color {
            Style::default()
        } else {
            Style::default().fg(self.text).bg(self.composer_bg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_kinds_roundtrip_through_their_config_names() {
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            assert_eq!(ThemeKind::parse(kind.label()), Some(kind));
        }
        assert_eq!(ThemeKind::parse("sepia"), None);
    }

    #[test]
    fn the_plain_palette_drops_color_but_keeps_emphasis() {
        let palette = Palette::plain();
        assert!(palette.no_color);
        assert_eq!(palette.text(), Style::default());
        assert!(palette.bold().add_modifier.contains(Modifier::BOLD));
    }
}
