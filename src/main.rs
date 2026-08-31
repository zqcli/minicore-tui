use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use crossterm::event::{self, Event};

use minicore_tui::app::App;
use minicore_tui::args;
use minicore_tui::command::AppCommand;
use minicore_tui::event::AppEvent;
use minicore_tui::rpc::RpcError;
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

/// Phase 5 loop: terminal events become `AppEvent`s consumed by the single
/// `App::update`, side effects execute after each update, and rendering is
/// a pure read-only pass. The agent child and its RPC reader arrive in
/// Phase 6, so any RPC command reaching this loop is reported as a send
/// failure instead of being silently dropped.
fn run_fullscreen(
    guard: &mut TerminalGuard,
    theme_kind: minicore_tui::theme::ThemeKind,
) -> io::Result<()> {
    let terminal = guard.terminal_mut();
    let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut app = App::new(workspace);
    app.update(AppEvent::SetTheme(theme_kind));

    loop {
        // The main loop measures geometry and feeds it back to `update`;
        // the renderer never writes scroll state (spec 32).
        let size = terminal.size()?;
        let total = ui::transcript::total_lines(&app, size.width);
        let dock = ui::layout::dock_rows(&app, size.width, size.height);
        let visible = size.height.saturating_sub(dock) as usize;
        app.update(AppEvent::Viewport {
            total_lines: total,
            visible_rows: visible,
        });

        terminal.draw(|frame| ui::render(frame, &app))?;

        if event::poll(Duration::from_millis(100))? {
            let event = event::read()?;
            match event {
                Event::Key(_) | Event::Paste(_) | Event::Mouse(_) => {
                    let commands = app.update(AppEvent::Terminal(event));
                    if run_commands(&mut app, commands)? {
                        return Ok(());
                    }
                }
                // Resize is picked up by the next frame's Viewport event.
                _ => {}
            }
        }
        let commands = app.update(AppEvent::Tick);
        if run_commands(&mut app, commands)? {
            return Ok(());
        }
    }
}

/// Executes side effects without ever mutating `App` directly; failures
/// flow back as `AppEvent`s. Returns true when the loop should exit.
fn run_commands(app: &mut App, commands: Vec<AppCommand>) -> io::Result<bool> {
    for command in commands {
        match command {
            AppCommand::Quit => return Ok(true),
            AppCommand::KillChild => {} // no child before Phase 6
            AppCommand::Rpc(request) => {
                app.update(AppEvent::RpcSendFailed {
                    id: request.id,
                    error: RpcError::Closed,
                });
            }
        }
    }
    Ok(false)
}
