//! Process entry point: parse arguments, load config, start the agent, then run
//! the terminal loop.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{Event as TermEvent, EventStream};
use futures_util::StreamExt;
use tokio::sync::mpsc;

use minicore_tui::app::{App, Msg};
use minicore_tui::cli::{Args, USAGE};
use minicore_tui::config::{self, Home, Prepared, Request};
use minicore_tui::protocol::SessionInfo;
use minicore_tui::rpc::{Client, Incoming};
use minicore_tui::terminal;
use minicore_tui::ui::theme::{Palette, Styles};

/// Frame cap: coalesce bursts instead of drawing on every event.
const FRAME: Duration = Duration::from_millis(16);
/// Clock while something animates: one spinner frame.
const TICK: Duration = minicore_tui::ui::status::SPINNER_FRAME;

/// What made a frame necessary; the order indexes `Frames::draws`.
#[derive(Clone, Copy)]
enum Cause {
    Tick,
    Event,
    Key,
    Mouse,
    Resize,
}

const CAUSES: [&str; 5] = ["tick", "event", "key", "mouse", "resize"];

/// The frames wanted since the last draw, and, under `MINICORE_TUI_PERF_STATS`,
/// how many draws each cause led to and how long they took.
struct Frames {
    pending: u8,
    draws: [u64; CAUSES.len()],
    elapsed: Vec<Duration>,
    report: bool,
}

impl Frames {
    fn new() -> Self {
        Self {
            pending: 1 << Cause::Event as u8,
            draws: [0; CAUSES.len()],
            elapsed: Vec::new(),
            report: std::env::var_os("MINICORE_TUI_PERF_STATS").is_some(),
        }
    }

    fn mark(&mut self, cause: Cause) {
        self.pending |= 1 << cause as u8;
    }

    fn wanted(&self) -> bool {
        self.pending != 0
    }

    fn drawn(&mut self, elapsed: Duration) {
        for (cause, count) in self.draws.iter_mut().enumerate() {
            *count += u64::from(self.pending >> cause & 1);
        }
        self.pending = 0;
        if self.report {
            self.elapsed.push(elapsed);
        }
    }

    fn print(&mut self, app: &App) {
        if !self.report {
            return;
        }
        let causes: Vec<String> = CAUSES
            .iter()
            .zip(self.draws)
            .map(|(name, count)| format!("{name}={count}"))
            .collect();
        self.elapsed.sort();
        let at = |p: f64| {
            let last = self.elapsed.len().saturating_sub(1);
            self.elapsed
                .get((last as f64 * p) as usize)
                .map_or(0, Duration::as_micros)
        };
        let layout = &app.layout.stats;
        let rss = std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|statm| statm.split(' ').nth(1)?.parse::<u64>().ok())
            .map_or(0, |pages| pages * 4);
        eprintln!(
            "draws {{{}}} total={} draw_us p50={} p95={} max={} blocks_rendered={} rows_rendered={} prepare_us_max={} rss_kb={rss}",
            causes.join(","),
            self.elapsed.len(),
            at(0.5),
            at(0.95),
            at(1.0),
            layout.blocks_rendered,
            layout.rows_rendered,
            layout.prepare_max.as_micros(),
        );
    }
}

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
    let home = match Home::resolve(args.home.as_deref()) {
        Ok(home) => home,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };
    let request = Request {
        agent: args.agent.as_deref(),
        agent_config: args.agent_config.as_deref(),
        profile: args.profile.as_deref(),
        model: args.model.as_deref(),
    };
    let has_env = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    let launch = match config::prepare(&home, &request, &has_env) {
        Ok(Prepared::Launch(launch)) => launch,
        Ok(Prepared::FirstRun(message)) => {
            print!("{message}");
            return 1;
        }
        Err(error) => {
            eprintln!("minicore-tui: {error}");
            return 2;
        }
    };
    let theme = args.theme.unwrap_or(launch.ui.theme);
    let workspace = args
        .workspace
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<Msg>();
    let (incoming_tx, mut incoming_rx) = mpsc::channel::<Incoming>(256);
    let client = match Client::spawn(&launch.agent_binary, &launch.agent_config, incoming_tx) {
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
    app.notifier.bell = launch.ui.bell;
    app.editor = launch.ui.editor;
    app.composed = launch.composed;
    app.composer = minicore_tui::app::composer::Composer::with_history_file(home.history_file());
    app.home = Some(home);
    app.styles = Styles::of(Palette::for_kind(theme));

    let boot = {
        let client = client.clone();
        let tx = msg_tx.clone();
        tokio::spawn(async move { minicore_tui::app::tasks::bootstrap(client, tx).await })
    };
    let mut events = EventStream::new();
    let mut frames = Frames::new();
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
                    frames.mark(Cause::Event);
                }
                Some(Incoming::Closed { status }) => {
                    app.on_msg(Msg::AgentClosed(status.unwrap_or_else(|| "agent closed".into())));
                    agent_open = false;
                    frames.mark(Cause::Event);
                }
                None => agent_open = false,
            },
            message = msg_rx.recv() => {
                if let Some(message) = message {
                    app.on_msg(message);
                    frames.mark(Cause::Event);
                }
            }
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(TermEvent::Key(key))) => {
                        if key.is_press() {
                            if let Some(action) = app.on_key(key) {
                                app.on_action(action);
                            }
                            frames.mark(Cause::Key);
                        }
                    }
                    Some(Ok(TermEvent::Mouse(mouse))) => {
                        if app.on_mouse(mouse) {
                            frames.mark(Cause::Mouse);
                        }
                    }
                    Some(Ok(TermEvent::Paste(text))) => {
                        app.on_paste(&text);
                        frames.mark(Cause::Key);
                    }
                    Some(Ok(TermEvent::Resize(_, _))) => frames.mark(Cause::Resize),
                    // The terminal is gone; polling it again would spin.
                    None => app.quit = true,
                    _ => {}
                }
            }
            _ = tokio::time::sleep_until(next_tick), if tick_due => {
                app.tick();
                next_tick = tokio::time::Instant::now() + TICK;
                frames.mark(Cause::Tick);
            }
            // Frame pacing exists to wait out the rest of a frame, never to
            // block an idle loop: with nothing to draw this branch must not be
            // armed, or an already-expired deadline would spin the loop.
            _ = tokio::time::sleep_until(next_frame), if frames.wanted() => {}
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
                    app.notice(minicore_tui::app::NoticeLevel::Error, error);
                    // A failed editor must not cost the user what they typed.
                    Some(draft)
                }
            };
            app.on_msg(Msg::DraftEdited(edited));
            frames.mark(Cause::Event);
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
            frames.mark(Cause::Event);
        }

        if frames.wanted() && guard.is_active() && tokio::time::Instant::now() >= next_frame {
            let started = Instant::now();
            if let Err(error) = guard
                .terminal()
                .draw(|frame| minicore_tui::ui::draw(frame, &mut app))
            {
                eprintln!("draw failed: {error}");
                break;
            }
            frames.drawn(started.elapsed());
            next_frame = tokio::time::Instant::now() + FRAME;
        }
        app.sync_terminal();
        if app.quit {
            break;
        }
    }

    let _ = client.shutdown().await;
    let _ = guard.restore();
    frames.print(&app);
    print!("{}", app.exit_output());
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
