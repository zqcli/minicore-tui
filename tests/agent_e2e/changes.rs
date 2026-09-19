use super::*;
use minicore_tui::protocol::changes::{
    ChangeKind, ChangeOrigin, ChangeScope, Comparison, DiffAvailability,
};
// Git writes exist only in this disposable synthetic-repository test harness.
fn git(root: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
async fn ready(process: &mut RpcProcess, app: &mut App) {
    pump_until(process, app, |a| {
        a.changes().is_some()
            && !a.pending_requests.values().any(|r| {
                matches!(
                    r,
                    RequestKind::Changes { .. } | RequestKind::WorkspaceStatus { .. }
                )
            })
    })
    .await
    .unwrap();
    let s = app.changes().unwrap();
    assert!(s.error.is_none(), "{:?}", s.error);
    if s.in_diff {
        assert!(
            s.detail.as_ref().unwrap().error.is_none(),
            "{:?}",
            s.detail.as_ref().unwrap().error
        );
    }
}
async fn open(process: &mut RpcProcess, app: &mut App, scope: ChangeScope) {
    let commands = app.open_changes(scope);
    dispatch_commands(process, app, commands).await.unwrap();
    ready(process, app).await;
}
async fn select(process: &mut RpcProcess, app: &mut App, path: &str) {
    let target = app
        .changes()
        .unwrap()
        .records
        .iter()
        .position(|r| r.path == path)
        .unwrap();
    while app.changes().unwrap().selected != target {
        let code = if app.changes().unwrap().selected < target {
            KeyCode::Down
        } else {
            KeyCode::Up
        };
        press_key(process, app, code).await.unwrap();
    }
    press_key(process, app, KeyCode::Enter).await.unwrap();
    ready(process, app).await;
}
async fn layout(app: &mut App) {
    let mut jobs = minicore_tui::jobs::LocalJobs::new();
    let request = app.diff_layout_request(63).unwrap();
    app.mark_diff_layout_pending(request.identity.clone());
    assert!(jobs.try_schedule_diff_layout(request));
    app.update(
        tokio::time::timeout(TIMEOUT, jobs.events().recv())
            .await
            .unwrap()
            .unwrap(),
    );
    assert!(
        app.changes()
            .unwrap()
            .detail
            .as_ref()
            .unwrap()
            .layout
            .is_some()
    );
    jobs.shutdown().await;
}
#[test]
#[ignore = "requires fixed Agent; disposable Git repository, no Provider calls"]
fn e2e_changes_workspace_scopes_status_comparisons_fragments_and_stale() {
    let agent = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(async move {
            let mut process = env.spawn_agent(&agent);
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
                create_compact_session(&mut process, &mut app, &env.workspace_path, "Changes E3")
                    .await;
            pump_until(&mut process, &mut app, |a| {
                a.active_view().unwrap().workspace_status.value.is_some()
            })
            .await
            .unwrap();
            assert!(
                !app.active_view()
                    .unwrap()
                    .workspace_status
                    .value
                    .as_ref()
                    .unwrap()
                    .repo_available
            );
            let root = &env.workspace_path;
            git(root, &["init", "-b", "main"]);
            git(root, &["config", "user.email", "synthetic@example.invalid"]);
            git(root, &["config", "user.name", "Synthetic E3"]);
            open(&mut process, &mut app, ChangeScope::Workspace).await;
            assert!(
                app.active_view()
                    .unwrap()
                    .workspace_status
                    .value
                    .as_ref()
                    .unwrap()
                    .head_oid
                    .is_none()
            );
            for path in [
                "staged.txt",
                "mixed.txt",
                "delete.txt",
                "rename.txt",
                "conflict.txt",
            ] {
                std::fs::write(root.join(path), "before\r\n").unwrap();
            }
            std::fs::write(root.join("long.txt"), "a".repeat(140_000)).unwrap();
            std::fs::write(root.join("binary.bin"), [0, 1, 2]).unwrap();
            git(root, &["add", "."]);
            git(root, &["commit", "-m", "base"]);
            git(root, &["checkout", "-b", "other"]);
            std::fs::write(root.join("conflict.txt"), "other\n").unwrap();
            git(root, &["commit", "-am", "other"]);
            git(root, &["checkout", "main"]);
            std::fs::write(root.join("conflict.txt"), "main\n").unwrap();
            git(root, &["commit", "-am", "main"]);
            assert!(
                !std::process::Command::new("git")
                    .args(["merge", "other"])
                    .current_dir(root)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
            std::fs::write(root.join("staged.txt"), "index\n").unwrap();
            git(root, &["add", "staged.txt"]);
            std::fs::write(root.join("mixed.txt"), "worktree\r\n").unwrap();
            git(root, &["rm", "delete.txt"]);
            git(root, &["mv", "rename.txt", "renamed 空 格\".txt"]);
            std::fs::write(root.join("binary.bin"), [0, 9, 8]).unwrap();
            let target = format!("{}\r\n", "中🙂".repeat(20_000));
            std::fs::write(root.join("long.txt"), &target).unwrap();
            for i in 0..230 {
                std::fs::write(root.join(format!("untracked-{i:03}")), "new\n").unwrap();
            }
            open(&mut process, &mut app, ChangeScope::Workspace).await;
            assert!(app.changes().unwrap().cursor.is_some());
            let old_count = app.changes().unwrap().records.len();
            std::fs::write(root.join("new-observation"), "changed status").unwrap();
            press_ctrl_key(&mut process, &mut app, 'n').await.unwrap();
            pump_until(&mut process, &mut app, |a| {
                !a.pending_requests
                    .values()
                    .any(|r| matches!(r, RequestKind::Changes { .. }))
            })
            .await
            .unwrap();
            assert!(
                app.changes()
                    .unwrap()
                    .error
                    .as_ref()
                    .unwrap()
                    .contains("stale")
            );
            assert_eq!(app.changes().unwrap().records.len(), old_count);
            press_key(&mut process, &mut app, KeyCode::F(5))
                .await
                .unwrap();
            ready(&mut process, &mut app).await;
            let mut pages = 1;
            while app.changes().unwrap().cursor.is_some() {
                press_ctrl_key(&mut process, &mut app, 'n').await.unwrap();
                ready(&mut process, &mut app).await;
                pages += 1;
                assert!(pages < 12);
            }
            assert!(pages >= 3);
            let records = &app.changes().unwrap().records;
            assert!(records.len() > 230);
            assert!(
                records
                    .iter()
                    .all(|r| r.origin == ChangeOrigin::WorkspaceUnknown)
            );
            for kind in [
                ChangeKind::Deleted,
                ChangeKind::Renamed,
                ChangeKind::Conflict,
            ] {
                assert!(records.iter().any(|r| r.kind == kind), "missing {kind:?}");
            }
            select(&mut process, &mut app, "staged.txt").await;
            for comparison in [
                Comparison::HeadToWorktree,
                Comparison::HeadToIndex,
                Comparison::IndexToWorktree,
            ] {
                assert_eq!(
                    app.changes()
                        .unwrap()
                        .detail
                        .as_ref()
                        .unwrap()
                        .meta
                        .as_ref()
                        .unwrap()
                        .comparison,
                    comparison
                );
                if comparison != Comparison::IndexToWorktree {
                    press_key(&mut process, &mut app, KeyCode::Tab)
                        .await
                        .unwrap();
                    ready(&mut process, &mut app).await;
                }
            }
            press_key(&mut process, &mut app, KeyCode::Esc)
                .await
                .unwrap();
            select(&mut process, &mut app, "long.txt").await;
            let d = app.changes().unwrap().detail.as_ref().unwrap();
            assert!(d.buffer.partial_line());
            assert!(d.cursor.is_some());
            let mut reads = 1;
            while app
                .changes()
                .unwrap()
                .detail
                .as_ref()
                .unwrap()
                .cursor
                .is_some()
            {
                press_ctrl_key(&mut process, &mut app, 'n').await.unwrap();
                ready(&mut process, &mut app).await;
                reads += 1;
                assert!(reads < 15);
            }
            let d = app.changes().unwrap().detail.as_ref().unwrap();
            assert!(!d.buffer.partial_line());
            assert!(d.meta.as_ref().unwrap().complete);
            assert_eq!(d.buffer.bytes, 140_000 + target.len());
            layout(&mut app).await;
            assert_eq!(
                &*app
                    .changes()
                    .unwrap()
                    .detail
                    .as_ref()
                    .unwrap()
                    .layout
                    .as_ref()
                    .unwrap()
                    .copy_text,
                format!("{}{}", "a".repeat(140_000), target)
            );
            press_key(&mut process, &mut app, KeyCode::F(5))
                .await
                .unwrap();
            ready(&mut process, &mut app).await;
            let old = app.changes().unwrap().detail.as_ref().unwrap().buffer.bytes;
            std::fs::write(root.join("long.txt"), "changed during paging\n").unwrap();
            press_ctrl_key(&mut process, &mut app, 'n').await.unwrap();
            ready(&mut process, &mut app).await;
            assert!(app.changes().unwrap().detail.as_ref().unwrap().stale);
            assert_eq!(
                app.changes().unwrap().detail.as_ref().unwrap().buffer.bytes,
                old
            );
            press_key(&mut process, &mut app, KeyCode::Esc)
                .await
                .unwrap();
            select(&mut process, &mut app, "binary.bin").await;
            assert_eq!(
                app.changes()
                    .unwrap()
                    .detail
                    .as_ref()
                    .unwrap()
                    .meta
                    .as_ref()
                    .unwrap()
                    .availability,
                DiffAvailability::Binary
            );
            git(root, &["add", "conflict.txt"]);
            git(root, &["commit", "-m", "resolve synthetic conflict"]);
            git(root, &["checkout", "--detach", "HEAD"]);
            open(&mut process, &mut app, ChangeScope::Workspace).await;
            assert!(
                app.active_view()
                    .unwrap()
                    .workspace_status
                    .value
                    .as_ref()
                    .unwrap()
                    .detached
            );
            assert!(env._server.recorded_requests().is_empty());
            assert_eq!(app.sessions.known[&session].transcript.total, 0);
            assert!(
                drain_shutdown_strict(&mut process, &mut app)
                    .await
                    .unwrap()
                    .shutdown_ok
            );
            process.terminate().await;
        });
}
#[test]
#[ignore = "requires fixed Agent; native-write provenance excludes Bash and external edits"]
fn e2e_changes_native_records_remain_independent_and_turn_scoped() {
    let agent = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    let mut config: toml::Value =
        toml::from_str(&std::fs::read_to_string(&env.config_path).unwrap()).unwrap();
    config["profiles"]["coding"]["tools"] = toml::Value::Array(
        ["write", "bash"]
            .map(|s| toml::Value::String(s.into()))
            .to_vec(),
    );
    std::fs::write(&env.config_path, toml::to_string(&config).unwrap()).unwrap();
    for (id, content) in [
        ("write-first", "first\r\n"),
        ("write-second", "second no final newline"),
    ] {
        env._server.enqueue_sse(sse_tool_call_response(
            id,
            "write",
            &json!({"path":"native.txt","content":content}).to_string(),
        ));
    }
    env._server.enqueue_sse(sse_tool_call_response(
        "shell-change",
        "bash",
        &json!({"command":"printf shell > shell.txt"}).to_string(),
    ));
    env._server.enqueue_sse(sse_text_response("done"));
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(async move {
            let mut process = env.spawn_agent(&agent);
            let mut app = App::new(env.workspace_path.clone());
            dispatch(&mut process, &mut app, AppEvent::Bootstrap)
                .await
                .unwrap();
            pump_until(&mut process, &mut app, |a| {
                a.connection == ConnectionState::Ready
            })
            .await
            .unwrap();
            let session = create_additional_session(
                &mut process,
                &mut app,
                &env.workspace_path,
                "Native Changes",
            )
            .await;
            dispatch(
                &mut process,
                &mut app,
                AppEvent::SubmitTurn {
                    session_id: session.clone(),
                    text: "synthetic writes".into(),
                },
            )
            .await
            .unwrap();
            pump_until_with_decode(&mut process, &mut app, |a| {
                a.active_view().is_some_and(|v| {
                    v.live.is_none() && v.transcript.complete && v.last_result.is_some()
                })
            })
            .await
            .unwrap();
            assert_eq!(
                std::fs::read_to_string(env.workspace_path.join("native.txt")).unwrap(),
                "second no final newline"
            );
            assert!(env.workspace_path.join("shell.txt").exists());
            std::fs::write(env.workspace_path.join("external.txt"), "user edit").unwrap();
            let calls = env._server.recorded_requests().len();
            let turn = app
                .active_view()
                .unwrap()
                .last_result
                .as_ref()
                .unwrap()
                .turn
                .loop_id
                .clone();
            app.composer_mut().set_text("keep draft");
            open(&mut process, &mut app, ChangeScope::Session).await;
            let records = &app.changes().unwrap().records;
            assert_eq!(records.len(), 2);
            assert!(
                records
                    .iter()
                    .all(|r| r.path == "native.txt" && r.origin == ChangeOrigin::Tool)
            );
            assert_ne!(records[0].change_ref, records[1].change_ref);
            assert_ne!(records[0].tool_ref, records[1].tool_ref);
            let second = app
                .changes()
                .unwrap()
                .records
                .iter()
                .position(|r| {
                    r.tool_ref
                        .as_ref()
                        .is_some_and(|t| t.tool_call_id == "write-second")
                })
                .unwrap();
            for _ in 0..second {
                press_key(&mut process, &mut app, KeyCode::Down)
                    .await
                    .unwrap();
            }
            press_key(&mut process, &mut app, KeyCode::Enter)
                .await
                .unwrap();
            ready(&mut process, &mut app).await;
            let d = app.changes().unwrap().detail.as_ref().unwrap();
            assert_eq!(d.comparison, Comparison::ToolBeforeAfter);
            assert_eq!(
                d.meta.as_ref().unwrap().availability,
                DiffAvailability::Available
            );
            layout(&mut app).await;
            let text = &app
                .changes()
                .unwrap()
                .detail
                .as_ref()
                .unwrap()
                .layout
                .as_ref()
                .unwrap()
                .copy_text;
            assert!(text.contains("first\r\n"));
            assert!(text.ends_with("second no final newline"));
            open(&mut process, &mut app, ChangeScope::Turn { loop_id: turn }).await;
            assert_eq!(app.changes().unwrap().records.len(), 2);
            press_key(&mut process, &mut app, KeyCode::Esc)
                .await
                .unwrap();
            assert_eq!(app.composer().content(), "keep draft");
            let history_count = app.active_view().unwrap().transcript.total;
            let commands = app.open_context();
            dispatch_commands(&mut process, &mut app, commands)
                .await
                .unwrap();
            pump_until(&mut process, &mut app, |a| {
                a.active_view().unwrap().context.is_some()
                    && !a
                        .pending_requests
                        .values()
                        .any(|r| matches!(r, RequestKind::SessionContext { .. }))
            })
            .await
            .unwrap();
            assert!(app.context_panel().is_some());
            assert!(
                app.active_view()
                    .unwrap()
                    .context
                    .as_ref()
                    .unwrap()
                    .current_operation
                    .is_none()
            );
            assert_eq!(app.active_view().unwrap().transcript.total, history_count);
            press_key(&mut process, &mut app, KeyCode::Esc)
                .await
                .unwrap();
            assert_eq!(app.composer().content(), "keep draft");
            assert_eq!(env._server.recorded_requests().len(), calls);
            assert!(
                drain_shutdown_strict(&mut process, &mut app)
                    .await
                    .unwrap()
                    .shutdown_ok
            );
            process.terminate().await;
        });
}
