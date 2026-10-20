//! Render-path timings over a long session. Ignored by default; run with
//! `cargo test --release -- --ignored --nocapture perf`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::mpsc;

use crate::app::{App, Msg, Scroll, Session};
use crate::protocol::read::UserKind;
use crate::protocol::{LoopOutcome, Reasoning, SessionInfo, ToolRef};
use crate::transcript::block::{ToolState, Transcript};
use crate::transcript::{Block, RequestKey, ToolCall};
use crate::ui::theme::{Palette, Styles};

const BLOCKS: usize = 5000;
const WIDTH: u16 = 140;
const HEIGHT: u16 = 45;

struct Bench {
    app: App,
    terminal: Terminal<TestBackend>,
    _runtime: tokio::runtime::Runtime,
}

impl Bench {
    fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let (tx, _rx) = mpsc::unbounded_channel::<Msg>();
        let (incoming_tx, _incoming_rx) = mpsc::channel(4);
        let client = runtime
            .block_on(async {
                crate::rpc::Client::spawn(
                    std::path::Path::new("/bin/sleep"),
                    std::path::Path::new("/dev/null"),
                    incoming_tx,
                )
            })
            .expect("client");
        let mut app = App::new(client, tx, PathBuf::from("/work/minicore-tui"));
        app.styles = Styles::of(Palette::dark());
        app.ready = true;
        let mut session = Session::new(info(), 1);
        session.transcript = Transcript::default();
        app.session = Some(session);
        Self {
            app,
            terminal: Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("terminal"),
            _runtime: runtime,
        }
    }

    fn transcript(&mut self) -> &mut Transcript {
        &mut self.app.session.as_mut().expect("session").transcript
    }

    fn draw(&mut self) -> Duration {
        let start = Instant::now();
        self.terminal
            .draw(|frame| crate::ui::draw(frame, &mut self.app))
            .expect("draw");
        start.elapsed()
    }

    fn resize(&mut self, width: u16) {
        self.terminal.backend_mut().resize(width, HEIGHT);
    }
}

fn info() -> SessionInfo {
    SessionInfo {
        session_id: "ses_perf".into(),
        title: None,
        profile: "coding".into(),
        workspace: "/work/minicore-tui".into(),
        model: "gpt-6-luna".into(),
        reasoning: Reasoning::Max,
        loaded: true,
        updated_at: String::new(),
    }
}

fn user(index: usize) -> Block {
    Block::User {
        text: format!("Prompt {index}: please look at the parser and explain why it is slow."),
        kind: UserKind::Prompt,
    }
}

fn answer(index: usize) -> Block {
    Block::Assistant {
        key: RequestKey {
            loop_id: format!("loop_{index}"),
            request_index: 0,
        },
        reasoning: "Read the module first.\nThen compare the two paths.".into(),
        text: format!(
            "## Result {index}\n\nThe parser **re-reads** the whole input on every call, which \
             is `O(n)` per delta.\n\n- cache the prefix\n- re-parse only the tail\n- keep the \
             rows shared\n\n```rust\nfn main() {{\n    println!(\"{index}\");\n}}\n```"
        ),
        streaming: false,
        expanded: None,
    }
}

/// A bash card whose output is close to the 400-line, 64 KiB bound.
fn tool(index: usize) -> Block {
    let mut call = ToolCall::new(
        ToolRef {
            session_id: "ses_perf".into(),
            loop_id: format!("loop_{index}"),
            request_index: 0,
            tool_call_id: format!("call_{index}"),
        },
        "bash",
    );
    call.detail = format!("$ cargo test --lib module_{index}");
    call.expanded_input = Some(format!("cargo test --lib module_{index}"));
    call.state = ToolState::Succeeded;
    call.exit_code = Some(0);
    call.duration = Some(Duration::from_millis(1200));
    call.output = (0..400)
        .map(|line| {
            format!(
                "{line:>3} test module_{index}::case_{line} ... ok {}",
                "-".repeat(110)
            )
        })
        .collect();
    call.output_lines = Some(400);
    Block::Tool(Box::new(call))
}

fn turn_end(index: usize) -> Block {
    Block::TurnEnd {
        loop_id: format!("loop_{index}"),
        outcome: LoopOutcome::Completed,
        duration: Some(Duration::from_secs(9)),
        requests: 2,
        tools: 2,
        usage: Default::default(),
        changes: None,
    }
}

