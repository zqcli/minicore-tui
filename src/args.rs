use std::fmt;

use crate::theme::ThemeKind;

pub const USAGE: &str = "\
minicore-tui — a coding agent TUI for minicore-agent (phase 0 scaffold)

Usage: minicore-tui [OPTIONS]

Options:
  --theme <dark|light>  Color theme [default: dark]
  --version             Print version
  --help                Print help
";

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct Args {
    pub theme: ThemeKind,
    pub help: bool,
    pub version: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ArgsError {
    UnknownFlag(String),
    MissingValue(String),
    InvalidTheme(String),
}

impl fmt::Display for ArgsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownFlag(flag) => write!(f, "unknown flag `{flag}`"),
            Self::MissingValue(flag) => write!(f, "flag `{flag}` requires a value"),
            Self::InvalidTheme(value) => {
                write!(f, "invalid theme `{value}` (expected `dark` or `light`)")
            }
        }
    }
}

/// Parses CLI flags. Callers pass the arguments without `argv[0]`.
pub fn parse<I>(args: I) -> Result<Args, ArgsError>
where
    I: IntoIterator<Item = String>,
{
    let mut parsed = Args::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--theme=") {
            parsed.theme = parse_theme(value)?;
            continue;
        }
        match arg.as_str() {
            "--theme" => {
                let value = args
                    .next()
                    .ok_or_else(|| ArgsError::MissingValue("--theme".to_owned()))?;
                parsed.theme = parse_theme(&value)?;
            }
            "--help" | "-h" => parsed.help = true,
            "--version" | "-V" => parsed.version = true,
            other => return Err(ArgsError::UnknownFlag(other.to_owned())),
        }
    }
    Ok(parsed)
}

fn parse_theme(value: &str) -> Result<ThemeKind, ArgsError> {
    match value {
        "dark" => Ok(ThemeKind::Dark),
        "light" => Ok(ThemeKind::Light),
        other => Err(ArgsError::InvalidTheme(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_flags(args: &[&str]) -> Result<Args, ArgsError> {
        parse(args.iter().map(|flag| flag.to_string()))
    }

    #[test]
    fn defaults_are_dark_theme_without_actions() {
        assert_eq!(parse_flags(&[]).unwrap(), Args::default());
        assert_eq!(Args::default().theme, ThemeKind::Dark);
    }

    #[test]
    fn parses_theme_flag_in_both_forms() {
        assert_eq!(
            parse_flags(&["--theme", "light"]).unwrap().theme,
            ThemeKind::Light
        );
        assert_eq!(
            parse_flags(&["--theme=light"]).unwrap().theme,
            ThemeKind::Light
        );
        assert_eq!(
            parse_flags(&["--theme", "dark"]).unwrap().theme,
            ThemeKind::Dark
        );
    }

    #[test]
    fn parses_help_and_version() {
        assert!(parse_flags(&["--help"]).unwrap().help);
        assert!(parse_flags(&["-h"]).unwrap().help);
        assert!(parse_flags(&["--version"]).unwrap().version);
        assert!(parse_flags(&["-V"]).unwrap().version);
    }

    #[test]
    fn rejects_unknown_flags() {
        assert_eq!(
            parse_flags(&["--bogus"]),
            Err(ArgsError::UnknownFlag("--bogus".to_owned()))
        );
    }

    #[test]
    fn rejects_missing_theme_value() {
        assert_eq!(
            parse_flags(&["--theme"]),
            Err(ArgsError::MissingValue("--theme".to_owned()))
        );
    }

    #[test]
    fn rejects_invalid_theme() {
        assert_eq!(
            parse_flags(&["--theme", "blue"]),
            Err(ArgsError::InvalidTheme("blue".to_owned()))
        );
    }
}
