//! Process entry point: parse arguments, load config, start the agent, then run
//! the terminal loop.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{Event as TermEvent, EventStream};
#[cfg(not(windows))]
use futures_util::StreamExt;
use tokio::sync::mpsc;

use minicore_tui::app::{App, Msg};
use minicore_tui::cli::{Args, USAGE};
use minicore_tui::config::{self, Home, Prepared, Request};
use minicore_tui::protocol::SessionInfo;
use minicore_tui::rpc::{self, Client, Incoming};
use minicore_tui::terminal;
use minicore_tui::ui::theme::{Palette, Styles};

#[cfg(feature = "e2e-observe")]
mod application_gate;

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

async fn next_backend_message(
    incoming: &mut mpsc::Receiver<Incoming>,
    messages: &mut mpsc::UnboundedReceiver<Msg>,
    agent_open: &mut bool,
) -> Option<Msg> {
    tokio::select! {
        biased;
        incoming = incoming.recv(), if *agent_open => match incoming {
            Some(Incoming::Event { event, bytes }) => Some(Msg::Agent { event, bytes }),
            Some(Incoming::Closed { status }) => {
                *agent_open = false;
                Some(Msg::AgentClosed(status.unwrap_or_else(|| "agent closed".into())))
            }
            None => {
                *agent_open = false;
                None
            }
        },
        message = messages.recv() => message,
    }
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
    let mut args = std::env::args().skip(1).peekable();
    if args
        .next_if(|first| first == rpc::AGENT_SUBCOMMAND)
        .is_some()
    {
        std::process::exit(minicore_agent::cli::run(args));
    }
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
        Ok(Prepared::Launch(launch)) => *launch,
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

    let remote = if args.remote {
        match minicore_tui::remote::Bound::bind(&launch.remote, home.dir().join("remote.json"))
            .await
        {
            Ok(bound) => Some(bound),
            Err(error) => {
                eprintln!("Remote: {error}");
                return 1;
            }
        }
    } else {
        None
    };

    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<Msg>();
    let (incoming_tx, mut incoming_rx) = mpsc::channel::<Incoming>(256);
    let client = match rpc::agent_command(launch.agent_binary.as_deref(), &launch.agent_config)
        .map_err(|error| format!("cannot locate the running executable: {error}"))
        .and_then(|command| Client::spawn(command, incoming_tx).map_err(|error| error.to_string()))
    {
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
    #[cfg(feature = "e2e-observe")]
    let mut apply_gate = match application_gate::ApplicationGate::from_env() {
        Ok(gate) => gate,
        Err(error) => {
            eprintln!("acceptance application gate setup failed: {error}");
            return 1;
        }
    };
    #[cfg(feature = "e2e-observe")]
    let mut apply_gate_failed = false;
    #[cfg(feature = "e2e-observe")]
    if let Some(path) = std::env::var_os("MINICORE_E2E_TRACE") {
        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
        {
            Ok(file) => app.start_observer(file),
            Err(error) => {
                eprintln!("acceptance trace open failed: {error}");
                return 1;
            }
        }
    }
    #[cfg(feature = "e2e-observe")]
    if apply_gate.is_some() && app.observer.is_none() {
        eprintln!("acceptance application gate requires MINICORE_E2E_TRACE");
        return 1;
    }
    app.launch = minicore_tui::app::LaunchSettings {
        profile: args.profile.clone(),
        model: args.model.clone(),
        reasoning: args.reasoning,
    };
    app.theme = theme;
    app.notifier.bell = launch.ui.bell;
    app.editor = launch.ui.editor;
    app.composed = launch.composed;
    app.default_profile = launch.default_profile;
    app.composer = minicore_tui::app::composer::Composer::with_history_file(home.history_file());
    app.home = Some(home);
    app.remote.config = Some(launch.remote);
    if let Some(bound) = remote {
        app.enter_remote(bound);
    }
    app.styles = Styles::of(Palette::for_kind(theme));

    let boot = {
        let client = client.clone();
        let tx = msg_tx.clone();
        tokio::spawn(async move { minicore_tui::app::tasks::bootstrap(client, tx).await })
    };
    // The loop must not poll `EventStream` itself: every poll fights the stream's
    // reader thread for the event lock and spins on `sched_yield` about 15 times.
    let (term_tx, mut term_rx) = mpsc::unbounded_channel::<TermEvent>();
    tokio::spawn(async move {
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
    let mut frames = Frames::new();
    let mut next_frame = tokio::time::Instant::now();
    let mut next_tick = tokio::time::Instant::now() + TICK;
    let mut startup = Some(args.session.clone());
    // A closed agent stays a closed agent: the channel is drained once and then
    // left alone, so a `biased` select cannot spin on a permanently ready read.
    let mut agent_open = true;

    loop {
        let wake = if app.animating() {
            Some(next_tick)
        } else {
            app.next_expiry()
        };
        tokio::select! {
            biased;
            message = next_backend_message(&mut incoming_rx, &mut msg_rx, &mut agent_open) => {
                if let Some(message) = message {
                    #[cfg(feature = "e2e-observe")]
                    if let Some(gate) = &mut apply_gate
                        && let Err(error) = gate.before_apply(&message,&mut app).await {
                        eprintln!("acceptance application gate failed: {error}");
                        apply_gate_failed = true;
                        break;
                    }
                    app.on_msg(message);
                    frames.mark(Cause::Event);
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

        // Startup opens the session `--session` or `--continue` names, once;
        // without one the screen is a draft that creates a session at its
        // first message.
        if app.ready
            && let Some(session) = startup.take()
        {
            let recent = args
                .continue_recent
                .then(|| pick_recent(&app.sessions, &workspace))
                .flatten();
            if let Some(id) = session.or_else(|| recent.cloned()) {
                app.open_session(id);
                frames.mark(Cause::Event);
            }
        }

        if frames.wanted() && tokio::time::Instant::now() >= next_frame {
            let started = Instant::now();
            if guard.is_active()
                && let Err(error) = guard.draw(|frame| minicore_tui::ui::draw(frame, &mut app))
            {
                eprintln!("draw failed: {error}");
                break;
            }
            app.sync_remote();
            frames.drawn(started.elapsed());
            next_frame = tokio::time::Instant::now() + FRAME;
        }
        app.sync_terminal();
        if app.quit {
            break;
        }
    }

    #[cfg(feature = "e2e-observe")]
    app.observe_checkpoint(true);
    app.sync_remote();
    app.remote.shutdown().await;
    let _ = client.shutdown().await;
    let _ = guard.restore();
    frames.print(&app);
    print!("{}", app.exit_output());
    boot.abort();
    #[cfg(feature = "e2e-observe")]
    if apply_gate_failed {
        return 1;
    }
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
    use tokio::task::JoinHandle;

    use super::*;
    use minicore_tui::app::{Action, Session, SessionOp};
    use minicore_tui::protocol::AgentEvent;
    use minicore_tui::transcript::Block;

    const TEST_TIMEOUT: Duration = Duration::from_secs(10);

    struct SplitDispatch {
        app: App,
        client: Arc<Client>,
        incoming: mpsc::Receiver<Incoming>,
        messages: mpsc::UnboundedReceiver<Msg>,
        posted: mpsc::UnboundedReceiver<()>,
        agent_open: bool,
        requests: BufReader<ReadHalf<DuplexStream>>,
        output: WriteHalf<DuplexStream>,
        forward: JoinHandle<()>,
        control: Value,
    }

    impl SplitDispatch {
        fn new() -> Self {
            let (client_pipe, agent_pipe) = tokio::io::duplex(64 * 1024);
            let (client_read, client_write) = tokio::io::split(client_pipe);
            let (agent_read, output) = tokio::io::split(agent_pipe);
            let (incoming_tx, incoming) = mpsc::channel(256);
            let client = Client::start(client_write, client_read, tokio::io::empty(), incoming_tx);
            let (app_tx, mut app_rx) = mpsc::unbounded_channel();
            let (message_tx, messages) = mpsc::unbounded_channel();
            let (posted_tx, posted) = mpsc::unbounded_channel();
            let forward = tokio::spawn(async move {
                while let Some(message) = app_rx.recv().await {
                    if message_tx.send(message).is_err() || posted_tx.send(()).is_err() {
                        break;
                    }
                }
            });
            let workspace = std::env::temp_dir();
            let mut app = App::new(client.clone(), app_tx, workspace.clone());
            let info = serde_json::from_value(json!({
                "session_id": "s1", "title": "Test", "profile": "coding",
                "workspace": workspace, "model": "mock", "reasoning": "auto",
                "loaded": true, "updated_at": "2026-01-01T00:00:00Z"
            }))
            .unwrap();
            app.session = Some(Session::new(info, 1));
            let session = app.session.as_mut().unwrap();
            session.load = Some("load_1".into());
            session.recovery.begin("load_1", 1);
            app.ready = true;
            Self {
                app,
                client,
                incoming,
                messages,
                posted,
                agent_open: true,
                requests: BufReader::new(agent_read),
                output,
                forward,
                control: json!({"session_id":"s1","load":"load_1","revision":0,"execution":{"type":"none"},"queue":queue(vec![]),"completions":{"through_seq":0,"retained_from_seq":1,"entries":[],"truncated":false}}),
            }
        }

        /// The next request the app writes, whatever it is.
        async fn next_request(&mut self) -> Value {
            let mut line = String::new();
            assert_ne!(self.requests.read_line(&mut line).await.unwrap(), 0);
            serde_json::from_str(&line).unwrap()
        }

        /// The next request, which must be `method`. A control read the app
        /// makes for its own reasons is answered and skipped, because v2 has the
        /// client pull `session.state` after every control change and that read
        /// is not what the caller is asserting about.
        async fn request(&mut self, method: &str) -> Value {
            loop {
                let request = self.next_request().await;
                if request["method"] == method {
                    return request;
                }
                assert_eq!(
                    request["method"], "session.state",
                    "unexpected request before {method}"
                );
                let id = request["id"].clone();
                self.control["revision"] = json!(self.control["revision"].as_u64().unwrap() + 1);
                self.send(json!({"jsonrpc": "2.0", "id": id, "result": {
                    "session_id": "s1", "status": "idle", "snapshot":self.control
                }}))
                .await;
            }
        }

        fn install(&mut self, execution: Value) {
            self.control["revision"] = json!(self.control["revision"].as_u64().unwrap() + 1);
            self.control["execution"] = execution;
            self.app
                .apply_snapshot(serde_json::from_value(self.control.clone()).unwrap());
        }

        async fn send(&mut self, frame: Value) {
            self.output
                .write_all(format!("{frame}\n").as_bytes())
                .await
                .unwrap();
            self.output.flush().await.unwrap();
        }

        async fn posted(&mut self) {
            self.posted
                .recv()
                .await
                .expect("the reducer message was posted");
        }

        async fn select(&mut self) -> Msg {
            next_backend_message(&mut self.incoming, &mut self.messages, &mut self.agent_open)
                .await
                .expect("a backend message")
        }

        async fn ordered_response_and_event(&mut self, response: Value, event: Value) {
            let client = self.client.clone();
            let fence = tokio::spawn(async move { client.call_raw("agent.ping", &()).await });
            let request = self.request("agent.ping").await;
            self.send(response).await;
            self.send(event).await;
            self.send(json!({"jsonrpc": "2.0", "id": request["id"], "result": {}}))
                .await;
            fence.await.unwrap().expect("the ordered fence response");
            self.posted().await;
            // v2 composes a control read after the reply, so the message
            // channel may hold more than the reply. The event channel is what
            // this test proves: the event is ready before application, so
            // applying it first preserves the successor's opening delta.
            assert_eq!(
                self.incoming.len(),
                1,
                "the event is ready before application"
            );
            assert!(
                !self.messages.is_empty(),
                "the reply is ready before application"
            );
            println!("FIFO wire: response, event, fence; both application channels ready");
        }
    }

    impl Drop for SplitDispatch {
        fn drop(&mut self) {
            self.forward.abort();
        }
    }

    fn event(kind: &str, mut data: Value) -> Value {
        data["meta"]["load"] = json!("load_1");
        json!({"jsonrpc": "2.0", "method": "agent.event", "params": {"type": kind, "data": data}})
    }

    fn queue(items: Vec<Value>) -> Value {
        json!({"items": items, "paused": false})
    }

    fn entry(id: &str, text: &str) -> Value {
        json!({"id": id, "mode": "steer", "text": text})
    }

    #[tokio::test(start_paused = true)]
    async fn w0_dispatch_keeps_event_priority_and_disables_a_closed_agent() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (incoming_tx, mut incoming) = mpsc::channel(4);
            let (message_tx, mut messages) = mpsc::unbounded_channel();
            let event: AgentEvent = serde_json::from_value(json!({
                "type": "input_queue", "data": {
                    "meta": {"session_id": "s1", "dropped_before": 0}, "queue": queue(vec![])
                }
            }))
            .unwrap();
            incoming_tx
                .send(Incoming::Event {
                    event: Box::new(event),
                    bytes: 0,
                })
                .await
                .unwrap();
            message_tx.send(Msg::Flash("message".into())).unwrap();
            let mut agent_open = true;
            let first = next_backend_message(&mut incoming, &mut messages, &mut agent_open).await;
            assert!(matches!(first, Some(Msg::Agent { .. })));
            let second = next_backend_message(&mut incoming, &mut messages, &mut agent_open).await;
            assert!(matches!(second, Some(Msg::Flash(_))));

            incoming_tx
                .send(Incoming::Closed { status: None })
                .await
                .unwrap();
            message_tx.send(Msg::Flash("after close".into())).unwrap();
            let closed = next_backend_message(&mut incoming, &mut messages, &mut agent_open).await;
            assert!(matches!(closed, Some(Msg::AgentClosed(reason)) if reason == "agent closed"));
            assert!(!agent_open);
            let next = next_backend_message(&mut incoming, &mut messages, &mut agent_open).await;
            assert!(matches!(next, Some(Msg::Flash(_))));
        })
        .await
        .expect("dispatcher timed out");
    }

    #[tokio::test(start_paused = true)]
    async fn a_newer_snapshot_cannot_be_undone_by_an_earlier_push_reply() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let mut h = SplitDispatch::new();
            h.control["queue"] = queue(vec![entry("i1","queued")]);
            h.install(json!({"type":"none"}));
            h.app.composer.set_text("queued");
            h.app.on_action(Action::Submit);
            let request = h.request("input.push").await;
            assert_eq!(request["params"]["text"], "queued");
            let response = json!({"jsonrpc": "2.0", "id": request["id"], "result": {
                "id": "i1", "started": null, "queue": queue(vec![entry("i1", "queued")])
            }});
            h.control["queue"] = queue(vec![]);
            h.control["revision"] = json!(h.control["revision"].as_u64().unwrap()+1);
            let event = event("session_snapshot", json!({
                "meta": {"session_id": "s1", "dropped_before": 0}, "snapshot":h.control
            }));
            h.ordered_response_and_event(response, event).await;

            let first = h.select().await;
            assert!(
                matches!(&first, Msg::Agent { event, .. } if matches!(**event, AgentEvent::SessionSnapshot { .. }))
            );
            h.app.on_msg(first);
            let second = h.select().await;
            assert!(matches!(&second, Msg::LocalPushed(..)));
            h.app.on_msg(second);
            let queue = &h.app.session.as_ref().unwrap().queue;
            assert!(queue.items.is_empty(), "the consumed input must not return to the queue: {queue:?}");
        }).await.expect("split dispatch timed out");
    }

    #[tokio::test(start_paused = true)]
    async fn a_snapshot_written_before_a_receipt_needs_no_legacy_hint_to_prevent_resurrection() {
        tokio::time::timeout(TEST_TIMEOUT,async {
            let mut h = SplitDispatch::new();
            h.install(json!({"type":"none"}));
            h.app.composer.set_text("accepted then finished");
            h.app.on_action(Action::Submit);
            let push = h.request("input.push").await;
            let client = h.client.clone();
            let fence = tokio::spawn(async move { client.call_raw("agent.ping",&()).await });
            let ping = h.request("agent.ping").await;
            h.control["revision"] = json!(2);
            h.control["queue"] = queue(vec![]);
            h.control["completions"] = json!({"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":"A","completion_kind":"reported","outcome_kind":"completed","persistence":"failed","availability":"unavailable","unavailable_reason":"expired"}]});
            h.send(event("session_snapshot",json!({"meta":{"session_id":"s1","load":"load_1","dropped_before":0},"snapshot":h.control}))).await;
            h.send(json!({"jsonrpc":"2.0","id":push["id"],"result":{"id":"i1","accepted":true,"load":"load_1","started":{"turn":{"session_id":"s1","loop_id":"A"},"input":entry("i1","accepted then finished")},"queue":queue(vec![entry("stale","old queue")])}})).await;
            h.send(json!({"jsonrpc":"2.0","id":ping["id"],"result":{}})).await;
            fence.await.unwrap().unwrap();
            h.posted().await;
            let snapshot = h.select().await;
            assert!(matches!(&snapshot,Msg::Agent { event,.. } if matches!(**event,AgentEvent::SessionSnapshot { .. })));
            h.app.on_msg(snapshot);
            let receipt = h.select().await;
            assert!(matches!(&receipt,Msg::LocalPushed(..)));
            h.app.on_msg(receipt);
            let session = h.app.session.as_ref().unwrap();
            assert!(session.queue.items.is_empty());
            assert!(session.live.is_none());
            assert_eq!(session.observed_loop(),None);
            assert_eq!(session.recovery.observed_through,1);
        }).await.expect("FIFO dispatch did not finish");
    }

    #[cfg(feature = "e2e-observe")]
    #[tokio::test(start_paused = true)]
    async fn application_pause_leaves_the_stdio_reader_running_and_applies_fifo_after_release() {
        tokio::time::timeout(TEST_TIMEOUT,async {
            let mut h = SplitDispatch::new();
            h.install(json!({"type":"none"}));
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().to_path_buf();
            let mut gate = application_gate::ApplicationGate::new(path.clone()).unwrap();
            std::fs::write(path.join("arm"),b"").unwrap();
            h.control["revision"] = json!(2);
            h.control["queue"] = queue(vec![entry("first","first observation")]);
            let held:AgentEvent = serde_json::from_value(json!({"type":"session_snapshot","data":{"meta":{"session_id":"s1","load":"load_1","dropped_before":0},"snapshot":h.control}})).unwrap();
            let held = Msg::Agent { event:Box::new(held),bytes:0 };
            h.control["revision"] = json!(3);
            h.control["queue"] = queue(vec![]);
            let later = event("session_snapshot",json!({"meta":{"session_id":"s1","load":"load_1","dropped_before":0},"snapshot":h.control}));
            let read_while_paused = {
            let pausing = gate.before_apply(&held,&mut h.app);
            tokio::pin!(pausing);
            tokio::select! { result = &mut pausing => panic!("gate returned before release: {result:?}"), _ = async {
                while !path.join("ready").exists() { tokio::task::yield_now().await; }
            } => {} }
            h.output.write_all(format!("{later}\n").as_bytes()).await.unwrap();
            h.output.flush().await.unwrap();
            let read_while_paused = h.incoming.recv().await.unwrap();
            assert!(matches!(&read_while_paused,Incoming::Event { event,.. } if matches!(**event,AgentEvent::SessionSnapshot { .. })));
            std::fs::write(path.join("release"),b"").unwrap();
            pausing.await.unwrap();
            read_while_paused
            };
            h.app.on_msg(held);
            assert_eq!(h.app.session.as_ref().unwrap().queue.items[0].id,"first");
            let Incoming::Event { event,bytes } = read_while_paused else { unreachable!() };
            h.app.on_msg(Msg::Agent { event,bytes });
            assert_eq!(h.app.session.as_ref().unwrap().recovery.revision,3);
            assert!(h.app.session.as_ref().unwrap().queue.items.is_empty());
        }).await.expect("application gate did not release");
    }

    #[cfg(feature = "e2e-observe")]
    #[tokio::test(start_paused = true)]
    async fn an_unreleased_application_gate_times_out_without_applying_the_held_message() {
        let mut h = SplitDispatch::new();
        h.install(json!({"type":"none"}));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_path_buf();
        let mut gate = application_gate::ApplicationGate::new(path.clone()).unwrap();
        std::fs::write(path.join("arm"), b"").unwrap();
        let held = Msg::Session(1, Box::new(SessionOp::Done(Ok(json!({})))));
        let error = gate.before_apply(&held, &mut h.app).await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(path.join("ready").exists());
        assert_eq!(h.app.session.as_ref().unwrap().recovery.revision, 1);
        std::fs::write(path.join("release"), b"").unwrap();
        assert!(
            application_gate::ApplicationGate::new(path).is_err(),
            "stale gate state cannot be reused as a fresh acceptance run"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn w0_a_successors_first_delta_survives_dispatch_before_the_wait_reply() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let mut h = SplitDispatch::new();
            h.app.composer.set_text("first prompt");
            h.app.on_action(Action::Submit);
            let push = h.request("input.push").await;
            h.send(json!({"jsonrpc": "2.0", "id": push["id"], "result": {
                "id": "i1", "queue": queue(vec![]),
                "started": {"turn": {"session_id": "s1", "loop_id": "A"}, "input": entry("i1", "first prompt")}
            }})).await;
            h.posted().await;
            let started = h.select().await;
            h.app.on_msg(started);
            assert!(h.app.session.as_ref().unwrap().live.is_none(), "a receipt is not execution authority");
            h.install(json!({"type":"running","loop_id":"A","input":entry("i1","first prompt"),"request_index":0,"phase":"running_model"}));
            assert_eq!(h.app.session.as_ref().unwrap().live.as_ref().unwrap().turn.loop_id, "A");
            let wait = h.request("turn.wait").await;
            assert_eq!(wait["params"]["loop_id"], "A");
            let response = json!({"jsonrpc": "2.0", "id": wait["id"], "result": {
                "turn": {"session_id": "s1", "loop_id": "A"}, "outcome": {"type": "completed"},
                "queue": queue(vec![]), "next": {
                    "turn": {"session_id": "s1", "loop_id": "B"}, "input": entry("i2", "second prompt")
                }
            }});
            let event = event("output_delta", json!({
                "meta": {"session_id": "s1", "dropped_before": 0},
                "turn": {"session_id": "s1", "loop_id": "B"}, "request_index": 0,
                "channel": "text", "delta": "first part of B"
            }));
            h.ordered_response_and_event(response, event).await;

            let first = h.select().await;
            assert!(
                matches!(&first, Msg::Agent { event, .. } if matches!(**event, AgentEvent::OutputDelta { .. }))
            );
            h.app.on_msg(first);
            assert!(!h.app.transcript_blocks().iter().any(|block|block.loop_id() == Some("B")), "unknown B stays invisible until identity confirmation");
            assert!(h.app.session.as_ref().unwrap().early_bytes > 0);
            // The reply and the control read it asks for share a channel; only
            // the wait matters here, and applying a control answer first does
            // not move the view off the successor.
            let second = loop {
                let next = h.select().await;
                if matches!(&next, Msg::Session(1, op) if matches!(**op, SessionOp::Waited(..))) {
                    break next;
                }
                h.app.on_msg(next);
            };
            h.app.on_msg(second);
            h.control["completions"] = json!({"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":"A","completion_kind":"reported","outcome_kind":"completed","persistence":"persisted","availability":"stored","next_loop_id":"B"}]});
            h.install(json!({"type":"running","loop_id":"B","input":entry("i2","second prompt"),"request_index":0,"phase":"running_model"}));
            assert!(h.app.transcript_blocks().iter().any(|block| matches!(block,
                Block::Assistant { key, text, .. } if key.loop_id == "B" && text == "first part of B"
            )), "the successor's first output must survive until B is installed");
        }).await.expect("split dispatch timed out");
    }
}