fn long_session() -> Bench {
    let mut bench = Bench::new();
    let transcript = bench.transcript();
    for turn in 0..BLOCKS / 5 {
        transcript.push(user(turn));
        transcript.push(answer(turn));
        transcript.push(tool(turn * 2));
        transcript.push(tool(turn * 2 + 1));
        transcript.push(turn_end(turn));
    }
    bench
}

/// About 50 KB of markdown: headings, prose, lists and fenced code.
fn long_answer() -> String {
    let mut text = String::new();
    let mut section = 0;
    while text.len() < 50_000 {
        text.push_str(&format!("## Section {section}\n\n"));
        text.push_str(&"The renderer keeps a stable prefix and parses the tail again. ".repeat(6));
        text.push_str("\n\n- first point\n- second point\n- third point\n\n```rust\n");
        for line in 0..10 {
            text.push_str(&format!("let value_{line} = compute({section}, {line});\n"));
        }
        text.push_str("```\n\n");
        section += 1;
    }
    text
}

fn micros(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e6
}

fn percentile(samples: &mut [Duration], p: f64) -> Duration {
    samples.sort();
    samples[((samples.len() - 1) as f64 * p) as usize]
}

fn mean_micros(samples: &[Duration]) -> f64 {
    samples.iter().map(|sample| micros(*sample)).sum::<f64>() / samples.len() as f64
}

#[test]
#[ignore]
fn perf() {
    let built = Instant::now();
    let mut bench = long_session();
    println!("build {BLOCKS} blocks: {:?}", built.elapsed());
    println!("first draw: {:?}", bench.draw());

    let mut steady: Vec<Duration> = (0..300).map(|_| bench.draw()).collect();
    println!(
        "steady draw: p50 {:.0}us p95 {:.0}us max {:.0}us",
        micros(percentile(&mut steady, 0.5)),
        micros(percentile(&mut steady, 0.95)),
        micros(*steady.last().unwrap()),
    );

    let text = long_answer();
    let key = RequestKey {
        loop_id: "loop_live".into(),
        request_index: 0,
    };
    bench.transcript().push(user(BLOCKS));
    let step = text.len().div_ceil(200);
    let mut frames = Vec::new();
    for delta in text.as_bytes().chunks(step) {
        let delta = std::str::from_utf8(delta).expect("ascii");
        if let Block::Assistant { text, .. } = bench.transcript().assistant_mut(&key) {
            text.push_str(delta);
        }
        frames.push(bench.draw());
    }
    let (start, end) = (mean_micros(&frames[..5]), mean_micros(&frames[195..]));
    println!(
        "streaming {} bytes in {} deltas: first 5 frames {start:.0}us, last 5 frames {end:.0}us, ratio {:.1}x, max {:.0}us",
        text.len(),
        frames.len(),
        end / start,
        micros(*frames.iter().max().unwrap()),
    );

    for expanded in [true, false] {
        bench.app.tools_expanded = expanded;
        println!("ctrl+o -> expanded={expanded}: {:?}", bench.draw());
    }
    bench.app.tools_expanded = true;
    bench.draw();
    bench.app.scroll = Scroll::Anchor {
        block: BLOCKS / 2,
        offset: 0,
    };
    println!("jump into stale cards after ctrl+o: {:?}", bench.draw());
    let pages: Vec<Duration> = (0..10)
        .map(|_| {
            bench.app.scroll_by(-(HEIGHT as isize));
            bench.draw()
        })
        .collect();
    println!(
        "page up over stale cards: mean {:.0}us max {:.0}us",
        mean_micros(&pages),
        micros(*pages.iter().max().unwrap()),
    );
    bench.app.scroll = Scroll::Follow;
    bench.app.tools_expanded = false;
    bench.draw();

    bench.app.show_reasoning = !bench.app.show_reasoning;
    println!("ctrl+t: {:?}", bench.draw());

    bench.resize(WIDTH - 20);
    println!("width {WIDTH} -> {}: {:?}", WIDTH - 20, bench.draw());

    let transcript = bench.transcript();
    for index in (0..20).rev() {
        transcript.insert(0, user(10_000 + index));
    }
    println!("prepend 20 items: {:?}", bench.draw());
}
