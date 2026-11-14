//! Process entry point: prepare a persistent Host and run the terminal loop.

use std::path::Path;
use std::time::{Duration, Instant};

use crossterm::event::{Event as TermEvent, EventStream};
#[cfg(not(windows))]
use futures_util::StreamExt;
use tokio::sync::mpsc;

use minicore_host_core::{AgentSummary, SessionSummary};
use minicore_host_protocol::{
    AgentId, HostLifecycle, HostShutdownRequest, HostShutdownStrategy, ModelRef, OperationId,
    SessionId, SessionRef,
};
use minicore_tui::app::{App, Msg};
use minicore_tui::cli::{Args, USAGE};
use minicore_tui::config::{self, ConfigDirectory, Prepared};
use minicore_tui::host::Hosted;
use minicore_tui::terminal;
use minicore_tui::ui::theme::{Palette, Styles};

/// Frame cap: coalesce bursts instead of drawing on every event.
const FRAME: Duration = Duration::from_millis(16);
/// Clock while the Working line or a card animates: one spinner frame.
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

fn main() -> std::process::ExitCode {
    std::panic::set_hook(Box::new(|_| {
        eprintln!("minicore-tui: internal failure; controlled Host shutdown is not established");
    }));
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            eprintln!("minicore-tui: cannot initialize the execution runtime");
            return std::process::ExitCode::FAILURE;
        }
    };
    let code = runtime.block_on(run());
    runtime.shutdown_background();
    std::process::ExitCode::from(code as u8)
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
    let directory =
        match ConfigDirectory::resolve(args.config_dir.as_deref(), args.data_root.as_deref()) {
            Ok(directory) => directory,
            Err(error) => {
                eprintln!("{error}");
                return 2;
            }
        };
    let launch = match config::prepare(&directory, args.workspace.as_deref()) {
        Ok(Prepared::Launch(launch)) => *launch,
        Ok(Prepared::FirstRun(message)) => {
            println!("{message}");
            return 1;
        }
        Err(error) => {
            eprintln!("minicore-tui: {error}");
            return 2;
        }
    };
    let selected_agent = match args
        .agent_id
        .as_deref()
        .map(str::parse::<AgentId>)
        .transpose()
    {
        Ok(value) => value,
        Err(error) => {
            eprintln!("invalid --agent-id: {error}");
            return 2;
        }
    };
    let selected_session = match args
        .session
        .as_deref()
        .map(str::parse::<SessionId>)
        .transpose()
    {
        Ok(value) => value,
        Err(error) => {
            eprintln!("invalid --session: {error}");
            return 2;
        }
    };
    let model = match args
        .model
        .as_deref()
        .map(str::parse::<ModelRef>)
        .transpose()
    {
        Ok(value) => value,
        Err(error) => {
            eprintln!("invalid --model: {error}");
            return 2;
        }
    };
    let theme = args.theme.unwrap_or(launch.ui.theme);
    let workspace = match args
        .workspace
        .clone()
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)
        .and_then(std::fs::canonicalize)
    {
        Ok(workspace) => workspace,
        Err(error) => {
            eprintln!("cannot resolve workspace: {error}");
            return 2;
        }
    };
    if workspace.to_str().is_none() {
        eprintln!("workspace must be representable as UTF-8 for native Host configuration");
        return 2;
    }
    let mut signals = match ExitSignals::new() {
        Ok(signals) => signals,
        Err(error) => {
            eprintln!("cannot initialize shutdown signals: {error}");
            return 1;
        }
    };
    let mut hosted = match Hosted::open(&launch).await {
        Ok(hosted) => hosted,
        Err(error) => {
            eprintln!("minicore-tui: {error}");
            return 2;
        }
    };
    let client = hosted.client();
    let (msg_tx, mut msg_rx) = mpsc::channel::<Msg>(minicore_tui::app::MESSAGE_CAPACITY);

    terminal::install_panic_hook();
    let mut guard = match terminal::Guard::enter() {
        Ok(guard) => guard,
        Err(error) => {
            eprintln!("terminal setup failed: {error}");
            if let Err(error) = finish_host(&mut hosted).await {
                eprintln!("minicore-tui: {error}");
            }
            return 1;
        }
    };

    let mut app = App::new(client.clone(), msg_tx.clone(), workspace.clone());
    app.launch = minicore_tui::app::LaunchSettings {
        model,
        reasoning: args.reasoning,
    };
    app.selected_agent = selected_agent.clone();
    app.theme = theme;
    app.notifier.bell = launch.ui.bell;
    app.editor = launch.ui.editor;
    app.composer = minicore_tui::app::composer::Composer::with_history(
        minicore_tui::prompt_history::load(&directory.history_file()),
    );
    app.config_directory = Some(directory);
    app.styles = Styles::of(Palette::for_kind(theme));

    let boot = {
        let client = client.clone();
        let tx = msg_tx.clone();
        let selected = selected_agent.clone();
        tokio::spawn(async move { minicore_tui::app::bootstrap(client, tx, selected).await })
    };
    let (mut terminal_events, mut term_rx) = start_terminal_events();
    let mut frames = Frames::new();
    let mut next_frame = tokio::time::Instant::now();
    let mut next_tick = tokio::time::Instant::now() + TICK;
    let mut startup = true;
    let mut failed = None;

    loop {
        let wake = if app.animating() {
            Some(next_tick)
        } else {
            app.next_expiry()
        };
        tokio::select! {
            signal = signals.recv() => {
                if let Err(error) = signal {
                    failed = Some(format!("shutdown signal failed: {error}"));
                }
                app.quit = true;
            }
            message = msg_rx.recv() => {
                match message {
                    Some(message) => {
                        app.on_msg(message);
                        frames.mark(Cause::Event);
                    }
                    None => {
                        failed = Some("application event channel closed".to_owned());
                        break;
                    }
                }
            }
            maybe_event = term_rx.recv() => {
                match maybe_event {
                    Some(TermEvent::Key(key)) => {
                        if !key.is_release() {
                            if let Some(action) = app.on_key(key) {
                                app.on_action(action);
                            }
                            frames.mark(Cause::Key);
                        }
                    }
                    Some(TermEvent::Mouse(mouse)) => {
                        if app.on_mouse(mouse) {
                            frames.mark(Cause::Mouse);
                        }
                    }
                    Some(TermEvent::Paste(text)) => {
                        app.on_paste(&text);
                        frames.mark(Cause::Key);
                    }
                    Some(TermEvent::Resize(_, _)) => frames.mark(Cause::Resize),
                    // The stream ended: the terminal is gone.
                    None => app.quit = true,
                    _ => {}
                }
            }
            _ = tokio::time::sleep_until(wake.unwrap_or(next_tick)), if wake.is_some() => {
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
                    // Await cancellation so EventStream is dropped before the child
                    // owns stdin. Merely ignoring forwarded events keeps reading it.
                    terminal_events.abort();
                    let _ = (&mut terminal_events).await;
                    let result =
                        minicore_tui::editor::edit_in_terminal(&mut guard, &command, &draft).await;
                    (terminal_events, term_rx) = start_terminal_events();
                    result
                }
                Err(error) => Err(error),
            };
            let edited = match result {
                Ok(text) => text,
                Err(error) => {
                    app.notice(minicore_tui::app::NoticeLevel::Error, error);
                    // A failed editor must not cost the user what they typed.
                    draft
                }
            };
            app.on_msg(Msg::DraftEdited(edited));
            frames.mark(Cause::Event);
        }

        if app.ready && startup {
            startup = false;
            match startup_session(
                &app.sessions,
                &app.agents,
                app.selected_agent.as_ref(),
                selected_agent.as_ref(),
                selected_session.as_ref(),
                args.continue_recent,
                &workspace,
            ) {
                Ok(Some(target)) => {
                    app.open_session(target);
                    frames.mark(Cause::Event);
                }
                Ok(None) => {}
                Err(error) => app.notice(minicore_tui::app::NoticeLevel::Error, error),
            }
        }

        if frames.wanted() && tokio::time::Instant::now() >= next_frame {
            let started = Instant::now();
            if guard.is_active()
                && let Err(error) = guard.draw(|frame| minicore_tui::ui::draw(frame, &mut app))
            {
                failed = Some(format!("draw failed: {error}"));
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

    terminal_events.abort();
    boot.abort();
    let _ = terminal_events.await;
    let _ = boot.await;
    if let Err(error) = guard.restore() {
        failed = Some(format!("terminal restoration failed: {error}"));
    }
    msg_rx.close();
    app.local_files.finish().await;
    frames.print(&app);
    print!("{}", app.exit_output());
    drop(app);
    drop(msg_rx);
    drop(msg_tx);
    if let Err(error) = finish_host(&mut hosted).await {
        failed = Some(error);
    }
    if let Some(error) = failed {
        eprintln!("minicore-tui: {error}");
        return 1;
    }
    0
}

fn start_terminal_events() -> (
    tokio::task::JoinHandle<()>,
    mpsc::UnboundedReceiver<TermEvent>,
) {
    // Keep stream polling out of the frame loop: it contends with the reader
    // thread's event lock even when no input is ready.
    let (term_tx, term_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let mut events = EventStream::new();
        #[cfg(windows)]
        terminal::input::forward(&mut events, &term_tx).await;
        #[cfg(not(windows))]
        while let Some(event) = events.next().await {
            if let Ok(event) = event
                && term_tx.send(event).is_err()
            {
                break;
            }
        }
    });
    (task, term_rx)
}

