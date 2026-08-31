use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use minicore_tui::app::App;
use minicore_tui::args;
use minicore_tui::event::AppEvent;
use minicore_tui::terminal::{PanicHookGuard, TerminalGuard};
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

/// Phase 3 loop: paints the fullscreen conversation (constructed, not yet
/// connected to an agent) and repaints on resize or a ~10 Hz tick. Only
/// `q` / `Ctrl+C` quit; input handling arrives in Phase 5.
fn run_fullscreen(
    guard: &mut TerminalGuard,
    theme_kind: minicore_tui::theme::ThemeKind,
) -> io::Result<()> {
    let terminal = guard.terminal_mut();
    let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut app = App::new(workspace);
    app.update(AppEvent::SetTheme(theme_kind));

    loop {
        terminal.draw(|frame| ui::render(frame, &app))?;
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if is_quit(&key) => return Ok(()),
                Event::Resize(..) | Event::Key(_) => {}
                _ => {}
            }
        }
        app.update(AppEvent::Tick);
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
