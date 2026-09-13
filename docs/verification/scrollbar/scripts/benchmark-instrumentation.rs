#[test]
#[ignore = "manual matched scrollbar event benchmark"]
fn scrollbar_noop_event_benchmark() {
    let mut app = make_test_app(200);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| crate::ui::render(frame, &app))
        .unwrap();
    app.update(AppEvent::Rendered);
    reset_parse_count();
    let start = std::time::Instant::now();
    let mut frames = 0;
    for _ in 0..5000 {
        app.update(AppEvent::Terminal(crossterm::event::Event::Mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 4,
                row: 1,
                modifiers: KeyModifiers::empty(),
            },
        )));
        if app.dirty {
            terminal
                .draw(|frame| crate::ui::render(frame, &app))
                .unwrap();
            app.update(AppEvent::Rendered);
            frames += 1;
        }
    }
    println!(
        "scrollbar_noop events=5000 history_messages=200 frames={frames} elapsed_us={} markdown_parses={}",
        start.elapsed().as_micros(),
        parse_count()
    );
    assert_eq!(parse_count(), 0);
}