struct ExitSignals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
}

impl ExitSignals {
    fn new() -> std::io::Result<Self> {
        Ok(Self {
            #[cfg(unix)]
            interrupt: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?,
            #[cfg(unix)]
            terminate: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
        })
    }

    async fn recv(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        tokio::select! {
            _ = self.interrupt.recv() => Ok(()),
            _ = self.terminate.recv() => Ok(()),
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await
    }
}

async fn finish_host(hosted: &mut Hosted) -> Result<(), String> {
    eprintln!("Waiting for accepted Host work and saves; Ctrl+C requests force shutdown.");
    hosted.request_shutdown(HostShutdownStrategy::Graceful)?;
    let control = hosted.process_control();
    let mut current = hosted.snapshot()?;
    while current.lifecycle != HostLifecycle::Stopped {
        if let Some(fault) = &current.shutdown_fault {
            eprintln!(
                "Host shutdown is blocked by {} ({}); cleanup is incomplete and the Host remains open.",
                fault.type_name(),
                fault.code()
            );
        }
        tokio::select! {
            changed = control.changed(&current) => {
                current = changed.map_err(|fault| fault.message().to_owned())?;
            }
            signal = tokio::signal::ctrl_c() => {
                signal.map_err(|_| "cannot listen for an explicit shutdown signal".to_owned())?;
                eprintln!("Force shutdown requested; waiting for Host cancellation, saves and cleanup.");
                control.shutdown(HostShutdownRequest {
                    operation_id: OperationId::new().map_err(|error| error.to_string())?,
                    strategy: HostShutdownStrategy::Force,
                }).map_err(|fault| fault.message().to_owned())?;
                current = hosted.snapshot()?;
            }
        }
    }
    hosted.finish_shutdown().await
}

fn startup_session(
    sessions: &[SessionSummary],
    agents: &[AgentSummary],
    selected_agent: Option<&AgentId>,
    explicit_agent: Option<&AgentId>,
    requested: Option<&SessionId>,
    continue_recent: bool,
    workspace: &Path,
) -> Result<Option<SessionRef>, String> {
    let selected = if let Some(requested) = requested {
        Some(
            sessions
                .iter()
                .find(|session| {
                    &session.id == requested
                        && explicit_agent.is_none_or(|agent| &session.agent_id == agent)
                })
                .ok_or_else(|| {
                    format!("session {requested} is not available under the selected Host/Agent")
                })?,
        )
    } else if continue_recent {
        pick_recent(sessions, selected_agent, workspace)
    } else if let Some(agent) =
        selected_agent.and_then(|id| agents.iter().find(|agent| &agent.id == id))
    {
        match &agent.default_session_id {
            Some(id) => Some(
                sessions
                    .iter()
                    .find(|session| &session.id == id && session.agent_id == agent.id)
                    .ok_or_else(|| {
                        format!("Agent {} default Session {id} is unavailable", agent.id)
                    })?,
            ),
            None => None,
        }
    } else {
        None
    };
    Ok(selected.map(|session| SessionRef {
        agent_id: session.agent_id.clone(),
        session_id: session.id.clone(),
    }))
}

fn pick_recent<'a>(
    sessions: &'a [SessionSummary],
    agent: Option<&AgentId>,
    workspace: &Path,
) -> Option<&'a SessionSummary> {
    sessions
        .iter()
        .filter(|session| agent.is_none_or(|id| &session.agent_id == id))
        .filter(|session| {
            session
                .workspace
                .as_ref()
                .is_some_and(|bound| Path::new(&bound.path) == workspace)
        })
        .max_by(|left, right| left.last_activity_at.cmp(&right.last_activity_at))
}

#[cfg(test)]
#[path = "main/tests.rs"]
mod tests;
