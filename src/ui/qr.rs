//! A QR code as terminal rows of half blocks.

use qrcode::{Color, EcLevel, QrCode};
use ratatui::style::Style;

/// Black on white in every theme: scanners need dark modules on a light ground.
pub const STYLE: Style = Style::new()
    .fg(ratatui::style::Color::Rgb(0, 0, 0))
    .bg(ratatui::style::Color::Rgb(255, 255, 255));

/// Light modules around the code, so a scanner finds its edges.
const QUIET: usize = 2;

/// Rows of `▀`, `▄`, `█` and spaces drawing `text` (two modules per row) with
/// a quiet zone, or `None` when it is too long for a code.
pub fn rows(text: &str) -> Option<Vec<String>> {
    let code = QrCode::with_error_correction_level(text, EcLevel::L).ok()?;
    let size = code.width();
    let side = size + 2 * QUIET;
    let modules = QUIET..QUIET + size;
    let dark = |x: usize, y: usize| {
        modules.contains(&x) && modules.contains(&y) && code[(x - QUIET, y - QUIET)] == Color::Dark
    };
    Some(
        (0..side)
            .step_by(2)
            .map(|y| {
                (0..side)
                    .map(|x| match (dark(x, y), dark(x, y + 1)) {
                        (true, true) => '█',
                        (true, false) => '▀',
                        (false, true) => '▄',
                        (false, false) => ' ',
                    })
                    .collect()
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_is_drawn_in_half_blocks_inside_a_quiet_zone() {
        let rows = rows("http://192.168.1.20:8765/#pair=ABCDEFGH").unwrap();
        // Version 3 is 29 modules; two quiet modules on each side make 33.
        assert_eq!(rows.len(), 17);
        assert!(rows.iter().all(|row| row.chars().count() == 33));
        assert!(rows[0].chars().all(|c| c == ' '), "{:?}", rows[0]);
        for row in &rows {
            assert!(row.starts_with("  ") && row.ends_with("  "), "{row:?}");
        }
        // The finder pattern's top-left module is dark: full blocks start the third row.
        assert!(rows[1].starts_with("  █"), "{:?}", rows[1]);
        assert!(
            rows.iter()
                .flat_map(|row| row.chars())
                .all(|c| " ▀▄█".contains(c))
        );
    }

    #[test]
    fn a_text_too_long_for_a_code_has_none() {
        assert!(rows(&"x".repeat(3000)).is_none());
    }
}
