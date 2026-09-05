//! The fullscreen event loop (development spec 6-8). Real wiring:
//! parse/validate args, spawn `minicore-agent --config <path> --stdio` before
//! the terminal is entered, then run a multi-source `tokio::select!` loop
//! (RPC frames, terminal events, tick, OS signal, shutdown deadline, render
//! throttle) that feeds every source through the single `App::update`. The
//! terminal and RPC are runtime state; they never mutate the app directly.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

#[cfg(not(unix))]
use std::future::Future;
#[cfg(not(unix))]
use std::pin::Pin;

use crossterm::event::{Event, EventStream};
use futures_util::StreamExt;

use minicore_tui::app::{App, CliPrefs};
use minicore_tui::args::{self, Args};
use minicore_tui::command::AppCommand;
use minicore_tui::event::{AppEvent, RpcEvent};
use minicore_tui::protocol::OutgoingRequest;
use minicore_tui::rpc::{RpcError, RpcProcess};
use minicore_tui::terminal::{PanicHookGuard, TerminalGuard};
use minicore_tui::ui;

/// Maximum draw rate (spec 7): 30 FPS.
const RENDER_INTERVAL: Duration = Duration::from_millis(33);
/// Cut-off timer when no tick source is armed; the select stays idle on the
/// RPC/terminal arms instead of busy-looping.
const IDLE_POLL: Duration = Duration::from_secs(3600);
/// Maximum number of already-buffered RPC events handled in one select turn.
const RPC_BATCH_LIMIT: usize = 64;
/// Maximum time spent applying one buffered RPC batch before returning to the
/// scheduler. This keeps terminal, signal, render, and shutdown arms live.
const RPC_BATCH_BUDGET: Duration = Duration::from_millis(4);
/// Small explicit cooldown after a batch; it gives non-RPC arms a full select
/// opportunity even when the RPC channel is continuously ready.
const RPC_BATCH_COOLDOWN: Duration = Duration::from_millis(1);

