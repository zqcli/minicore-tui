//! Process entry point: parse arguments, load config, start the agent, then run
//! the terminal loop.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{Event as TermEvent, EventStream};
use futures_util::StreamExt;
use tokio::sync::mpsc;

use minicore_tui::app::keys::Input;
use minicore_tui::app::{App, Msg};
use minicore_tui::cli::{Args, USAGE};
use minicore_tui::config::Config;
use minicore_tui::protocol::SessionInfo;
use minicore_tui::rpc::{Client, Incoming};
use minicore_tui::terminal;
use minicore_tui::theme::{Palette, ThemeKind};

/// Frame cap: coalesce bursts instead of drawing on every event.
const FRAME: Duration = Duration::from_millis(16);
/// Clock while something animates.
const TICK: Duration = Duration::from_millis(100);

fn main() -> std::io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let code = runtime.block_on(run());
    std::process::exit(code);
}

async fn run() -> i32 {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}\n\n{USAGE}");
            return 2;
        }
    };
    if args.help {
        print!("{USAGE}");
        return 0;
    }
    if args.version {
        println!("minicore-tui {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    let config = Config::load();
    let args = args.with_config(config.clone());
    let theme = args.theme.unwrap_or(ThemeKind::Dark);
    let Some(agent_config) = args.agent_config.clone() else {
        eprintln!(
            "an agent config is required: pass --config or set `agent_config` in the TUI config"
        );
        return 2;
    };
    let agent = args
        .agent
        .clone()
        .unwrap_or_else(minicore_tui::rpc::default_agent_bin);
    let workspace = args
        .workspace
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<Msg>();
    let (incoming_tx, mut incoming_rx) = mpsc::channel::<Incoming>(256);
    let client = match Client::spawn(&agent, &agent_config, incoming_tx) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };

    terminal::install_panic_hook();
    let mut guard = match terminal::Guard::enter() {
        Ok(guard) => guard,
        Err(error) => {
            eprintln!("terminal setup failed: {error}");
            return 1;
        }
    };

    let mut app = App::new(client.clone(), msg_tx.clone(), workspace.clone());
    app.theme = theme;
    app.palette = Palette::for_kind(theme);

    let boot = {
        let client = client.clone();
        let tx = msg_tx.clone();
        tokio::spawn(async move { minicore_tui::app::tasks::bootstrap(client, tx).await })
    };
    let mut events = EventStream::new();
    let mut dirty = true;
    let mut next_frame = tokio::time::Instant::now();
    let mut next_tick = tokio::time::Instant::now() + TICK;
    let mut open_selection = (args.session.clone(), args.continue_recent);

    loop {
        let tick_due = app.wants_tick();
        tokio::select! {
            biased;
            incoming = incoming_rx.recv() => match incoming {
                Some(Incoming::Event(event)) => {
                    app.on_msg(Msg::Agent(event));
                    dirty = true;
                }
                Some(Incoming::Closed { status }) => {
                    app.on_msg(Msg::AgentClosed(status.unwrap_or_else(|| "agent closed".into())));
                    dirty = true;
                }
                None => {}
            },
            message = msg_rx.recv() => {
                if let Some(message) = message {
                    app.on_msg(message);
                    dirty = true;
                }
            }
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(TermEvent::Key(key))) => {
                        if key.is_press() {
                            match app.on_key(key) {
                                Input::Action(action) => {
                                    app.on_action(action);
                                }
                                Input::Edited | Input::Consumed => {}
                                Input::Ignored => {}
                            }
                            dirty = true;
                        }
                    }
                    Some(Ok(TermEvent::Mouse(mouse))) => {
                        if let Input::Action(action) = app.on_mouse(mouse) {
                            app.on_action(action);
                        }
                        dirty = true;
                    }
                    Some(Ok(TermEvent::Paste(text))) => {
                        app.on_paste(&text);
                        dirty = true;
                    }
                    Some(Ok(TermEvent::Resize(_, _))) => dirty = true,
                    Some(Err(_)) | None => {}
                    _ => {}
                }
            }
            _ = tokio::time::sleep_until(next_tick), if tick_due => {
                app.tick();
                next_tick = tokio::time::Instant::now() + TICK;
                dirty = true;
            }
            _ = tokio::time::sleep_until(next_frame) => {}
        }

        // An open session request from the CLI is applied once, after bootstrap.
        if app.ready && (open_selection.0.is_some() || open_selection.1) && app.session.is_none() {
            if let Some(id) = open_selection.0.take() {
                app.on_action(minicore_tui::app::Action::OpenSession(id));
            } else {
                open_selection.1 = false;
                if let Some(session) = pick_recent(&app.sessions, &workspace) {
                    app.on_action(minicore_tui::app::Action::OpenSession(session.clone()));
                } else {
                    app.on_action(minicore_tui::app::Action::NewSession);
                }
            }
            dirty = true;
        } else if app.ready && app.session.is_none() && !open_selection.1 {
            app.on_action(minicore_tui::app::Action::NewSession);
            dirty = true;
        }

        if dirty && tokio::time::Instant::now() >= next_frame {
            if let Err(error) = guard
                .terminal()
                .draw(|frame| minicore_tui::ui::draw(frame, &mut app))
            {
                eprintln!("draw failed: {error}");
                break;
            }
            dirty = false;
            next_frame = tokio::time::Instant::now() + FRAME;
        }
        if app.quit {
            break;
        }
    }

    let _ = client.shutdown().await;
    let _ = guard.restore();
    boot.abort();
    0
}

/// The most recent session for this workspace, for `--continue`.
fn pick_recent<'a>(sessions: &'a [SessionInfo], workspace: &Path) -> Option<&'a String> {
    sessions
        .iter()
        .filter(|session| session.workspace == workspace.to_string_lossy())
        .max_by(|left, right| left.updated_at.cmp(&right.updated_at))
        .map(|session| &session.session_id)
}
