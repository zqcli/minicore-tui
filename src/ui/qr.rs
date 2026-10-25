//! A cached QR module matrix as terminal rows of half blocks.

use ratatui::style::Style;

/// Black on white in every theme: scanners need dark modules on a light ground.
pub const STYLE: Style = Style::new()
    .fg(ratatui::style::Color::Rgb(0, 0, 0))
    .bg(ratatui::style::Color::Rgb(255, 255, 255));

/// Light modules around the code, so a scanner finds its edges.
const QUIET: usize = 2;

/// Draws two cached modules per cell, surrounded by a quiet zone.
pub fn rows(matrix: &[bool], size: usize) -> Vec<String> {
    let side = size + 2 * QUIET;
    let modules = QUIET..QUIET + size;
    let dark = |x: usize, y: usize| {
        modules.contains(&x) && modules.contains(&y) && matrix[(y - QUIET) * size + x - QUIET]
    };
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
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_modules_draw_half_blocks_inside_a_quiet_zone() {
        assert_eq!(
            rows(&[true; 9], 3),
            ["       ", "  ███  ", "  ▀▀▀  ", "       "]
        );
        assert_eq!(
            rows(&[false, true, true, false], 2),
            ["      ", "  ▄▀  ", "      "]
        );
    }
}
