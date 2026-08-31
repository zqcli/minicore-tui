use std::io;
use std::process::ExitCode;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use minicore_tui::args;
use minicore_tui::terminal::{PanicHookGuard, TerminalGuard};
use minicore_tui::theme::{Theme, ThemeKind};
use minicore_tui::ui;

fn main() -> ExitCode {
    let opts = match args::parse(std::env::args().skip(1)) {
        Ok(opts) => opts,
        Err(error) => {
            eprintln!("minicore-tui: {error}");
            eprintln!("Try 'minicore-tui --help' for usage.");
            return ExitCode::from(2);
        }
    };
    if opts.help {
        println!("{}", args::USAGE);
        return ExitCode::SUCCESS;
    }
    if opts.version {
        println!("minicore-tui {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    let _panic_hook = PanicHookGuard::install();
    let mut guard = match TerminalGuard::enter() {
        Ok(guard) => guard,
        Err(error) => {
            eprintln!("minicore-tui: failed to start terminal: {error}");
            return ExitCode::FAILURE;
        }
    };
    let run_result = run_fullscreen(&mut guard, opts.theme);
    let restore_result = guard.restore();
    match (run_result, restore_result) {
        (Ok(()), Ok(())) => ExitCode::SUCCESS,
        (Err(error), _) => {
            eprintln!("minicore-tui: {error}");
            ExitCode::FAILURE
        }
        (Ok(()), Err(error)) => {
            eprintln!("minicore-tui: failed to restore terminal: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Draws the empty fullscreen once and repaints on resize until `q` or
/// `Ctrl+C` quits. Phase 0 has no app state yet, so the render is stateless.
fn run_fullscreen(guard: &mut TerminalGuard, theme_kind: ThemeKind) -> io::Result<()> {
    let theme = Theme::for_kind(theme_kind);
    let terminal = guard.terminal_mut();
    terminal.draw(|frame| ui::render(frame, &theme))?;
    loop {
        match event::read()? {
            Event::Key(key) => {
                if is_quit(&key) {
                    return Ok(());
                }
            }
            Event::Resize(..) => {
                terminal.draw(|frame| ui::render(frame, &theme))?;
            }
            _ => {}
        }
    }
}

fn is_quit(key: &KeyEvent) -> bool {
    if key.kind != KeyEventKind::Press {
        return false;
    }
    match key.code {
        KeyCode::Char('q') => true,
        KeyCode::Char('c') => key.modifiers.contains(KeyModifiers::CONTROL),
        _ => false,
    }
}
