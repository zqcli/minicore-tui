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
use ratatui::layout::Rect;

use minicore_tui::app::{App, CliPrefs, StartupSession};
use minicore_tui::args::{self, Args};
use minicore_tui::command::AppCommand;
use minicore_tui::event::{AppEvent, JobOutcome, RpcEvent};
use minicore_tui::jobs::{CopyAdmission, LocalJobs};
use minicore_tui::protocol::{
    METHOD_SESSION_CLOSE, METHOD_SESSION_COMPACT_CANCEL, METHOD_SHUTDOWN, METHOD_TURN_CANCEL,
    OutgoingRequest,
};
use minicore_tui::rpc::{RpcError, RpcProcess, SendClass, SendError};
use minicore_tui::terminal::{PanicHookGuard, TerminalGuard};
use minicore_tui::ui;

/// Maximum draw rate (spec 7): 30 FPS.
const RENDER_INTERVAL: Duration = Duration::from_millis(33);
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
    let mut opts = match args::parse(std::env::args().skip(1)) {
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

    let config_path = opts
        .config_path
        .clone()
        .unwrap_or_else(minicore_tui::config::default_path);
    let mut tui_config = match minicore_tui::config::load(&config_path, opts.config_path.is_some())
    {
        Ok(config) => config,
        Err(error) => {
            eprintln!("minicore-tui: configuration error: {error}");
            return ExitCode::from(2);
        }
    };
    if tui_config.editor.is_none() {
        match minicore_tui::config::editor_from_environment() {
            Ok(editor) => tui_config.editor = editor,
            Err(error) => {
                eprintln!("minicore-tui: configuration error: {error}");
                return ExitCode::from(2);
            }
        }
    }
    if let Err(error) = opts.apply_tui_config(&tui_config) {
        eprintln!("minicore-tui: configuration error: {error}");
        return ExitCode::from(2);
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

    let mut jobs = LocalJobs::new();
    let run_result = run_fullscreen(
        &mut guard,
        &mut process,
        &mut jobs,
        &opts,
        config_path,
        tui_config,
    )
    .await;
    // Join every owned local job before restoring the terminal; each worker is
    // deadline-bounded, so a hung helper cannot extend this forever.
    jobs.shutdown().await;
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
    Job(Option<AppEvent>),
    Terminal(Event),
    TerminalEof,
    Signal,
    Tick,
    ShutdownTimeout,
    Render,
}

#[derive(Default)]
struct TickDeadline {
    deadline: Option<Instant>,
}

impl TickDeadline {
    fn arm(&mut self, requested: Option<Duration>, now: Instant) -> Option<Duration> {
        let Some(requested) = requested else {
            self.deadline = None;
            return None;
        };
        let requested_deadline = now + requested;
        let deadline = self.deadline.map_or(requested_deadline, |deadline| {
            deadline.min(requested_deadline)
        });
        self.deadline = Some(deadline);
        Some(deadline.saturating_duration_since(now))
    }

    fn fired(&mut self) {
        self.deadline = None;
    }

    fn is_due(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| deadline <= now)
    }
}

fn dispatch_due_tick(app: &mut App, deadline: &mut TickDeadline, now: Instant) -> bool {
    if !deadline.is_due(now) {
        return false;
    }
    deadline.fired();
    app.update(AppEvent::Tick);
    true
}

