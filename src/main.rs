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
    app.launch = minicore_tui::app::LaunchSettings {
        profile: args.profile.clone(),
        model: args.model.clone(),
        reasoning: args.reasoning,
    };
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
    let mut startup = Some(args.session.clone());
    // A closed agent stays a closed agent: the channel is drained once and then
    // left alone, so a `biased` select cannot spin on a permanently ready read.
    let mut agent_open = true;

    loop {
        let tick_due = app.wants_tick();
        tokio::select! {
            biased;
            incoming = incoming_rx.recv(), if agent_open => match incoming {
                Some(Incoming::Event(event)) => {
                    app.on_msg(Msg::Agent(event));
                    dirty = true;
                }
                Some(Incoming::Closed { status }) => {
                    app.on_msg(Msg::AgentClosed(status.unwrap_or_else(|| "agent closed".into())));
                    agent_open = false;
                    dirty = true;
                }
                None => agent_open = false,
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
            // Frame pacing exists to wait out the rest of a frame, never to
            // block an idle loop: with nothing to draw this branch must not be
            // armed, or an already-expired deadline would spin the loop.
            _ = tokio::time::sleep_until(next_frame), if dirty => {}
        }

        // Ctrl+G delegates the draft to an external editor. The terminal is the
        // loop's, so the reducer only files the request.
        if let Some(draft) = app.take_edit_request() {
            let result = match app.editor_command() {
                Ok(command) => {
                    minicore_tui::editor::edit_in_terminal(&mut guard, &command, &draft).await
                }
                Err(error) => Err(error),
            };
            let edited = match result {
                Ok(text) => text,
                Err(error) => {
                    app.notice(minicore_tui::transcript::NoticeLevel::Error, error);
                    // A failed editor must not cost the user what they typed.
                    Some(draft)
                }
            };
            app.on_msg(Msg::DraftEdited(edited));
            dirty = true;
        }

        // Startup opens or creates a session once; after a failure Enter or
        // `/new` retries, so a bad flag cannot loop on `session.create`.
        if app.ready
            && let Some(session) = startup.take()
        {
            let recent = args
                .continue_recent
                .then(|| pick_recent(&app.sessions, &workspace))
                .flatten();
            let action = match session.or_else(|| recent.cloned()) {
                Some(id) => minicore_tui::app::Action::OpenSession(id),
                None => minicore_tui::app::Action::NewSession,
            };
            app.on_action(action);
            dirty = true;
        }

        if dirty && guard.is_active() && tokio::time::Instant::now() >= next_frame {
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
