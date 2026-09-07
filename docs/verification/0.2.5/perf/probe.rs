//! Analysis-only synthetic fixture. No RPC process, provider, config, or Store.
//! Fixture initialization is direct; all measured events use actual App::update.
use std::{hint::black_box, path::PathBuf, time::{Duration, Instant}};
use minicore_tui::{app::App, event::{AppEvent, RpcEvent}, protocol::*, state::{session::SessionView, transcript::{AssistantBlock, AssistantPart, TranscriptBlock}, turn::{LiveLoop, LocalSubmissionId}}, ui};
use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use serde_json::json;

const WIDTH: u16 = 100;
const HEIGHT: u16 = 40;
const MD: &str = "## Analysis section\n\nA synthetic **bold** paragraph with `inline code`, *emphasis*, and [reference](https://example.invalid/test). 中文内容用于验证真实宽字符排版路径。\n\n- First observation with a detailed explanation\n- Second observation with more supporting details\n\n```rust\nfn example() {\n    println!(\"synthetic fixture only\");\n}\n```\n\n";

fn make_app(count: usize, live_bytes: usize, reasoning: bool) -> App {
    let mut app = App::new(PathBuf::from("/tmp/minicore-render-analysis.XfzOcG"));
    app.connection = minicore_tui::app::ConnectionState::Ready;
    let info = serde_json::from_value(json!({
        "session_id":"synthetic", "title":"Synthetic performance fixture", "profile":"test",
        "workspace":"/tmp/minicore-render-analysis.XfzOcG", "model":"test", "reasoning":"high",
        "loaded":true, "created_at":"2026-01-01T00:00:00Z", "updated_at":"2026-01-01T00:00:00Z"
    })).unwrap();
    let mut view = SessionView::new(info);
    view.state = Some(serde_json::from_value(json!({"session_id":"synthetic", "status":"idle", "active_loop":null, "block_reason":null})).unwrap());
    let body = MD.repeat(3);
    for index in 0..count {
        view.transcript.blocks.push(TranscriptBlock::Assistant(AssistantBlock {
            index, loop_id: format!("history-{index}"), request_index: 0,
            model: "test".into(), reasoning_level: Reasoning::High,
            parts: vec![AssistantPart::Text(body.clone())], tool_calls: vec![],
            usage: UsageWire::default(), finish_reason: "stop".into(), terminal_error: None,
        }));
    }
    view.transcript.complete = true;
    view.transcript.invalidate();
    if live_bytes > 0 {
        let mut live = LiveLoop::new(LocalSubmissionId(1), "Synthetic live request".into());
        live.reference = Some(TurnRef { session_id: "synthetic".into(), loop_id: "live".into() });
        let request = live.ensure_request_mut(0, 0, "test".into(), Reasoning::High);
        let text = MD.repeat(live_bytes.div_ceil(MD.len()));
        if reasoning {
            request.parts.push(minicore_tui::state::turn::LivePart::Reasoning(text.clone()));
            request.reasoning_text = text;
        } else {
            request.parts.push(minicore_tui::state::turn::LivePart::Text(text.clone()));
            request.text = text;
        }
        view.live = Some(live);
    }
    app.sessions.known.insert("synthetic".into(), view);
    app.sessions.active = Some("synthetic".into());
    app.update(AppEvent::TerminalSize { width: WIDTH, height: HEIGHT });
    let prepared = ui::transcript::prepare_conversation(&app, content_width(&app));
    let visible = ui::transcript::visible_rows(&app, prepared.total_rows(), screen(&app).transcript.height);
    app.update(AppEvent::Viewport { total_lines: prepared.total_rows(), visible_rows: visible });
    app.update(AppEvent::ConversationPrepared(prepared));
    app.update(AppEvent::Rendered);
    app
}
fn screen(app: &App) -> ui::layout::ScreenLayout {
    ui::layout::screen_layout(app, Rect::new(0, 0, WIDTH, HEIGHT))
}
fn content_width(app: &App) -> u16 { screen(app).content.width }
fn ensure_prepared(app: &mut App) {
    let width = content_width(app);
    if app.prepared_conversation(width).is_none() {
        let p = ui::transcript::prepare_conversation(app, width);
        black_box(app.update(AppEvent::ConversationPrepared(p)));
    }
}
fn mouse(kind: MouseEventKind, column: u16, row: u16) -> AppEvent {
    AppEvent::Terminal(Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }))
}
fn delta(reasoning: bool) -> AppEvent {
    let event = serde_json::from_value(json!({"type":"output_delta", "data":{
        "turn":{"session_id":"synthetic", "loop_id":"live"}, "request_index":0,
        "channel":if reasoning {"reasoning"} else {"text"}, "delta":"x",
        "meta":{"session_id":"synthetic", "dropped_before":0}
    }})).unwrap();
    AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(RpcNotification::AgentEvent(event))))
}
fn frame(app: &mut App, terminal: &mut Terminal<TestBackend>, event: AppEvent) {
    black_box(app.update(event));
    ensure_prepared(app);
    terminal.draw(|f| ui::render(f, app)).unwrap();
    app.update(AppEvent::Rendered);
}
fn median(mut f: impl FnMut(), reps: usize) -> f64 {
    f();
    let mut times = Vec::new();
    for _ in 0..reps { let t = Instant::now(); f(); times.push(t.elapsed().as_secs_f64()*1000.0); }
    times.sort_by(f64::total_cmp);
    times[times.len()/2]
}
fn report(name: &str, count: usize, rows: usize, ms: f64) {
    println!("{name},{count},{rows},{ms:.4}");
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--assert-cache") {
        let mut app = make_app(1, 0, false);
        let width = content_width(&app);
        assert!(app.prepared_conversation(width).is_some());
        let revision = app.active_view().unwrap().transcript.render_revision;
        let offset = app.active_view().unwrap().scroll.offset;
        let commands = app.update(mouse(MouseEventKind::Moved, 5, 5));
        println!("after no-op MouseMoved: commands={} dirty={} cache_present={} revision_unchanged={} scroll_unchanged={}",
            commands.len(), app.dirty, app.prepared_conversation(width).is_some(),
            revision == app.active_view().unwrap().transcript.render_revision,
            offset == app.active_view().unwrap().scroll.offset);
        assert!(app.prepared_conversation(width).is_some(), "RED: no-op mouse movement must retain content layout");
        assert!(!app.dirty, "RED: no-op mouse movement must not request a redraw");
        return;
    }
    if args.iter().any(|a| a == "--profile") {
        let mut app = make_app(200, 0, false);
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
        println!("synthetic profiler pid={}", std::process::id());
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(8) { frame(&mut app, &mut terminal, mouse(MouseEventKind::Moved, 5, 5)); }
        return;
    }
    println!("profile={},viewport={}x{},bytes_per_message={}", if cfg!(debug_assertions) {"debug"} else {"release"}, WIDTH, HEIGHT, MD.len()*3);
    println!("operation,messages,prepared_rows,median_ms");
    for count in [10, 50, 200] {
        let mut app = make_app(count, 0, false);
        let width = content_width(&app);
        let rows = app.prepared_conversation(width).unwrap().total_rows();
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
        report("prepare_only", count, rows, median(|| { black_box(ui::transcript::prepare_conversation(&app, width)); }, 5));
        report("prepared_clone", count, rows, median(|| { black_box(app.prepared_conversation(width).unwrap().clone()); }, 5));
        report("cached_render", count, rows, median(|| { terminal.draw(|f| ui::render(f, &app)).unwrap(); }, 5));
        report("scrollbar_only", count, rows, median(|| {
            terminal.draw(|f| ui::scrollbar::render(f, Rect::new(0, 0, WIDTH, 34), rows, 34, 0, &app.theme.theme())).unwrap();
        }, 20));
        report("hover_event_prepare_render", count, rows, median(|| {
            frame(&mut app, &mut terminal, mouse(MouseEventKind::Moved, 5, 5));
        }, 5));
        report("tick_prepare_render", count, rows, median(|| { frame(&mut app, &mut terminal, AppEvent::Tick); }, 5));
        report("wheel_prepare_render", count, rows, median(|| {
            frame(&mut app, &mut terminal, mouse(MouseEventKind::ScrollUp, 5, 5));
        }, 5));
        let area = screen(&app).transcript;
        let visible = ui::transcript::visible_rows(&app, rows, area.height);
        let offset = app.active_view().unwrap().scroll.offset;
        let geo = ui::scrollbar::geometry(area, rows, visible, offset).unwrap();
        frame(&mut app, &mut terminal, mouse(MouseEventKind::Down(MouseButton::Left), geo.column as u16, geo.thumb_top as u16));
        assert!(app.scrollbar_preview_offset("synthetic").is_some(), "real scrollbar drag must start");
        let mut toggle = false;
        report("thumb_drag_prepare_render", count, rows, median(|| {
            toggle = !toggle;
            frame(&mut app, &mut terminal, mouse(MouseEventKind::Drag(MouseButton::Left), geo.column as u16, area.y + if toggle {2} else {area.height-3}));
        }, 5));
    }
    for (history, bytes, reasoning) in [(0, 1000, false), (0, 16000, false), (0, 64000, false), (0, 64000, true), (200, 1000, false)] {
        let mut app = make_app(history, bytes, reasoning);
        let rows = app.prepared_conversation(content_width(&app)).unwrap().total_rows();
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
        let name = format!("live_{}_{}_bytes", if reasoning {"reasoning"} else {"text"}, bytes);
        report(&name, history, rows, median(|| { frame(&mut app, &mut terminal, delta(reasoning)); }, 5));
    }
}