/// Drives the app until `AppCommand::Exit` (the agent is gone) or a fatal
/// shutdown timeout. App state is only ever changed through `App::update`;
/// this function owns the terminal, the RPC process and the timers.
async fn run_fullscreen(
    guard: &mut TerminalGuard,
    process: &mut RpcProcess,
    jobs: &mut LocalJobs,
    opts: &Args,
    config_path: PathBuf,
    tui_config: minicore_tui::config::TuiConfig,
) -> io::Result<()> {
    let workspace = if opts.workspace_explicit {
        opts.workspace.clone()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };
    let startup_session = if let Some(session_id) = opts.session.clone() {
        Some(StartupSession::Exact(session_id))
    } else if opts.continue_recent {
        Some(StartupSession::ContinueCurrentWorkspace)
    } else {
        None
    };
    let prefs = CliPrefs {
        profile: opts.profile.clone(),
        model: opts.model.clone(),
        reasoning: opts.reasoning,
        open_new_session_on_ready: opts.workspace_explicit && startup_session.is_none(),
        startup_session,
    };
    let mut app = App::with_tui_config(workspace, config_path, tui_config);
    app.set_cli_prefs(prefs);
    app.update(AppEvent::SetTheme(opts.theme));
    app.enable_async_layout();
    app.enable_async_decode();
    // `--debug` owns one writer thread with a bounded queue; the UI path only
    // enqueues a line (spec 13/§5.5). Dropping it at the end joins the writer.
    let debug_log = DebugLog::new(opts.debug);

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
    if run_commands(guard, process, &mut app, jobs, commands, &debug_log).await? {
        return Ok(());
    }

    let mut last_size = (0, 0);
    let mut last_render = Instant::now();
    let mut rpc_open = true;
    let mut rpc_cooldown_until: Option<Instant> = None;
    let mut tick_deadline = TickDeadline::default();

    loop {
        // Measured geometry flows back through `AppEvent::Viewport`; the
        // renderer never writes scroll state (spec 3, 7).
        if !app.editor_active() {
            let size = guard.terminal_mut().size()?;
            if (size.width as usize, size.height as usize) != last_size {
                app.update(AppEvent::TerminalSize {
                    width: size.width,
                    height: size.height,
                });
                last_size = (size.width as usize, size.height as usize);
            }
        }
        if shutdown_timeout_command(&app).is_some() {
            return Err(force_kill_and_report(process, &mut app).await);
        }
        let shutdown_deadline = app.shutdown_remaining();
        let tick_sleep = tick_deadline.arm(app.next_tick(), Instant::now());
        let render_deadline = if app.editor_active() {
            None
        } else {
            render_deadline(app.dirty, last_render.elapsed())
        };
        let rpc_cooldown =
            rpc_cooldown_until.map(|deadline| deadline.saturating_duration_since(Instant::now()));

        let selected = tokio::select! {
            () = sleep_or_pending(shutdown_deadline) => Selected::ShutdownTimeout,
            maybe = process.recv(), if rpc_open && rpc_cooldown_until.is_none() => Selected::Rpc(maybe),
            () = sleep_or_pending(rpc_cooldown), if rpc_cooldown_until.is_some() => Selected::RpcCooldown,
            maybe = jobs.events().recv() => Selected::Job(maybe),
            maybe = events.next(), if !app.editor_active() => match maybe {
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
            () = sleep_or_pending(tick_sleep) => Selected::Tick,
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
                let batch =
                    run_rpc_batch(guard, process, &mut app, jobs, event, &debug_log).await?;
                if batch.exit {
                    exit = true;
                } else if batch.channel_ended {
                    let commands = rpc_channel_ended(&mut rpc_open, &mut app);
                    if run_commands(guard, process, &mut app, jobs, commands, &debug_log).await? {
                        exit = true;
                    }
                } else {
                    rpc_cooldown_until = Some(Instant::now() + RPC_BATCH_COOLDOWN);
                }
            }
            Selected::Rpc(None) => {
                let commands = rpc_channel_ended(&mut rpc_open, &mut app);
                if run_commands(guard, process, &mut app, jobs, commands, &debug_log).await? {
                    exit = true;
                }
            }
            Selected::RpcCooldown => {
                rpc_cooldown_until = None;
                tokio::task::yield_now().await;
            }
            Selected::Job(Some(event)) => {
                let commands = if matches!(&event, AppEvent::JobFinished(JobOutcome::Editor { .. }))
                {
                    guard
                        .resume()
                        .map_err(|error| io::Error::other(error.to_string()))?;
                    app.update(event)
                } else {
                    app.update(event)
                };
                jobs.reap_finished().await;
                if run_commands(guard, process, &mut app, jobs, commands, &debug_log).await? {
                    exit = true;
                }
            }
            Selected::Job(None) => {
                return Err(io::Error::other(
                    "local job event channel ended unexpectedly",
                ));
            }
            Selected::Terminal(event) => {
                let commands = if app.editor_active() {
                    Vec::new()
                } else {
                    app.update(AppEvent::Terminal(event))
                };
                if run_commands(guard, process, &mut app, jobs, commands, &debug_log).await? {
                    exit = true;
                }
            }
            Selected::TerminalEof => {
                return Err(io::Error::other("terminal input stream ended"));
            }
            Selected::Signal => {
                signal_fired = true;
                let commands = app.update(AppEvent::ShutdownRequested);
                if run_commands(guard, process, &mut app, jobs, commands, &debug_log).await? {
                    exit = true;
                }
            }
            Selected::Tick => {
                dispatch_due_tick(&mut app, &mut tick_deadline, Instant::now());
            }
            Selected::Render => {
                if app.editor_active() {
                    continue;
                }
                let size = guard.terminal_mut().size()?;
                prepare_frame_with_jobs(&mut app, jobs, Rect::new(0, 0, size.width, size.height));
                guard.terminal_mut().draw(|frame| ui::render(frame, &app))?;
                last_render = Instant::now();
                app.update(AppEvent::Rendered);
            }
        }
        if exit {
            return Ok(());
        }

        // A ready RPC or terminal arm may win the select after the tick timer
        // has elapsed. Consume that overdue deadline before arming another
        // wait, so continuous non-Tick traffic cannot postpone animation.
        dispatch_due_tick(&mut app, &mut tick_deadline, Instant::now());

        // Render when state changed and the 30 FPS budget allows it; the
        // Rendered event clears the dirty flag so idle frames never draw.
        if !app.editor_active() && app.dirty && last_render.elapsed() >= RENDER_INTERVAL {
            let size = guard.terminal_mut().size()?;
            prepare_frame_with_jobs(&mut app, jobs, Rect::new(0, 0, size.width, size.height));
            guard.terminal_mut().draw(|frame| ui::render(frame, &app))?;
            last_render = Instant::now();
            app.update(AppEvent::Rendered);
        }
    }
}

