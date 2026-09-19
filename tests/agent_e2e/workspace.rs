use super::*;
use minicore_tui::app::workspace::WorkspaceQuery;
use minicore_tui::protocol::workspace::FileStatus;
use minicore_tui::state::workspace::ReturnTarget;

async fn browser_ready(process: &mut RpcProcess, app: &mut App) {
    pump_until(process, app, |a| a.workspace_browser().is_some_and(|b| b.stopped_by.is_some() || b.error.is_some()) && !a.pending_requests.values().any(|k| matches!(k,RequestKind::Workspace { kind, .. } if *kind != WorkspaceQuery::File))).await.unwrap();
    assert!(app.workspace_browser().unwrap().error.is_none());
}
async fn file_ready(process: &mut RpcProcess, app: &mut App) {
    pump_until(process, app, |a| {
        a.file_preview()
            .is_some_and(|f| f.status.is_some() || f.error.is_some())
            && !a.pending_requests.values().any(|k| {
                matches!(
                    k,
                    RequestKind::Workspace {
                        kind: WorkspaceQuery::File,
                        ..
                    }
                )
            })
    })
    .await
    .unwrap();
    assert!(app.file_preview().unwrap().error.is_none());
}
async fn layout_file(app: &mut App) {
    // Exercise the real shared serialized worker, not a synchronous renderer build.
    let mut jobs = minicore_tui::jobs::LocalJobs::new();
    let request = app.file_layout_request(70).unwrap();
    app.mark_file_layout_pending(request.identity.clone());
    assert!(jobs.try_schedule_file_layout(request));
    let event = tokio::time::timeout(TIMEOUT, jobs.events().recv())
        .await
        .unwrap()
        .unwrap();
    app.update(event);
    assert!(app.file_preview().unwrap().layout.is_some());
    jobs.shutdown().await;
}
async fn preview(process: &mut RpcProcess, app: &mut App, path: &str) {
    let commands = app.open_file_preview(path.into(), None, ReturnTarget::Conversation);
    dispatch_commands(process, app, commands).await.unwrap();
    file_ready(process, app).await;
}
#[test]
#[ignore = "requires fixed MINICORE_AGENT_BIN; synthetic workspace, no Provider calls"]
fn e2e_workspace_files_and_literal_search_page_without_attaching_content() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    let directory = env.workspace_path.join("子 dir");
    std::fs::create_dir(&directory).unwrap();
    for i in 0..230 {
        std::fs::write(
            directory.join(format!("sample-{i:03} 空 格\"quote\".txt")),
            format!("中🙂 NeEdLe-{i}\r\nDO NOT ATTACH"),
        )
        .unwrap();
    }
    let position = format!("{}中🙂 needle needle at line 1200", "line\n".repeat(1199));
    std::fs::write(env.workspace_path.join("position.txt"), &position).unwrap();
    std::fs::write(env.workspace_path.join("binary"), [0, 255, 0]).unwrap();
    std::fs::write(env.workspace_path.join("oversized"), vec![b'x'; 600_000]).unwrap();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(async move {
            let mut process = env.spawn_agent(&agent_bin);
            let mut app = App::new(env.workspace_path.clone());
            dispatch(&mut process, &mut app, AppEvent::Bootstrap)
                .await
                .unwrap();
            pump_until(&mut process, &mut app, |a| {
                a.connection == ConnectionState::Ready
            })
            .await
            .unwrap();
            let session =
                create_compact_session(&mut process, &mut app, &env.workspace_path, "Workspace E2")
                    .await;
            type_draft(&mut process, &mut app, "keep draft ")
                .await
                .unwrap();
            press_key(&mut process, &mut app, KeyCode::Char('@'))
                .await
                .unwrap();
            type_draft(&mut process, &mut app, "sample-").await.unwrap();
            browser_ready(&mut process, &mut app).await;
            let mut pages = 1;
            while app.workspace_browser().unwrap().cursor.is_some() {
                press_ctrl_key(&mut process, &mut app, 'n').await.unwrap();
                browser_ready(&mut process, &mut app).await;
                pages += 1;
                assert!(pages <= 4);
            }
            assert_eq!(pages, 3);
            let b = app.workspace_browser().unwrap();
            assert_eq!(b.files.len(), 230);
            assert!(b.files.windows(2).all(|w| w[0].path < w[1].path));
            // Paging sorts received candidates but preserves the highlighted
            // identity; newly discovered paths may sort before that selection.
            let selected = b.files[b.selected].path.clone();
            press_key(&mut process, &mut app, KeyCode::F(4))
                .await
                .unwrap();
            file_ready(&mut process, &mut app).await;
            assert!(
                app.file_preview()
                    .unwrap()
                    .content
                    .chunks
                    .iter()
                    .any(|s| s.contains("DO NOT ATTACH"))
            );
            assert_eq!(app.composer.content(), "keep draft @");
            press_key(&mut process, &mut app, KeyCode::Esc)
                .await
                .unwrap();
            press_key(&mut process, &mut app, KeyCode::Enter)
                .await
                .unwrap();
            assert_eq!(
                app.composer.content(),
                format!(
                    "keep draft {}",
                    minicore_tui::state::workspace::reference_token(&selected)
                )
            );
            assert!(!app.composer.content().contains("DO NOT ATTACH"));
            run_slash_command(&mut process, &mut app, "/grep needle")
                .await
                .unwrap();
            browser_ready(&mut process, &mut app).await;
            let mut pages = 1;
            let mut skipped = app.workspace_browser().unwrap().skipped;
            while app.workspace_browser().unwrap().cursor.is_some() {
                press_ctrl_key(&mut process, &mut app, 'n').await.unwrap();
                browser_ready(&mut process, &mut app).await;
                pages += 1;
                skipped += app.workspace_browser().unwrap().skipped;
                assert!(pages <= 4);
            }
            assert_eq!(pages, 3);
            let b = app.workspace_browser().unwrap();
            assert_eq!(b.matches.len(), 231);
            assert!(skipped >= 2);
            assert!(
                b.matches
                    .iter()
                    .all(|m| m.valid_ranges() && m.match_byte_ranges[0].start == 8)
            );
            let identities: std::collections::HashSet<_> = b
                .matches
                .iter()
                .map(|m| (&m.path, m.line_number, m.line_text_byte_offset))
                .collect();
            assert_eq!(identities.len(), 231);
            let target = b
                .matches
                .iter()
                .position(|m| m.path == "position.txt")
                .unwrap();
            for _ in 0..target {
                press_key(&mut process, &mut app, KeyCode::Down)
                    .await
                    .unwrap();
            }
            press_key(&mut process, &mut app, KeyCode::Enter)
                .await
                .unwrap();
            file_ready(&mut process, &mut app).await;
            assert_eq!(app.file_preview().unwrap().content.bytes, position.len());
            layout_file(&mut app).await;
            let f = app.file_preview().unwrap();
            assert_eq!(
                f.layout.as_ref().unwrap().rows[f.offset].source.start_line,
                1200
            );
            press_key(&mut process, &mut app, KeyCode::Esc)
                .await
                .unwrap();
            // Scope and case are explicit Dock input, not shell/pathspec/regex commands.
            press_key(&mut process, &mut app, KeyCode::Tab)
                .await
                .unwrap();
            type_draft(&mut process, &mut app, "子 dir").await.unwrap();
            press_ctrl_key(&mut process, &mut app, 'i').await.unwrap();
            browser_ready(&mut process, &mut app).await;
            assert!(app.workspace_browser().unwrap().matches.is_empty());
            assert_eq!(app.sessions.known[&session].transcript.total, 0);
            assert!(
                env._server.recorded_requests().is_empty(),
                "workspace queries cannot call a Provider"
            );
            let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
            assert!(rep.shutdown_ok && rep.seen_eof && rep.seen_exit);
            process.terminate().await;
        });
}
#[test]
#[ignore = "requires fixed MINICORE_AGENT_BIN; raw UTF-8 paging and changed revision"]
fn e2e_workspace_preview_raw_paging_changed_binary_and_too_large() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    let text = format!("{}\r\nlast without newline", "中🙂".repeat(20000));
    std::fs::write(env.workspace_path.join("空 格 long.txt"), &text).unwrap();
    std::fs::write(env.workspace_path.join("changing.txt"), "a".repeat(120000)).unwrap();
    std::fs::write(env.workspace_path.join("binary"), [0, 255, 0]).unwrap();
    std::fs::write(env.workspace_path.join("oversized"), vec![b'x'; 600_000]).unwrap();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(async move {
            let mut process = env.spawn_agent(&agent_bin);
            let mut app = App::new(env.workspace_path.clone());
            dispatch(&mut process, &mut app, AppEvent::Bootstrap)
                .await
                .unwrap();
            pump_until(&mut process, &mut app, |a| {
                a.connection == ConnectionState::Ready
            })
            .await
            .unwrap();
            let session = create_compact_session(
                &mut process,
                &mut app,
                &env.workspace_path,
                "File preview E2",
            )
            .await;
            preview(&mut process, &mut app, "空 格 long.txt").await;
            assert_eq!(app.file_preview().unwrap().next.unwrap().start_line, 1);
            let revision = app.file_preview().unwrap().revision.clone();
            let mut pages = 1;
            while let Some(next) = app.file_preview().unwrap().next {
                press_ctrl_key(&mut process, &mut app, 'n').await.unwrap();
                file_ready(&mut process, &mut app).await;
                assert_eq!(app.file_preview().unwrap().requested, next);
                assert_eq!(app.file_preview().unwrap().revision, revision);
                pages += 1;
                assert!(pages <= 5);
            }
            assert!(pages >= 3);
            let raw: String = app
                .file_preview()
                .unwrap()
                .content
                .chunks
                .iter()
                .map(AsRef::as_ref)
                .collect();
            assert_eq!(raw, text);
            layout_file(&mut app).await;
            assert_eq!(
                &*app
                    .file_preview()
                    .unwrap()
                    .layout
                    .as_ref()
                    .unwrap()
                    .copy_text,
                text
            );
            preview(&mut process, &mut app, "changing.txt").await;
            let old = app.file_preview().unwrap().content.bytes;
            assert!(old < 120000);
            std::fs::write(env.workspace_path.join("changing.txt"), "NEW".repeat(40000)).unwrap();
            press_ctrl_key(&mut process, &mut app, 'n').await.unwrap();
            file_ready(&mut process, &mut app).await;
            let f = app.file_preview().unwrap();
            assert_eq!(f.status, Some(FileStatus::Changed));
            assert_eq!(f.content.bytes, old);
            assert!(f.content.chunks.iter().all(|s| !s.contains("NEW")));
            assert!(f.next.is_none());
            press_key(&mut process, &mut app, KeyCode::F(5))
                .await
                .unwrap();
            file_ready(&mut process, &mut app).await;
            assert_eq!(app.file_preview().unwrap().status, Some(FileStatus::Ok));
            assert!(app.file_preview().unwrap().content.chunks[0].starts_with("NEW"));
            for (path, status) in [
                ("binary", FileStatus::Binary),
                ("oversized", FileStatus::TooLarge),
            ] {
                preview(&mut process, &mut app, path).await;
                let f = app.file_preview().unwrap();
                assert_eq!(f.status, Some(status));
                assert_eq!(f.content.bytes, 0);
                assert!(f.next.is_none());
            }
            assert_eq!(app.sessions.known[&session].transcript.total, 0);
            assert!(env._server.recorded_requests().is_empty());
            let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
            assert!(rep.shutdown_ok && rep.seen_eof && rep.seen_exit);
            process.terminate().await;
        });
}