#[tokio::main]
async fn main() -> ExitCode {
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

    // Spawn the agent before the alternate screen: config validation and
    // spawn failures are ordinary stderr errors (spec 1, 2).
    let mut process = match RpcProcess::spawn(&opts.agent_bin, &opts.agent_config) {
        Ok(process) => process,
        Err(error) => {
            eprintln!("minicore-tui: {error}");
            eprintln!("Try 'minicore-tui --help' for usage.");
            return ExitCode::from(2);
        }
    };

    let _panic_hook = PanicHookGuard::install();
    let mut guard = match TerminalGuard::enter() {
        Ok(guard) => guard,
        Err(error) => {
            // Never leave the agent child behind when the terminal failed.
            process.terminate().await;
            eprintln!("minicore-tui: failed to start terminal: {error}");
            return ExitCode::FAILURE;
        }
    };

    let run_result = run_fullscreen(&mut guard, &mut process, &opts).await;
    // Reap the child on every path (idempotent after a clean shutdown).
    process.terminate().await;
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

/// One iteration of the async loop picks one source at a time.
#[allow(clippy::large_enum_variant)]
enum Selected {
    Rpc(Option<RpcEvent>),
    RpcCooldown,
    Terminal(Event),
    TerminalEof,
    Signal,
    Tick,
    ShutdownTimeout,
    Render,
}

/// Drives the app until `AppCommand::Exit` (the agent is gone) or a fatal
/// shutdown timeout. App state is only ever changed through `App::update`;
/// this function owns the terminal, the RPC process and the timers.
async fn run_fullscreen(
    guard: &mut TerminalGuard,
    process: &mut RpcProcess,
    opts: &Args,
) -> io::Result<()> {
    let workspace = if opts.workspace_explicit {
        opts.workspace.clone()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };
    let prefs = CliPrefs {
        profile: opts.profile.clone(),
        model: opts.model.clone(),
        reasoning: opts.reasoning,
        open_new_session_on_ready: opts.workspace_explicit,
    };
    let mut app = App::with_cli_prefs(workspace, prefs);
    app.update(AppEvent::SetTheme(opts.theme));

    let terminal = guard.terminal_mut();
    // The application does not create a blocking input-reader thread.
    // Crossterm's EventStream owns its reader and uses its Drop behavior to
    // wake/stop it; the exact internal implementation remains Crossterm's
    // responsibility (spec 3, 10).
    let mut events = EventStream::new();
    // One OS-signal listener for the whole run, reused by every iteration.
    let mut signals = SignalListener::new()?;
    let mut signal_fired = false;

    // Bootstrap fires the four discovery requests concurrently (spec 6).
    let commands = app.update(AppEvent::Bootstrap);
    if run_commands(process, &mut app, commands, opts.debug).await? {
        return Ok(());
    }

    let mut last_size = (0, 0);
    let mut last_total = 0usize;
    let mut last_visible = 0usize;
    let mut last_dock_rows = 0usize;
    let mut last_render = Instant::now();
    let mut rpc_open = true;
    let mut rpc_cooldown_until: Option<Instant> = None;

    loop {
        // Measured geometry flows back through `AppEvent::Viewport`; the
        // renderer never writes scroll state (spec 3, 7).
        let size = terminal.size()?;
        prepare_transcript_cache(&mut app, size.width);
        let width = size.width as usize;
        let total = ui::transcript::total_lines(&app, size.width);
        let dock_rows = ui::layout::dock_rows(&app, size.width, size.height) as usize;
        let transcript_height = size.height.saturating_sub(dock_rows as u16);
        let visible = ui::transcript::visible_rows(&app, total, transcript_height);
        if (width, size.height as usize) != last_size
            || total != last_total
            || visible != last_visible
            || dock_rows != last_dock_rows
        {
            last_size = (width, size.height as usize);
            last_total = total;
            last_visible = visible;
            last_dock_rows = dock_rows;
            app.update(AppEvent::Viewport {
                total_lines: total,
                visible_rows: visible,
            });
        }
        if shutdown_timeout_command(&app).is_some() {
            return Err(force_kill_and_report(process, &mut app).await);
        }
        let shutdown_deadline = app.shutdown_remaining();
        let tick_deadline = app.next_tick().unwrap_or(IDLE_POLL);
        let render_deadline = render_deadline(app.dirty, last_render.elapsed());
        let rpc_cooldown =
            rpc_cooldown_until.map(|deadline| deadline.saturating_duration_since(Instant::now()));

        let selected = tokio::select! {
            () = sleep_or_pending(shutdown_deadline) => Selected::ShutdownTimeout,
            maybe = process.recv(), if rpc_open && rpc_cooldown_until.is_none() => Selected::Rpc(maybe),
            () = sleep_or_pending(rpc_cooldown), if rpc_cooldown_until.is_some() => Selected::RpcCooldown,
            maybe = events.next() => match maybe {
                Some(Ok(event)) => Selected::Terminal(event),
                Some(Err(error)) => {
                    return Err(io::Error::new(
                        error.kind(),
                        format!("terminal input failed: {error}"),
                    ))
                }
                None => Selected::TerminalEof,
            },
            () = shutdown_signal(&mut signals), if !signal_fired => Selected::Signal,
            () = tokio::time::sleep(tick_deadline) => Selected::Tick,
            () = sleep_or_pending(render_deadline) => Selected::Render,
        };

        let mut exit = false;
        match selected {
            Selected::ShutdownTimeout => {
                // 5s without a clean exit: force-kill; main's terminate()
                // reaps the child before the terminal is restored.
                if shutdown_timeout_command(&app).is_some() {
                    return Err(force_kill_and_report(process, &mut app).await);
                }
            }
            Selected::Rpc(Some(event)) => {
                let batch = run_rpc_batch(process, &mut app, event, opts.debug).await?;
                if batch.exit {
                    exit = true;
                } else if batch.channel_ended {
                    let commands = rpc_channel_ended(&mut rpc_open, &mut app);
                    if run_commands(process, &mut app, commands, opts.debug).await? {
                        exit = true;
                    }
                } else {
                    rpc_cooldown_until = Some(Instant::now() + RPC_BATCH_COOLDOWN);
                }
            }
            Selected::Rpc(None) => {
                let commands = rpc_channel_ended(&mut rpc_open, &mut app);
                if run_commands(process, &mut app, commands, opts.debug).await? {
                    exit = true;
                }
            }
            Selected::RpcCooldown => {
                rpc_cooldown_until = None;
                tokio::task::yield_now().await;
            }
            Selected::Terminal(event) => {
                let commands = app.update(AppEvent::Terminal(event));
                if run_commands(process, &mut app, commands, opts.debug).await? {
                    exit = true;
                }
            }
            Selected::TerminalEof => {
                return Err(io::Error::other("terminal input stream ended"));
            }
            Selected::Signal => {
                signal_fired = true;
                let commands = app.update(AppEvent::ShutdownRequested);
                if run_commands(process, &mut app, commands, opts.debug).await? {
                    exit = true;
                }
            }
            Selected::Tick => {
                app.update(AppEvent::Tick);
            }
            Selected::Render => {
                prepare_transcript_cache(&mut app, terminal.size()?.width);
                terminal.draw(|frame| ui::render(frame, &app))?;
                last_render = Instant::now();
                app.update(AppEvent::Rendered);
            }
        }
        if exit {
            return Ok(());
        }

        // Render when state changed and the 30 FPS budget allows it; the
        // Rendered event clears the dirty flag so idle frames never draw.
        if app.dirty && last_render.elapsed() >= RENDER_INTERVAL {
            prepare_transcript_cache(&mut app, terminal.size()?.width);
            terminal.draw(|frame| ui::render(frame, &app))?;
            last_render = Instant::now();
            app.update(AppEvent::Rendered);
        }
    }
}

fn prepare_transcript_cache(app: &mut App, width: u16) {
    if let Some(prepared) = ui::transcript::prepare_cache(app, width) {
        app.update(AppEvent::TranscriptCachePrepared(prepared));
    }
}

/// A timer arm is authoritative only when the App-owned deadline has really
/// elapsed; the timer itself is never recreated from the current loop time.
fn shutdown_timeout_command(app: &App) -> Option<AppCommand> {
    app.shutting_down()
        .then(|| app.shutdown_remaining())
        .flatten()
        .filter(Duration::is_zero)
        .map(|_| AppCommand::KillChild)
}

async fn force_kill_and_report(process: &mut RpcProcess, app: &mut App) -> io::Error {
    process
        .terminate_with_observer(|event| {
            // Forced shutdown is reporting-only. App::update remains the
            // sole state mutation entry point, but no commands from late
            // events may be dispatched after the child was killed.
            let _ = app.update(AppEvent::Rpc(event));
        })
        .await;
    io::Error::other(app.shutdown_force_message())
}

struct RpcBatchResult {
    exit: bool,
    channel_ended: bool,
}

/// Apply at most one bounded RPC batch. Any event after the limit remains in
/// `RpcProcess`'s bounded channel for the next select turn, so no frame is
/// dropped merely to preserve fairness.
async fn run_rpc_batch(
    process: &mut RpcProcess,
    app: &mut App,
    first: RpcEvent,
    debug: bool,
) -> io::Result<RpcBatchResult> {
    let started = Instant::now();
    let mut processed = 0usize;
    let mut pending = Some(first);
    let mut channel_ended = false;
    while let Some(event) = pending {
        processed += 1;
        let commands = app.update(AppEvent::Rpc(event));
        if run_commands(process, app, commands, debug).await? {
            return Ok(RpcBatchResult {
                exit: true,
                channel_ended: false,
            });
        }
        if rpc_batch_should_yield(processed, started.elapsed()) {
            break;
        }
        match process.try_recv() {
            Ok(next) => pending = next,
            Err(RpcError::Closed) => {
                channel_ended = true;
                break;
            }
            Err(error) => {
                return Err(io::Error::other(format!("RPC polling failed: {error}")));
            }
        }
    }
    tokio::task::yield_now().await;
    Ok(RpcBatchResult {
        exit: false,
        channel_ended,
    })
}

fn rpc_batch_should_yield(processed: usize, elapsed: Duration) -> bool {
    processed >= RPC_BATCH_LIMIT || elapsed >= RPC_BATCH_BUDGET
}

fn render_deadline(dirty: bool, elapsed: Duration) -> Option<Duration> {
    dirty.then(|| RENDER_INTERVAL.saturating_sub(elapsed))
}

/// Marks the channel closed exactly once and routes the lifecycle consequence
/// through `App::update`. The caller must not poll `RpcProcess::recv` again.
fn rpc_channel_ended(rpc_open: &mut bool, app: &mut App) -> Vec<AppCommand> {
    *rpc_open = false;
    app.update(AppEvent::RpcChannelEnded)
}

/// OS shutdown signals, registered exactly once for the whole run and
/// reused by every `select!` iteration (never re-registered per frame).
#[cfg(unix)]
struct SignalListener {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl SignalListener {
    fn new() -> io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }
}

#[cfg(unix)]
async fn shutdown_signal(signals: &mut SignalListener) {
    tokio::select! {
        _ = signals.interrupt.recv() => {}
        _ = signals.terminate.recv() => {}
    }
}

/// On Windows the console Ctrl+C event is handled by the single crossterm
/// key path in raw mode; tokio's `ctrl_c` covers OS-level CTRL_C_EVENT.
#[cfg(not(unix))]
struct SignalListener {
    _ctrl_c: Pin<Box<dyn Future<Output = io::Result<()>> + Send>>,
}

#[cfg(not(unix))]
impl SignalListener {
    fn new() -> io::Result<Self> {
        Ok(Self {
            _ctrl_c: Box::pin(tokio::signal::ctrl_c()),
        })
    }
}

#[cfg(not(unix))]
async fn shutdown_signal(signals: &mut SignalListener) {
    let _ = (&mut signals._ctrl_c).await;
}

async fn sleep_or_pending(duration: Option<Duration>) {
    match duration {
        Some(duration) => tokio::time::sleep(duration).await,
        None => std::future::pending().await,
    }
}

/// Executes side effects without ever mutating `App` directly; failures (and
/// their follow-up commands) flow back as `AppEvent`s. Returns `true` when
/// the loop must exit (`AppCommand::Exit`).
async fn run_commands(
    process: &mut RpcProcess,
    app: &mut App,
    commands: Vec<AppCommand>,
    debug: bool,
) -> io::Result<bool> {
    let mut queue: VecDeque<AppCommand> = commands.into();
    while let Some(command) = queue.pop_front() {
        match command {
            AppCommand::Rpc(request) => {
                let start = Instant::now();
                let result = process.send(request.clone()).await;
                if debug {
                    debug_log_request(&request, start);
                }
                if let Err(error) = result {
                    // A send failure is an event: on_send_failed recovers
                    // state and may chain KillChild/Exit here.
                    let more = app.update(AppEvent::RpcSendFailed {
                        id: request.id,
                        error,
                    });
                    queue.extend(more);
                }
            }
            AppCommand::KillChild => process.kill_child(),
            AppCommand::Exit => return Ok(true),
        }
    }
    Ok(false)
}

/// `--debug` records only method, id, serialized byte count and duration to
/// a temp file (never message content, error text, reasoning, tool args, or
/// raw frames, spec 13).
fn debug_log_request(request: &OutgoingRequest, start: Instant) {
    use std::io::Write;
    let bytes = serde_json::to_string(request)
        .map(|line| line.len() + 1)
        .unwrap_or(0);
    let line = format!(
        "id={} method={} bytes={} ms={:.1}\n",
        request.id.0,
        request.method,
        bytes,
        start.elapsed().as_secs_f64() * 1000.0
    );
    let path = std::env::temp_dir().join("minicore-tui-debug.log");
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(line.as_bytes()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    /// The OS-signal listener is registered once at construction and is safe
    /// to drive from every `select!` iteration (a fresh borrowing future per
    /// poll). No signal fires in a test, so `recv` must simply remain
    /// pending — proving there is no per-iteration re-registration.
    #[tokio::test]
    async fn signal_listener_installs_once_and_recv_stays_pending() {
        let mut listener = SignalListener::new().expect("signal listeners install");
        let first =
            tokio::time::timeout(Duration::from_millis(20), shutdown_signal(&mut listener)).await;
        assert!(first.is_err(), "no OS signal should fire inside the test");
        let second =
            tokio::time::timeout(Duration::from_millis(20), shutdown_signal(&mut listener)).await;
        assert!(second.is_err(), "the same listener stays reusable");
    }

    #[test]
    fn idle_has_no_render_deadline_and_busy_clamps_at_30_fps() {
        assert_eq!(render_deadline(false, Duration::ZERO), None);
        assert_eq!(render_deadline(true, Duration::ZERO), Some(RENDER_INTERVAL));
        assert_eq!(render_deadline(true, RENDER_INTERVAL), Some(Duration::ZERO));
        assert_eq!(
            render_deadline(true, RENDER_INTERVAL + Duration::from_secs(1)),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn rpc_batch_is_bounded_and_yields_before_a_render_interval() {
        assert!(rpc_batch_should_yield(RPC_BATCH_LIMIT, Duration::ZERO));
        assert!(!rpc_batch_should_yield(
            RPC_BATCH_LIMIT - 1,
            RPC_BATCH_BUDGET.saturating_sub(Duration::from_nanos(1))
        ));
        assert!(rpc_batch_should_yield(1, RPC_BATCH_BUDGET));
        assert!(RPC_BATCH_BUDGET + RPC_BATCH_COOLDOWN < RENDER_INTERVAL);

        let mut high_frequency = (0..(RPC_BATCH_LIMIT * 2)).collect::<VecDeque<_>>();
        let mut first_batch = Vec::new();
        while let Some(event) = high_frequency.pop_front() {
            first_batch.push(event);
            if rpc_batch_should_yield(first_batch.len(), Duration::ZERO) {
                break;
            }
        }
        assert_eq!(first_batch.len(), RPC_BATCH_LIMIT);
        assert_eq!(high_frequency.len(), RPC_BATCH_LIMIT);
    }

    #[test]
    fn rpc_channel_end_disables_polling_but_keeps_failed_overlay_interactive() {
        let mut app = App::new(PathBuf::from("/workspace"));
        app.update(AppEvent::Rpc(RpcEvent::ProtocolError(
            minicore_tui::protocol::FrameError::new(
                minicore_tui::protocol::FrameErrorKind::Io,
                "pipe broke",
            ),
        )));
        app.update(AppEvent::Rendered);
        let mut rpc_open = true;
        assert!(rpc_channel_ended(&mut rpc_open, &mut app).is_empty());
        assert!(!rpc_open, "the closed channel must not be polled again");
        assert!(matches!(
            app.connection,
            minicore_tui::app::ConnectionState::Failed(_)
        ));
        assert!(
            app.dirty,
            "channel closure still schedules the fatal redraw"
        );

        let commands = app.update(AppEvent::Terminal(Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('q'),
                crossterm::event::KeyModifiers::empty(),
            ),
        )));
        assert!(matches!(commands.as_slice(), [AppCommand::Exit]));
    }

    #[test]
    fn rpc_channel_end_exits_a_normal_shutdown() {
        let mut app = App::new(PathBuf::from("/workspace"));
        let commands = app.update(AppEvent::ShutdownRequested);
        assert!(matches!(
            commands.as_slice(),
            [AppCommand::Rpc(request)] if request.method == "agent.shutdown"
        ));
        let mut rpc_open = true;
        let commands = rpc_channel_ended(&mut rpc_open, &mut app);
        assert!(!rpc_open);
        assert!(matches!(commands.as_slice(), [AppCommand::Exit]));
    }

    #[test]
    fn shutdown_timeout_command_uses_the_app_latched_deadline() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let base = Instant::now();
        let elapsed = std::sync::Arc::new(AtomicU64::new(0));
        let clock_elapsed = std::sync::Arc::clone(&elapsed);
        let mut app = App::with_monotonic_clock(PathBuf::from("/workspace"), move || {
            base.checked_add(Duration::from_millis(clock_elapsed.load(Ordering::Relaxed)))
                .expect("test clock remains representable")
        });
        assert!(shutdown_timeout_command(&app).is_none());
        let first = app.update(AppEvent::ShutdownRequested);
        assert!(matches!(first.as_slice(), [AppCommand::Rpc(_)]));
        assert_eq!(app.shutdown_remaining(), Some(Duration::from_secs(5)));
        for millis in 0..5000 {
            elapsed.store(millis, Ordering::Relaxed);
            app.update(AppEvent::Tick);
            app.update(AppEvent::Rpc(RpcEvent::AgentLogLine("busy".to_owned())));
            app.update(AppEvent::ShutdownRequested);
        }
        assert!(
            app.shutdown_remaining()
                .is_some_and(|remaining| !remaining.is_zero())
        );
        elapsed.store(5000, Ordering::Relaxed);
        assert_eq!(app.shutdown_remaining(), Some(Duration::ZERO));
        assert!(matches!(
            shutdown_timeout_command(&app),
            Some(AppCommand::KillChild)
        ));
        assert_eq!(
            app.shutdown_force_message(),
            "shutdown timed out; Agent force-terminated; last Agent stderr: busy"
        );
    }

    #[test]
    fn forced_shutdown_timeout_report_keeps_unknown_and_known_failure_facts() {
        use minicore_tui::protocol::{Reasoning, SessionInfo};
        use minicore_tui::state::session::SessionView;
        use minicore_tui::state::turn::{LiveLoop, LocalSubmissionId};
        use serde_json::json;
        use std::sync::atomic::{AtomicU64, Ordering};

        let base = Instant::now();
        let elapsed = std::sync::Arc::new(AtomicU64::new(0));
        let clock_elapsed = std::sync::Arc::clone(&elapsed);
        let mut app = App::with_monotonic_clock(PathBuf::from("/workspace"), move || {
            base.checked_add(Duration::from_millis(clock_elapsed.load(Ordering::Relaxed)))
                .expect("test clock remains representable")
        });
        let info = |session_id: &str| SessionInfo {
            session_id: session_id.to_owned(),
            title: None,
            profile: "coding".to_owned(),
            workspace: "/workspace".to_owned(),
            model: "deep".to_owned(),
            reasoning: Reasoning::High,
            loaded: true,
            created_at: "2026-01-02T03:04:05Z".to_owned(),
            updated_at: "2026-01-02T03:04:05Z".to_owned(),
        };

        let mut unknown = SessionView::new(info("unknown"));
        let mut unknown_live = LiveLoop::new(LocalSubmissionId(1), "unknown turn".to_owned());
        unknown_live.reference = Some(minicore_tui::protocol::TurnRef {
            session_id: "unknown".to_owned(),
            loop_id: "loop_unknown".to_owned(),
        });
        unknown.live = Some(unknown_live);
        unknown.result_unconfirmed = true;

        let mut known_failed = SessionView::new(info("known-failed"));
        known_failed.last_result = Some(
            serde_json::from_value(json!({
                "turn": {"session_id": "known-failed", "loop_id": "loop_failed"},
                "outcome": {"type": "completed"},
                "usage": {},
                "requests": 1,
                "tool_rounds": 0,
                "final_config_revision": 0,
                "persistence": "failed"
            }))
            .expect("known failed result fixture parses"),
        );

        app.sessions.known.insert("unknown".to_owned(), unknown);
        app.sessions
            .known
            .insert("known-failed".to_owned(), known_failed);
        app.update(AppEvent::Rpc(RpcEvent::AgentLogLine(
            "stderr before forced kill".to_owned(),
        )));
        app.update(AppEvent::ShutdownRequested);
        elapsed.store(5000, Ordering::Relaxed);

        assert!(matches!(
            shutdown_timeout_command(&app),
            Some(AppCommand::KillChild)
        ));
        let message = app.shutdown_force_message();
        assert!(message.contains("force-terminated"));
        assert!(message.contains("result/save status unconfirmed"));
        assert!(message.contains("known persistence failure retained"));
        assert!(message.contains("stderr before forced kill"));
    }

    #[tokio::test]
    async fn forced_shutdown_drains_gated_stderr_before_reporting() {
        use minicore_tui::app::ConnectionState;
        use minicore_tui::protocol::{Reasoning, SessionInfo, TurnRef};
        use minicore_tui::state::session::SessionView;
        use minicore_tui::state::turn::{LiveLoop, LocalSubmissionId};

        let executable = std::env::current_exe()
            .expect("main test executable")
            .canonicalize()
            .unwrap_or_else(|_| std::env::current_exe().expect("main test executable"));
        let binary = std::env::var_os("CARGO_BIN_EXE_agent_process")
            .map(PathBuf::from)
            .filter(|path| is_agent_process_executable(path))
            .or_else(|| {
                std::fs::read_dir(executable.parent().expect("test executable directory"))
                    .ok()?
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| {
                                name == "agent_process" || name.starts_with("agent_process-")
                            })
                    })
                    .filter(|path| is_agent_process_executable(path))
                    .max_by_key(|path| {
                        std::fs::metadata(path)
                            .and_then(|metadata| metadata.modified())
                            .ok()
                    })
            })
            .expect("agent_process test target must be built");
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        );
        let config = std::env::temp_dir().join(format!("mct-main-{suffix}.toml"));
        let ready = std::env::temp_dir().join(format!("mct-main-{suffix}.ready"));
        std::fs::write(&config, "hang_stderr_gate").expect("write fake agent config");
        let ready_text = ready.to_str().expect("ready path is UTF-8");
        let mut process =
            RpcProcess::spawn_with_env(&binary, &config, &[("FAKE_AGENT_READY_FILE", ready_text)])
                .expect("spawn gated fake agent");

        let info = |session_id: &str| SessionInfo {
            session_id: session_id.to_owned(),
            title: None,
            profile: "coding".to_owned(),
            workspace: "/workspace".to_owned(),
            model: "deep".to_owned(),
            reasoning: Reasoning::High,
            loaded: true,
            created_at: "2026-01-02T03:04:05Z".to_owned(),
            updated_at: "2026-01-02T03:04:05Z".to_owned(),
        };
        let mut app = App::new(PathBuf::from("/workspace"));
        let mut unknown = SessionView::new(info("unknown"));
        let mut unknown_live = LiveLoop::new(LocalSubmissionId(1), "unknown turn".to_owned());
        unknown_live.reference = Some(TurnRef {
            session_id: "unknown".to_owned(),
            loop_id: "loop_unknown".to_owned(),
        });
        unknown.live = Some(unknown_live);
        unknown.result_unconfirmed = true;
        let mut known_failed = SessionView::new(info("known-failed"));
        let mut known_live = LiveLoop::new(LocalSubmissionId(2), "known turn".to_owned());
        known_live.reference = Some(TurnRef {
            session_id: "known-failed".to_owned(),
            loop_id: "loop_failed".to_owned(),
        });
        known_failed.live = Some(known_live);
        app.sessions.known.insert("unknown".to_owned(), unknown);
        app.sessions
            .known
            .insert("known-failed".to_owned(), known_failed);
        app.sessions.active = Some("known-failed".to_owned());
        app.connection = ConnectionState::Ready;

        let wait_request = match app
            .update(AppEvent::RefreshTurn {
                session_id: "known-failed".to_owned(),
            })
            .as_slice()
        {
            [AppCommand::Rpc(request)] => request.clone(),
            other => panic!("expected turn.wait request, got {other:?}"),
        };
        process.send(wait_request).await.expect("send wait request");

        let commands = app.update(AppEvent::ShutdownRequested);
        let request = match commands.as_slice() {
            [AppCommand::Rpc(request)] => request.clone(),
            other => panic!("expected shutdown request, got {other:?}"),
        };
        process.send(request).await.expect("send shutdown request");

        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() {
            assert!(
                Instant::now() < deadline,
                "fake agent did not reach stderr gate"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        // No process event was consumed before the force path. The ready file
        // is written only after the fake agent flushed stderr, so this covers
        // the reader/child-waiter ordering race directly.
        let error = force_kill_and_report(&mut process, &mut app).await;
        let report = error.to_string();
        assert!(report.contains("fake agent stderr after forced termination"));
        assert!(report.contains("result/save status unconfirmed"));
        assert!(report.contains("known persistence failure retained"));
        assert_eq!(
            app.sessions.known["known-failed"]
                .last_result
                .as_ref()
                .map(|result| result.persistence),
            Some(minicore_tui::protocol::TurnPersistenceWire::Failed)
        );
        assert!(
            process.child_reaped(),
            "forced shutdown must reap the child"
        );

        let _ = std::fs::remove_file(config);
        let _ = std::fs::remove_file(ready);
    }

    fn is_agent_process_executable(path: &std::path::Path) -> bool {
        let Ok(metadata) = std::fs::metadata(path) else {
            return false;
        };
        if !metadata.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o111 != 0
        }
        #[cfg(windows)]
        {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        }
        #[cfg(not(any(unix, windows)))]
        {
            true
        }
    }
}