/// Layout is coalesced with drawing, not repeated for every queued input/delta.
fn prepare_frame_with_jobs(app: &mut App, jobs: &mut LocalJobs, area: Rect) {
    if ui::layout::is_too_small(area) {
        return;
    }
    let screen = ui::layout::screen_layout(app, area);
    if app.context_panel().is_some() {
        return;
    }
    if app.changes().is_some() {
        let body = ui::workspace::file_body(screen.transcript);
        if let Some(request) = app.diff_layout_request(body.width.saturating_sub(17).max(1)) {
            let identity = request.identity.clone();
            if jobs.try_schedule_diff_layout(request) {
                app.mark_diff_layout_pending(identity);
            }
        }
        return;
    }
    if app.file_preview().is_some() {
        let body = ui::workspace::file_body(screen.transcript);
        if let Some(request) = app.file_layout_request(body.width.saturating_sub(9).max(1)) {
            let identity = request.identity.clone();
            if jobs.try_schedule_file_layout(request) {
                app.mark_file_layout_pending(identity);
            }
        }
        return;
    }
    if app.tool_detail().is_some() {
        let body = ui::tool_detail::body_area(screen.transcript);
        if let Some(request) = app.tool_layout_request(body.width.saturating_sub(1).max(1)) {
            let identity = request.identity.clone();
            if jobs.try_schedule_tool_layout(request) {
                app.mark_tool_layout_pending(identity);
            }
        }
        return;
    }
    let width = screen.content.width;
    if app.prepared_conversation(width).is_none() {
        if !app.async_layout_enabled() {
            return;
        }
        if app.active_view().is_some() {
            if let Some(durable) = app.cached_durable(width) {
                let prepared = ui::transcript::prepare_conversation_from_cache(app, width, durable);
                app.update(AppEvent::ConversationPrepared(prepared));
            } else if let Some(request) = app.layout_request(width) {
                let identity = request.identity.clone();
                if jobs.try_schedule_layout(request) {
                    app.mark_layout_pending(identity);
                }
            }
        } else {
            let prepared = ui::transcript::prepare_startup_conversation(app, width);
            app.update(AppEvent::ConversationPrepared(prepared));
        }
    }
    let Some(prepared) = app.prepared_conversation(width) else {
        if app.viewport != (0, 0) {
            app.update(AppEvent::Viewport {
                total_lines: 0,
                visible_rows: 0,
            });
        }
        return;
    };
    let total = prepared.total_rows();
    let visible = ui::transcript::visible_rows(app, total, screen.transcript.height);
    if app.viewport != (total, visible) {
        app.update(AppEvent::Viewport {
            total_lines: total,
            visible_rows: visible,
        });
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
    guard: &mut TerminalGuard,
    process: &mut RpcProcess,
    app: &mut App,
    jobs: &mut LocalJobs,
    first: RpcEvent,
    debug_log: &DebugLog,
) -> io::Result<RpcBatchResult> {
    let started = Instant::now();
    let mut processed = 0usize;
    let mut pending = Some(first);
    let mut channel_ended = false;
    while let Some(event) = pending {
        processed += 1;
        let commands = app.update(AppEvent::Rpc(event));
        if run_commands(guard, process, app, jobs, commands, debug_log).await? {
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

/// Classifies an outbound request for admission (spec §5.2). Cancel, close,
/// and shutdown keep four reserved FIFO slots so a saturated UI can still
/// stop work; everything else shares the first 28.
fn send_class(request: &OutgoingRequest) -> SendClass {
    match request.method {
        METHOD_TURN_CANCEL
        | METHOD_SESSION_COMPACT_CANCEL
        | METHOD_SESSION_CLOSE
        | METHOD_SHUTDOWN => SendClass::Control,
        _ => SendClass::Normal,
    }
}

/// Executes side effects without ever mutating `App` directly; failures (and
/// their follow-up commands) flow back as `AppEvent`s. Returns `true` when
/// the loop must exit (`AppCommand::Exit`).
///
/// RPC admission is synchronous and never awaits queue capacity; the
/// clipboard is started as an owned job. This function therefore never
/// blocks the UI loop on a full queue or a slow helper (spec §5.1/§5.2/§5.5).
fn schedule_pending_decode(app: &mut App, jobs: &mut LocalJobs) {
    let Some(request) = app.pending_decode_request() else {
        return;
    };
    if jobs.try_schedule_decode(request) {
        app.mark_decode_scheduled();
    }
}

async fn run_commands(
    guard: &mut TerminalGuard,
    process: &mut RpcProcess,
    app: &mut App,
    jobs: &mut LocalJobs,
    commands: Vec<AppCommand>,
    debug_log: &DebugLog,
) -> io::Result<bool> {
    schedule_pending_decode(app, jobs);
    let mut queue: VecDeque<AppCommand> = commands.into();
    while let Some(command) = queue.pop_front() {
        match command {
            AppCommand::Rpc(request) => {
                let start = Instant::now();
                debug_log.record(debug_log_line(&request, start));
                let class = send_class(&request);
                match process.try_send(request.clone(), class) {
                    Ok(()) => {}
                    Err(SendError::QueueFull(class)) => {
                        // Never written: revoke the pending registration and
                        // keep one retry intent for the exact target.
                        let more = app.update(AppEvent::RpcQueueFull { request, class });
                        queue.extend(more);
                    }
                    Err(error) => {
                        let more = app.update(AppEvent::RpcSendFailed {
                            id: request.id,
                            error: RpcError::from(error),
                        });
                        queue.extend(more);
                    }
                }
            }
            AppCommand::LocalScan(request) => {
                // One owned scan worker; a superseded queued scan is dropped
                // and its generation-checked result is ignored anyway.
                jobs.try_schedule_scan(*request);
            }
            AppCommand::StartExport(request) => {
                let request = *request;
                // The owned writer owns the target path, the temp file and the
                // bounded receiver. No file I/O happens on the main loop. If
                // the single slot is still busy the request is not started and
                // the App keeps its unstarted owner instead of orphaning one.
                let capture = request.capture.clone();
                let target = request.target.clone();
                match jobs.start_export(
                    request.capture,
                    request.target,
                    request.overwrite,
                    request.cancel,
                    request.rx,
                ) {
                    Ok(_) => {}
                    Err(_busy) => {
                        // This should be unreachable because App keeps the
                        // writer owner busy until its typed outcome arrives.
                        // Still settle the exact capture if the defensive
                        // guard fires; otherwise App would retain a sender
                        // forever and the next export could never start.
                        let more = app.update(AppEvent::JobFinished(JobOutcome::Export {
                            capture,
                            outcome: minicore_tui::jobs::ExportOutcome::Failed {
                                target: target.display().to_string(),
                                error: "the export writer slot is busy".to_owned(),
                                temp_removed: true,
                                target_state_unknown: false,
                            },
                        }));
                        queue.extend(more);
                    }
                }
            }
            AppCommand::PersistConfig(request) => {
                if let Err(_busy) = jobs.start_config_write(*request) {
                    let more = app.update(AppEvent::JobFinished(JobOutcome::Config {
                        path: app.config_path.clone(),
                        config: app.tui_config.clone(),
                        result: Err("another settings write is still in progress".to_owned()),
                    }));
                    queue.extend(more);
                }
            }
            AppCommand::StartEditor(request) => {
                let request = *request;
                if let Err(error) = guard.suspend() {
                    return Err(io::Error::other(format!(
                        "failed to suspend terminal for external editor: {error}"
                    )));
                }
                if let Err(_busy) = jobs.start_editor(request) {
                    guard
                        .resume()
                        .map_err(|error| io::Error::other(error.to_string()))?;
                    return Err(io::Error::other("another external editor is still running"));
                }
            }
            AppCommand::KillChild => process.kill_child(),
            AppCommand::CopySelection(text) => {
                match jobs.copy_to_clipboard(
                    text.session_id(),
                    text.revision(),
                    text.as_str().to_owned(),
                ) {
                    CopyAdmission::Started(_) => {}
                    CopyAdmission::Busy => {
                        // A definite refusal: the selection is kept so the
                        // user can retry, and no second thread/text is queued.
                        let more = app.update(AppEvent::JobFinished(JobOutcome::Clipboard {
                            session_id: text.session_id().to_owned(),
                            revision: text.revision(),
                            result: Err(
                                "clipboard is already handling the previous copy; the selection \
                                 is kept"
                                    .to_owned(),
                            ),
                        }));
                        queue.extend(more);
                    }
                }
            }
            AppCommand::Exit => return Ok(true),
        }
        schedule_pending_decode(app, jobs);
    }
    schedule_pending_decode(app, jobs);
    Ok(false)
}

/// `--debug` records only method, id, serialized byte count and duration to
/// a temp file (never message content, error text, reasoning, tool args, or
/// raw frames, spec 13). The UI path never touches the file: `record`
/// enqueues one line into a bounded channel and a single writer thread owns
/// the file for the whole run.
struct DebugLog {
    sender: Option<std::sync::mpsc::SyncSender<String>>,
    writer: Option<std::thread::JoinHandle<()>>,
}

/// One debug log line; pure and bounded so it can be tested without IO.
fn debug_log_line(request: &OutgoingRequest, start: Instant) -> String {
    let bytes = serde_json::to_string(request)
        .map(|line| line.len() + 1)
        .unwrap_or(0);
    format!(
        "id={} method={} bytes={} ms={:.1}\n",
        request.id.0,
        request.method,
        bytes,
        start.elapsed().as_secs_f64() * 1000.0
    )
}

impl DebugLog {
    /// Log lines that cannot keep up are dropped instead of stalling the loop
    /// or growing memory without bound.
    const QUEUE_CAPACITY: usize = 256;

    fn new(enabled: bool) -> Self {
        if !enabled {
            return Self {
                sender: None,
                writer: None,
            };
        }
        let path = std::env::temp_dir().join("minicore-tui-debug.log");
        Self::new_at(path)
    }

    fn new_at(path: std::path::PathBuf) -> Self {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<String>(Self::QUEUE_CAPACITY);
        let writer = std::thread::spawn(move || {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(path)
                .ok();
            if let Some(file) = file.as_ref() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
                }
            }
            let mut written = 0;
            for line in receiver {
                if written >= Self::MAX_LINES {
                    continue;
                }
                let Some(file) = file.as_mut() else {
                    break;
                };
                if file
                    .write_all(bound_debug_log_line(line).as_bytes())
                    .is_err()
                {
                    break;
                }
                written += 1;
            }
        });
        Self {
            sender: Some(sender),
            writer: Some(writer),
        }
    }

    /// Enqueues one line; never blocks and never performs file IO.
    fn record(&self, line: String) {
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(line);
        }
    }

    const MAX_LINES: usize = 200;
}

const MAX_DEBUG_LOG_LINE_BYTES: usize = 4096;

fn bound_debug_log_line(mut line: String) -> String {
    if !line.ends_with('\n') {
        line.push('\n');
    }
    if line.len() <= MAX_DEBUG_LOG_LINE_BYTES {
        return line;
    }
    let mut end = MAX_DEBUG_LOG_LINE_BYTES.saturating_sub(1);
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    line.truncate(end);
    line.push('\n');
    line
}

impl Drop for DebugLog {
    fn drop(&mut self) {
        self.sender = None;
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepare_conversation(app: &mut App, width: u16) {
        if app.prepared_conversation(width).is_none() {
            let prepared = ui::transcript::prepare_conversation(app, width);
            app.update(AppEvent::ConversationPrepared(prepared));
        }
    }

    fn prepare_frame(app: &mut App, area: Rect) {
        if ui::layout::is_too_small(area) {
            return;
        }
        let screen = ui::layout::screen_layout(app, area);
        prepare_conversation(app, screen.content.width);
        let Some(prepared) = app.prepared_conversation(screen.content.width) else {
            return;
        };
        let total = prepared.total_rows();
        let visible = ui::transcript::visible_rows(app, total, screen.transcript.height);
        if app.viewport != (total, visible) {
            app.update(AppEvent::Viewport {
                total_lines: total,
                visible_rows: visible,
            });
        }
    }

    #[test]
    fn debug_log_lines_are_content_free_and_the_disabled_log_is_a_noop() {
        let request = OutgoingRequest::new(
            minicore_tui::protocol::RequestId(9),
            "turn.send",
            serde_json::json!({"text": "SECRET-PROMPT"}),
        );
        let line = debug_log_line(&request, Instant::now());
        assert!(line.contains("id=9"));
        assert!(line.contains("method=turn.send"));
        assert!(line.contains("bytes="));
        assert!(line.contains("ms="));
        assert!(
            !line.contains("SECRET-PROMPT"),
            "debug logging must never record message content"
        );
        // The disabled logger allocates no thread and drops lines silently.
        let disabled = DebugLog::new(false);
        disabled.record("ignored".to_owned());

        let path = std::env::temp_dir().join(format!(
            "minicore-tui-debug-test-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        std::fs::write(&path, "stale\n").expect("seed debug log");
        let logger = DebugLog::new_at(path.clone());
        for _ in 0..DebugLog::MAX_LINES + 5 {
            logger.record("request\n".to_owned());
        }
        drop(logger);
        let contents = std::fs::read(&path).expect("read bounded debug log");
        assert!(contents.iter().filter(|byte| **byte == b'\n').count() <= DebugLog::MAX_LINES);
        assert!(contents.len() <= DebugLog::MAX_LINES * MAX_DEBUG_LOG_LINE_BYTES);
        assert!(!contents.windows(5).any(|window| window == b"stale"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn debug_log_lines_are_bounded_on_utf8_boundaries() {
        let line = bound_debug_log_line(format!("{}\n", "界".repeat(MAX_DEBUG_LOG_LINE_BYTES)));
        assert!(line.len() <= MAX_DEBUG_LOG_LINE_BYTES);
        assert!(line.ends_with('\n'));
        assert!(std::str::from_utf8(line.as_bytes()).is_ok());
    }

    #[test]
    fn prepare_frame_reuses_layout_and_reports_real_geometry() {
        let mut app = App::new(std::path::PathBuf::from("/synthetic"));
        let area = Rect::new(0, 0, 80, 24);
        prepare_frame(&mut app, area);
        let width = ui::layout::screen_layout(&app, area).content.width;
        let prepared = app.prepared_conversation(width).unwrap();
        assert_eq!(app.viewport.0, prepared.total_rows());
        let pointer = prepared.history_ptr();
        app.update(AppEvent::Rendered);
        for _ in 0..20 {
            prepare_frame(&mut app, area);
            assert_eq!(
                app.prepared_conversation(width).unwrap().history_ptr(),
                pointer
            );
            assert!(!app.dirty);
        }
    }

    #[test]
    fn small_terminal_skips_hidden_conversation_preparation() {
        let mut app = App::new(std::path::PathBuf::from("/synthetic"));
        let area = Rect::new(0, 0, 10, 4);
        prepare_frame(&mut app, area);
        let width = ui::layout::screen_layout(&app, area).content.width;
        assert!(app.prepared_conversation(width).is_none());
    }

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
    fn active_tick_deadline_survives_continuous_non_tick_events() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};

        let elapsed = Arc::new(AtomicU64::new(0));
        let clock_elapsed = Arc::clone(&elapsed);
        let base = Instant::now();
        let mut app =
            App::with_monotonic_clock(std::path::PathBuf::from("/synthetic"), move || {
                base + Duration::from_millis(clock_elapsed.load(Ordering::Relaxed))
            });
        let info: minicore_tui::protocol::SessionInfo = serde_json::from_value(serde_json::json!({
            "session_id": "ses_tick",
            "title": null,
            "profile": "coding",
            "workspace": "/synthetic",
            "model": "deep",
            "reasoning": "high",
            "loaded": true,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap();
        let mut view = minicore_tui::state::session::SessionView::new(info);
        view.live = Some(minicore_tui::state::turn::LiveLoop::new(
            minicore_tui::state::turn::LocalSubmissionId(1),
            "tick test".to_owned(),
        ));
        app.sessions.known.insert("ses_tick".to_owned(), view);
        app.sessions.active = Some("ses_tick".to_owned());

        let mut scheduler = TickDeadline::default();
        let mut tick_at = None;
        for millis in 0..=120 {
            elapsed.store(millis, Ordering::Relaxed);
            let now = base + Duration::from_millis(millis);
            let requested = app.next_tick().expect("live App must arm a tick");
            scheduler.arm(Some(requested), now);
            app.update(AppEvent::Rpc(RpcEvent::AgentStderr {
                bytes: 8,
                dropped: 0,
            }));
            if dispatch_due_tick(&mut app, &mut scheduler, now) {
                tick_at = Some(millis);
                break;
            }
        }
        assert_eq!(
            tick_at,
            Some(100),
            "continuous non-Tick events must not reset the active absolute deadline"
        );
        assert_eq!(app.frame_count, 1);

        app.sessions
            .known
            .get_mut("ses_tick")
            .expect("tick session")
            .live = None;
        elapsed.store(40, Ordering::Relaxed);
        app.update(AppEvent::Rpc(RpcEvent::AgentStderr {
            bytes: 4,
            dropped: 0,
        }));
        assert_eq!(
            scheduler.arm(app.next_tick(), base + Duration::from_millis(40)),
            None,
            "idle must disarm the tick timer"
        );
        assert_eq!(
            app.frame_count, 1,
            "idle traffic must not refresh the spinner"
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
            app.update(AppEvent::Rpc(RpcEvent::AgentStderr {
                bytes: 4,
                dropped: 0,
            }));
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
            "shutdown timed out; Agent force-terminated; last Agent stderr: agent stderr: 4 bytes"
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
        unknown.result_confirmation = minicore_tui::state::session::ResultConfirmation::NeedsRead;

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
        app.update(AppEvent::Rpc(RpcEvent::AgentStderr {
            bytes: "stderr before forced kill".len(),
            dropped: 0,
        }));
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
        assert!(message.contains(&format!(
            "agent stderr: {} bytes",
            "stderr before forced kill".len()
        )));
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
        unknown.result_confirmation = minicore_tui::state::session::ResultConfirmation::NeedsRead;
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
        assert!(report.contains("agent stderr: 42 bytes"));
        assert!(report.contains("result/save status unconfirmed"));
        assert!(report.contains("known persistence failure retained"));
        assert_eq!(
            app.sessions.known["known-failed"]
                .last_result
                .as_ref()
                .map(|result| result.persistence),
            Some(Some(minicore_tui::protocol::TurnPersistenceWire::Failed))
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
