use super::*;

mod body_recovery {
    use crate::app::session::{EARLY_BYTES, EARLY_TURNS};
    use crate::app::{LiveTurn, Session};
    use crate::protocol::{
        AgentEvent, TurnRef, Usage,
        read::{RawHistoryItem, TurnPersistence, UserKind},
    };
    use crate::transcript::{Block, append_history_item};

    fn session() -> Session {
        Session::new(
            serde_json::from_value(crate::testing::session_info("s")).unwrap(),
            1,
            crate::testing::LOAD.into(),
        )
    }

    fn delta(loop_id: &str, text: &str) -> AgentEvent {
        serde_json::from_value(serde_json::json!({"type":"output_delta","data":{
            "meta":{"session_id":"s","load":"load_1","dropped_before":0},
            "turn":{"session_id":"s","loop_id":loop_id},"request_index":0,
            "channel":"text","delta":text
        }}))
        .unwrap()
    }

    #[test]
    fn early_body_waits_for_its_turn_and_replays_in_first_seen_order() {
        let mut session = session();
        session.live = Some(LiveTurn::new(TurnRef::new("s", "A"), 0));
        session.record_region("A", 0);
        for (id, text) in [
            ("Z", "first "),
            ("B", "held "),
            ("C", "other "),
            ("Z", "answer"),
            ("B", "answer"),
            ("C", "answer"),
        ] {
            session.apply_body_event(delta(id, text), 128);
        }
        assert!(session.transcript.is_empty());
        assert_eq!(session.early_bytes, 6 * 128);
        session.record_region("Z", 0);
        session.record_region("C", 0);
        session.replay_early();
        assert_eq!(
            session
                .transcript
                .blocks()
                .iter()
                .filter_map(|block| match block {
                    Block::Assistant { key, text, .. } => {
                        Some((key.loop_id.as_str(), text.as_str()))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
            [("Z", "first answer"), ("C", "other answer")]
        );
        assert_eq!(session.early_bytes, 2 * 128);
        session.replay_early();
        assert_eq!(session.transcript.len(), 2);
        session.record_region("B", session.transcript.len());
        session.replay_early();
        assert!(
            matches!(&session.transcript.blocks()[2], Block::Assistant { key, text, .. } if key.loop_id == "B" && text == "held answer")
        );
        assert_eq!(session.early_bytes, 0);
    }

    #[test]
    fn unknown_body_overflow_is_bounded_and_marks_recovery_without_creating_turns() {
        let mut session = session();
        for index in 0..EARLY_TURNS {
            session.apply_body_event(delta(&format!("unknown-{index}"), "x"), 128);
        }
        let held = format!("unknown-{}", EARLY_TURNS - 1);
        session.apply_body_event(delta(&held, "more"), 128);
        assert!(!session.early_gap);
        assert_eq!(session.early_bytes, (EARLY_TURNS + 1) * 128);
        for index in EARLY_TURNS..EARLY_TURNS + 2 {
            session.apply_body_event(delta(&format!("unknown-{index}"), "x"), 128);
        }
        assert!(session.early_gap);
        assert!(session.recovery.behind);
        assert_eq!(session.early_bytes, (EARLY_TURNS + 1) * 128);
        assert!(session.transcript.is_empty());
        session.apply_body_event(delta(&held, "retained"), EARLY_BYTES);
        assert_eq!(session.early_bytes, EARLY_BYTES);
        session.apply_body_event(delta("huge", "x"), EARLY_BYTES + 1);
        assert_eq!(session.early_bytes, EARLY_BYTES);
        assert!(session.transcript.is_empty());
        session.record_region(&held, 0);
        session.replay_early();
        assert!(
            matches!(&session.transcript.blocks()[0], Block::Assistant { text, .. } if text == "retained")
        );
        assert_eq!(session.early_bytes, 0);
    }

    #[test]
    fn unsaved_a_saved_b_and_live_c_keep_order_body_and_draft_in_either_recovery_order() {
        use crate::protocol::{CompletedTurn, LoopOutcome};
        use serde_json::json;
        for order in [["A", "B"], ["B", "A"]] {
            let mut session = session();
            session.draft = Some("unfinished draft".into());
            let b: Vec<RawHistoryItem> = [
                json!({"type":"user","data":{"loop_id":"B","kind":"prompt","input":{"text":"prompt B"}}}),
                json!({"type":"assistant","data":{"loop_id":"B","request_index":0,"content":[{"type":"text","data":"answer B"}]}}),
            ].into_iter().map(|item| crate::protocol::read::decode_item(&json!({"item":item}).to_string()).unwrap()).collect();
            for item in &b {
                append_history_item(&mut session.transcript, item, "s");
            }
            session.transcript.push(Block::TurnEnd {
                loop_id: "B".into(),
                outcome: LoopOutcome::Completed,
                duration: None,
                requests: 1,
                tools: 0,
                usage: Usage::default(),
                changes: None,
                saving: None,
            });
            session.record_region("B", 0);
            let c = session.transcript.len();
            session.transcript.push(Block::User {
                text: "prompt C".into(),
                kind: UserKind::Prompt,
                at: None,
            });
            session.record_region("C", c);
            session.live = Some(LiveTurn::new(TurnRef::new("s", "C"), c));
            session.apply_body_event(delta("C", "new live text"), 256);
            let descriptions: Vec<CompletedTurn> = ["A", "B"].into_iter().enumerate().map(|(i, id)| serde_json::from_value(json!({
                "seq":i+1,"loop_id":id,"completion_kind":"reported","outcome_kind":"completed",
                "persistence":if id == "A" {"failed"} else {"persisted"},"availability":if id == "A" {"live"} else {"stored"}
            })).unwrap()).collect();
            session.recovery = crate::app::recovery::RecoveryState::default();
            session
                .recovery
                .discover(&crate::protocol::CompletionDirectory {
                    through_seq: 2,
                    retained_from_seq: 1,
                    entries: descriptions.clone(),
                    truncated: false,
                });
            for id in order {
                let description = &descriptions[usize::from(id == "B")];
                if id == "B" {
                    // Metadata alone does not claim coverage; the verified body uses its exact region.
                    session
                        .reconcile_completion(
                            description,
                            &crate::app::read::TurnDetails {
                                outcome: LoopOutcome::Completed,
                                usage: None,
                                requests: None,
                                tool_rounds: None,
                            },
                            &b,
                            None,
                            TurnPersistence::Persisted,
                            false,
                        )
                        .unwrap();
                } else {
                    let a: Vec<RawHistoryItem> = [
                        json!({"type":"user","data":{"loop_id":"A","kind":"prompt","input":{"text":"prompt A"}}}),
                        json!({"type":"assistant","data":{"loop_id":"A","request_index":0,"content":[{"type":"text","data":"answer A"}]}}),
                    ].into_iter().map(|item| crate::protocol::read::decode_item(&json!({"item":item}).to_string()).unwrap()).collect();
                    session
                        .reconcile_completion(
                            description,
                            &crate::app::read::TurnDetails {
                                outcome: LoopOutcome::Completed,
                                usage: None,
                                requests: None,
                                tool_rounds: None,
                            },
                            &a,
                            None,
                            TurnPersistence::Failed,
                            false,
                        )
                        .unwrap();
                }
            }
            let texts: Vec<&str> = session
                .transcript
                .blocks()
                .iter()
                .filter_map(|block| match block {
                    Block::User { text, .. } | Block::Assistant { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                texts,
                [
                    "prompt A",
                    "answer A",
                    "prompt B",
                    "answer B",
                    "prompt C",
                    "new live text"
                ],
                "order {order:?}"
            );
            assert_eq!(session.draft.as_deref(), Some("unfinished draft"));
            let ends: Vec<&str> = session
                .transcript
                .blocks()
                .iter()
                .filter_map(|block| match block {
                    Block::TurnEnd { loop_id, .. } => Some(loop_id.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                ends,
                ["A", "B"],
                "both recovered regions must keep their own terminal boundary: {order:?}"
            );
            assert!(session.transcript.blocks().iter().any(|block|matches!(block,Block::TurnEnd { loop_id,saving:Some(crate::transcript::Saving::Saved),.. } if loop_id == "B")));
            assert!(
                matches!(&session.transcript.blocks()[session.live.as_ref().unwrap().start], Block::User { text, .. } if text == "prompt C")
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn dropped_events_make_the_durable_items_replace_only_that_turn() {
    let mut h = Harness::opened(page(
        2,
        0,
        vec![
            user(0, "l0", "earlier question"),
            assistant(1, "l0", "earlier answer"),
        ],
    ))
    .await;
    let mut starting = running_snapshot(h.next_revision(), "l1", "$text");
    starting["execution"]["input"]["accepted_at"] = json!("2026-10-20T21:38:00Z");
    h.fake.script(
        "input.push",
        after_snapshot(started_at("l1", "2026-10-20T21:38:00Z"), starting).with_request_text(),
    );
    h.fake.script_held(
        "turn.wait",
        after_snapshot(
            waited("l1", completed()),
            finished_snapshot(3, vec![completion(1, "l1", "completed")]),
        ),
    );
    h.fake.script(
        "turn.result",
        turn_result(
            "l1",
            vec![
                user(2, "l1", "the prompt"),
                assistant(3, "l1", "the durable answer"),
            ],
        ),
    );
    h.submit("the prompt");
    h.until(|app| app.is_busy()).await;
    h.emit(
        "output_delta",
        json!({"turn": turn("l1"), "request_index": 0, "channel": "text", "delta": "half an ans",
               "meta": {"session_id": "s1", "dropped_before": 2}}),
    )
    .await;
    h.settle().await;
    h.fake.release("turn.wait");
    h.until(|app| !app.is_busy()).await;
    h.settle().await;

    assert_eq!(
        h.texts(),
        [
            "earlier question",
            "earlier answer",
            "the prompt",
            "the durable answer"
        ]
    );
    assert_eq!(turn_ends(&h), ["l1"]);
    assert_eq!(h.sent_at("the prompt"), unix_millis("2026-10-20T21:38:00Z"));
    assert!(matches!(h.blocks().last(), Some(Block::TurnEnd { .. })));
}

#[tokio::test(start_paused = true)]
async fn a_capacity_refusal_on_turn_wait_is_retried() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
    h.fake.script_error_then(
        "turn.wait",
        -32019,
        "no deferred slot",
        "resource_exhausted",
        completed_reply("l1", 3, vec![user(0, "l1", "retry the wait")]),
    );
    h.submit("retry the wait");
    h.settle().await;
    assert_eq!(h.calls("turn.wait").len(), 2);
    assert_eq!(turn_ends(&h), ["l1"]);
    assert!(h.notices().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_lost_wait_recovers_the_stored_result_after_completion_is_observed() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
    h.fake
        .script_error("turn.wait", -32007, "turn not found", "turn_not_found");
    h.fake.script(
        "turn.result",
        after_snapshot(json!({"turn": turn("l1"), "availability": "stored", "persistence": "persisted",
               "load": crate::testing::LOAD, "result_id": RESULT_ID,
               "total": 0, "items": [], "next_cursor": null, "outcome": completed(), "requests": 3}),
            finished_snapshot(3,vec![completion(1,"l1","completed")])) ,
    );
    h.submit("go");
    h.settle().await;
    assert!(!h.is_busy());
    assert_eq!(turn_ends(&h), ["l1"]);
}

#[tokio::test(start_paused = true)]
async fn a_wait_read_failure_cannot_override_a_running_snapshot() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
    h.fake
        .script_error("turn.wait", -32007, "turn not found", "turn_not_found");
    h.fake
        .script_error("turn.result", -32007, "turn not found", "turn_not_found");
    h.submit("go");
    h.settle().await;
    assert!(
        h.is_busy(),
        "only an authoritative snapshot can end execution"
    );
    assert!(
        turn_ends(&h).is_empty(),
        "read failure is not a terminal outcome"
    );
    h.control_event(json!({"execution":{"type":"none"}})).await;
    h.until(|app| !app.is_busy()).await;
}

#[tokio::test(start_paused = true)]
async fn an_oversized_turn_read_keeps_the_live_region_and_reports_the_missing_item() {
    use crate::app::read::{ReadPartial, TurnRead};

    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
    h.fake.script_held(
        "turn.wait",
        after_snapshot(
            waited("l1", cancelled()),
            finished_snapshot(
                h.next_revision() + 1,
                vec![completion(1, "l1", "cancelled")],
            ),
        ),
    );
    h.fake
        .script_held("turn.result", turn_result("l1", vec![user(0, "l1", "go")]));
    h.submit("go");
    h.until(|app| app.is_busy()).await;
    h.emit(
        "output_delta",
        json!({"turn":turn("l1"), "request_index":0,
        "channel":"text", "delta":"already shown"}),
    )
    .await;
    h.until(|app| {
        app.transcript_blocks().iter().any(|block| {
            matches!(block,
        Block::Assistant { text, .. } if text == "already shown")
        })
    })
    .await;
    h.fake.release("turn.wait");
    h.until(|app| {
        app.resident.current().is_some_and(|session| {
            session
                .recovery
                .entries
                .values()
                .any(|entry| entry.turn.loop_id == "l1")
        })
    })
    .await;
    let body = |h: &Harness| {
        format!(
            "{:?}",
            h.blocks()
                .iter()
                .filter(|block| !matches!(block, Block::TurnEnd { .. }))
                .collect::<Vec<_>>()
        )
    };
    let before = body(&h);
    let session = h.session();
    let seq = *session.recovery.entries.first_key_value().unwrap().0;

    h.op(SessionOp::Recovered(
        crate::app::recovery::RecoveryOutcome {
            seq,

            turn: crate::protocol::TurnRef::new("s1", "l1"),
            result: Ok(TurnRead::Partial(Box::new(ReadPartial::Oversized {
                index: 1,
                total_bytes: 8_388_609,
            }))),
        },
    ));
    assert_eq!(
        body(&h),
        before,
        "the received live body is preserved byte for byte"
    );
    assert_eq!(
        turn_ends(&h),
        ["l1"],
        "a partial result has one persistent terminal, not an empty canonical answer"
    );
    assert!(!h.session().is_closed("l1"));
    assert_eq!(
        h.session().body_recovery[0].status,
        crate::app::recovery::RecoveryStatus::Partial
    );
    assert_eq!(h.session().reconcile_from.len(), 1);
    assert!(
        h.notices()
            .iter()
            .any(|notice| notice.contains("Item 1 is too large"))
    );
}

#[tokio::test(start_paused = true)]
async fn a_successor_started_before_the_finished_turn_reconciles_stays_after_it() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script_error("tool.read", -32021, "tool not found", "tool_not_found");
    let next = entry("i2", "follow_up", "next");
    h.fake.script_sequence(
        "input.push",
        vec![
            starting_reply("l1", 2),
            after_snapshot(
                pushed(vec![next.clone()]),
                running_queue(3, "l1", "start", vec![next]),
            ),
        ],
    );
    let mut successor = finished_snapshot(4, vec![completion(1, "l1", "completed")]);
    successor["execution"] = running_snapshot(4, "l2", "next")["execution"].clone();
    h.fake.script_held(
        "turn.wait@s1/l1",
        after_snapshot(
            waited_then(
                "l1",
                completed(),
                Some(next_turn("l2", "next")),
                queue(vec![], false),
            ),
            successor,
        ),
    );
    h.fake.script_held(
        "turn.wait@s1/l2",
        after_snapshot(
            waited("l2", completed()),
            finished_snapshot(
                5,
                vec![
                    completion(1, "l1", "completed"),
                    completion(2, "l2", "completed"),
                ],
            ),
        ),
    );
    h.fake.script(
        "turn.result@s1/l2",
        completed_result("l2", vec![user(0, "l2", "next")]),
    );
    h.fake.script_held(
        "turn.result@s1/l1",
        turn_result(
            "l1",
            vec![
                user(0, "l1", "start"),
                assistant_tool(1, "l1", "c1", "bash"),
                assistant(2, "l1", "partial answer"),
            ],
        ),
    );
    h.submit("start");
    h.until(|app| app.is_busy()).await;
    // A gap in the events makes the durable items replace what streamed.
    h.emit(
        "request_started",
        json!({"turn": turn("l1"), "request_index": 0, "model": "mock",
               "meta": {"session_id": "s1", "dropped_before": 2}}),
    )
    .await;
    h.app.composer.set_text("next");
    h.key(KeyCode::Enter, KeyModifiers::ALT);
    h.until(|app| app.resident.current().unwrap().queue.items.len() == 1)
        .await;
    h.fake.release("turn.wait@s1/l1");
    // The successor is observed while the stored items are still held.
    h.until(|app| app.transcript_blocks().len() >= 3).await;
    h.run_for(Duration::from_secs(3)).await;
    assert_eq!(h.sends(), ["start", "next"]);

    h.fake.release("turn.result@s1/l1");
    h.run_for(Duration::from_secs(2)).await;
    assert_eq!(h.texts(), ["start", "partial answer", "next"]);
    let shape: Vec<&str> = h
        .blocks()
        .iter()
        .map(|block| match block {
            Block::User { .. } => "user",
            Block::Assistant { .. } => "assistant",
            Block::Tool(_) => "tool",
            Block::TurnEnd { .. } => "end",
            _ => "other",
        })
        .collect();
    assert_eq!(shape, ["user", "tool", "assistant", "end", "user"]);
    let live = h.session().live.as_ref().unwrap();
    assert_eq!(live.start, 4, "the running turn still starts at its prompt");

    h.fake.release("turn.wait@s1/l2");
    h.settle().await;
    assert_eq!(turn_ends(&h), ["l1", "l2"]);
    assert!(matches!(h.blocks().last(), Some(Block::TurnEnd { .. })));
}

#[tokio::test(start_paused = true)]
async fn an_interrupted_turn_keeps_its_card_details_and_unfinished_thinking() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script_error("tool.read", -32021, "tool not found", "tool_not_found");
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
    h.fake.script_held(
        "turn.wait",
        after_snapshot(
            waited("l1", cancelled()),
            finished_snapshot(
                h.next_revision() + 1,
                vec![completion(1, "l1", "cancelled")],
            ),
        ),
    );
    h.fake.script(
        "turn.result",
        turn_result(
            "l1",
            vec![
                user(0, "l1", "go"),
                assistant_tool(1, "l1", "c1", "bash"),
                assistant_tool(2, "l1", "c2", "bash"),
            ],
        ),
    );
    h.submit("go");
    h.until(|app| app.is_busy()).await;
    for (id, display) in [
        (
            "c1",
            json!({"detail": "$ sleep 90", "expanded_input": "sleep 90"}),
        ),
        (
            "c2",
            json!({"detail": "", "expanded_input": "echo hi\nsecond line"}),
        ),
    ] {
        h.emit(
            "tool_presentation",
            json!({"turn": turn("l1"), "request_index": 0, "tool_call_id": id,
                   "tool_name": "bash", "display": display}),
        )
        .await;
    }
    h.emit(
        "output_delta",
        json!({"turn": turn("l1"), "request_index": 1, "channel": "reasoning",
               "delta": "Implementing strict import validation"}),
    )
    .await;
    h.settle().await;
    h.fake.release("turn.wait");
    h.settle().await;

    let cards: Vec<(String, Option<String>)> = h
        .session()
        .transcript
        .tool_calls()
        .map(|call| (call.detail.clone(), call.expanded_input.clone()))
        .collect();
    assert_eq!(
        cards,
        [
            ("$ sleep 90".into(), Some("sleep 90".into())),
            ("$ echo hi".into(), Some("echo hi\nsecond line".into())),
        ]
    );
    let thinking = h.blocks().iter().any(|block| {
        matches!(block, Block::Assistant { reasoning, streaming: false, .. }
            if reasoning == "Implementing strict import validation")
    });
    assert!(thinking, "{:?}", h.blocks());
    assert!(matches!(h.blocks().last(), Some(Block::TurnEnd { .. })));
}

#[tokio::test(start_paused = true)]
async fn the_interrupted_reply_of_a_cancelled_turn_replaces_its_streamed_block_in_place() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script_error("tool.read", -32021, "tool not found", "tool_not_found");
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
    h.fake.script_held(
        "turn.wait",
        after_snapshot(
            waited("l1", cancelled()),
            finished_snapshot(
                h.next_revision() + 1,
                vec![completion(1, "l1", "cancelled")],
            ),
        ),
    );
    let mut result = turn_result(
        "l1",
        vec![user(0, "l1", "go"), assistant_tool(1, "l1", "c1", "bash")],
    );
    result["interrupted"] = json!({"stop_reason": "aborted", "request_index": 1, "model": "mock",
                                   "reasoning": "", "text": "I will start by readi"});
    h.fake.script("turn.result", result);
    h.submit("go");
    h.until(|app| app.is_busy()).await;
    h.emit(
        "output_delta",
        json!({"turn": turn("l1"), "request_index": 1, "channel": "text", "delta": "I will st"}),
    )
    .await;
    h.settle().await;
    h.fake.release("turn.wait");
    h.settle().await;

    let shape: Vec<&str> = h
        .blocks()
        .iter()
        .map(|block| match block {
            Block::User { .. } => "user",
            Block::Assistant { .. } => "assistant",
            Block::Tool(_) => "tool",
            Block::TurnEnd { .. } => "end",
            _ => "other",
        })
        .collect();
    assert_eq!(shape, ["user", "tool", "assistant", "end"]);
    assert_eq!(h.texts(), ["go", "I will start by readi"]);
}

#[tokio::test(start_paused = true)]
async fn a_completion_whose_region_was_evicted_does_not_close_the_turn() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
    h.fake.script_held(
        "turn.wait",
        after_snapshot(
            waited("l1", cancelled()),
            finished_snapshot(
                h.next_revision() + 1,
                vec![completion(1, "l1", "cancelled")],
            ),
        ),
    );
    h.fake
        .script_held("turn.result", turn_result("l1", vec![user(0, "l1", "go")]));
    h.submit("go");
    h.until(|app| app.is_busy()).await;
    h.emit(
        "output_delta",
        json!({"turn":turn("l1"), "request_index":1, "channel":"text", "delta":"streamed"}),
    )
    .await;
    h.until(|app| {
        app.transcript_blocks()
            .iter()
            .any(|block| matches!(block, Block::Assistant { text, .. } if text == "streamed"))
    })
    .await;
    h.fake.release("turn.wait");
    // The bounded region list drops old turns, so by the time this completion
    // arrives its streamed region is no longer recorded.
    h.until(|app| {
        app.resident
            .current()
            .is_some_and(|session| !session.reconcile_from.is_empty())
    })
    .await;
    h.session_mut().reconcile_from.clear();
    h.fake.release("turn.result");
    h.settle().await;

    let session = h.session();
    assert!(
        !session.is_closed("l1"),
        "a completion that installed nothing must leave the turn repairable"
    );
    assert!(
        session
            .transcript
            .blocks()
            .iter()
            .any(|block| matches!(block, Block::TurnEnd { saving: None, .. })),
        "and must not state a saving fact it did not establish: {:?}",
        h.blocks()
    );
}

#[tokio::test(start_paused = true)]
async fn catalog_eviction_does_not_release_a_recovery_slot_before_its_read_finishes() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_held("turn.result", turn_result("l1", vec![]));
    let directory = |start: u64| json!({"through_seq":start+255,"retained_from_seq":start,"truncated":start>1,"entries":(start..start+256).map(|seq|json!({"seq":seq,"loop_id":format!("l{seq}"),"completion_kind":"reported","outcome_kind":"completed","persistence":"persisted","availability":"stored"})).collect::<Vec<_>>()});
    h.control_event(json!({"completions":directory(1)})).await;
    h.until(|app| app.resident.current().unwrap().recovery_tasks.len() == 2)
        .await;
    h.fake.wait_for_calls("turn.result", 2).await;
    h.control_event(json!({"completions":directory(257)})).await;
    h.settle().await;
    let session = h.session();
    assert_eq!(session.recovery_tasks.len(), 2);
    assert_eq!(h.calls("turn.result").len(), 2);
    assert_eq!(session.recovery.observed_through, 512);
    assert_eq!(session.recovery.settled_through, 0);
    assert_eq!(session.recovery.entries.len(), 256);
    assert!(session.recovery.truncated_gap);
    h.session_mut().recovery.retry_later(257);
    tokio::time::advance(Duration::from_secs(1)).await;
    h.app.notices.clear();
    h.app.flash = None;
    h.app.scrolled_at = None;
    assert!(
        h.app.next_expiry().is_none(),
        "an overdue retry cannot busy-poll while both reads still own their slots"
    );
}

#[tokio::test(start_paused = true)]
async fn pending_recovery_exhausts_without_settling_or_losing_body_and_unrelated_revisions_do_not_restart_it()
 {
    use crate::app::recovery::RecoveryStatus;
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.running_state("l1", entry("i1", "steer", "question"));
    h.emit(
        "output_delta",
        json!({"turn":turn("l1"),"request_index":0,"channel":"text","delta":"already seen"}),
    )
    .await;
    h.settle().await;
    h.fake.script("turn.result",json!({"turn":turn("l1"),"load":crate::testing::LOAD,"availability":"pending","total":0,"items":[],"next_cursor":null}));
    let completions = json!({"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":"l1","completion_kind":"reported","outcome_kind":"completed","persistence":"failed","availability":"live"}]});
    h.control_event(json!({"execution":{"type":"none"},"completions":completions}))
        .await;
    h.settle().await;
    for delay in [
        Duration::from_millis(250),
        Duration::from_secs(1),
        Duration::from_secs(4),
    ] {
        tokio::time::advance(delay).await;
        h.app.tick();
        h.settle().await;
    }
    assert_eq!(h.calls("turn.result").len(), 4);
    let session = h.session();
    assert_eq!(
        session.recovery.entries[&1].status,
        RecoveryStatus::Exhausted
    );
    assert_eq!(session.recovery.observed_through, 1);
    assert_eq!(session.recovery.settled_through, 0);
    assert!(!session.is_closed("l1"));
    assert_eq!(h.texts(), ["question", "already seen"]);
    assert_eq!(session.body_recovery[0].status, RecoveryStatus::Exhausted);
    h.control_event(json!({"execution":{"type":"none"},"completions":completions}))
        .await;
    h.settle().await;
    h.app.tick();
    h.settle().await;
    assert_eq!(
        h.calls("turn.result").len(),
        4,
        "same availability at a fresh revision must not restart retries"
    );
    h.fake.script("turn.result",json!({"turn":turn("l1"),"load":crate::testing::LOAD,"availability":"unavailable","unavailable_reason":"expired","persistence":"failed","total":0,"items":[],"next_cursor":null}));
    h.action(Action::RetryRecovery);
    h.settle().await;
    let session = h.session();
    assert_eq!(session.recovery.settled_through, 1);
    assert_eq!(session.body_recovery[0].status, RecoveryStatus::Unavailable);
    assert!(session.body_recovery[0].message.contains("not saved"));
    assert_eq!(h.texts(), ["question", "already seen"]);
    assert!(
        !session.is_closed("l1"),
        "explicit unavailability preserves seen content but is not canonical installation"
    );
}

#[tokio::test(start_paused = true)]
async fn a_stale_load_body_cannot_evict_the_installed_loads_early_cache() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.emit(
        "output_delta",
        json!({"turn":turn("unknown"),"request_index":0,"channel":"text","delta":"kept"}),
    )
    .await;
    h.settle().await;
    let before = h.session().early_bytes;
    h.emit_no_meta("output_delta",json!({"meta":{"session_id":"s1","load":"closed-load","dropped_before":0},"turn":turn("stale"),"request_index":0,"channel":"text","delta":"x".repeat(9*1024*1024)})).await;
    h.settle().await;
    let session = h.session();
    assert_eq!(session.early_bytes, before);
    assert!(!session.early_gap);
    assert!(session.transcript.is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_unseen_completion_inserted_before_live_content_rebases_the_actual_view_anchor() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_held("turn.wait", waited("C", completed()));
    let input = entry("iC", "steer", "prompt C");
    h.running_state("C", input.clone());
    h.emit(
        "output_delta",
        json!({"turn":turn("C"),"request_index":0,"channel":"text","delta":"live C"}),
    )
    .await;
    h.until(|app| {
        app.transcript_blocks()
            .iter()
            .any(|block| matches!(block,Block::Assistant { text,.. } if text == "live C"))
    })
    .await;
    h.app.scroll = Scroll::Anchor {
        block: 1,
        offset: 1,
    };
    h.app.composer.set_text("keep the draft");
    let result = completed_result(
        "A",
        vec![user(0, "A", "prompt A"), assistant(1, "A", "answer A")],
    );
    h.fake.script("turn.result", result);
    h.control_event(json!({"execution":{"type":"running","loop_id":"C","input":input,"request_index":0,"phase":"running_model"},"completions":{"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":"A","completion_kind":"reported","outcome_kind":"completed","persistence":"persisted","availability":"stored"}]}})).await;
    h.until(|app| app.resident.current().unwrap().is_closed("A"))
        .await;
    assert_eq!(h.texts(), ["prompt A", "answer A", "prompt C", "live C"]);
    assert_eq!(h.session().live.as_ref().unwrap().start, 3);
    assert_eq!(
        h.app.scroll,
        Scroll::Anchor {
            block: 4,
            offset: 1
        },
        "a zero-width insertion still moves the anchored old content"
    );
    assert_eq!(h.app.composer.text(), "keep the draft");
}

#[tokio::test(start_paused = true)]
async fn a_saved_anchor_after_a_recovered_unsaved_turn_controls_unseen_completion_order() {
    let mut history = page(
        4,
        0,
        vec![
            user(0, "S", "saved S"),
            assistant(1, "S", "answer S"),
            user(2, "B", "saved B"),
            assistant(3, "B", "answer B"),
        ],
    );
    history["records"] =
        json!([{"loop_id":"S","outcome":completed()},{"loop_id":"B","outcome":completed()}]);
    let mut h = Harness::opened(history).await;
    let result = |id: &str, text: &str| {
        let mut page = completed_result(
            id,
            vec![user(0, id, &format!("prompt {id}")), assistant(1, id, text)],
        );
        page["persistence"] = json!(if id == "B" { "persisted" } else { "failed" });
        page
    };
    h.fake
        .script_sequence("turn.result", vec![result("A", "answer A")]);
    h.fake.script_held("turn.result", result("B", "answer B"));
    h.control_event(json!({"completions":{"through_seq":3,"retained_from_seq":1,"truncated":false,"entries":[
        {"seq":1,"loop_id":"A","completion_kind":"reported","outcome_kind":"completed","persistence":"failed","availability":"live","after_saved_loop_id":"S"},
        {"seq":2,"loop_id":"B","completion_kind":"reported","outcome_kind":"completed","persistence":"persisted","availability":"stored","after_saved_loop_id":"S"},
        {"seq":3,"loop_id":"C","completion_kind":"reported","outcome_kind":"completed","persistence":"failed","availability":"live","after_saved_loop_id":"B"}
    ]}})).await;
    h.until(|app| app.resident.current().unwrap().recovery_tasks.len() == 2)
        .await;
    h.fake.wait_for_calls("turn.result", 2).await;
    h.fake
        .script_sequence("turn.result", vec![result("C", "answer C")]);
    h.until(|app| app.resident.current().unwrap().is_closed("C"))
        .await;
    assert!(!h.session().is_closed("B"), "B's reader is still held");
    assert_eq!(
        turn_ends(&h),
        ["S", "A", "B", "C"],
        "the saved B anchor is stronger than the earlier reconciled A"
    );
    h.fake.release("turn.result");
    h.until(|app| app.resident.current().unwrap().is_closed("B"))
        .await;
    assert_eq!(turn_ends(&h), ["S", "A", "B", "C"]);
}

#[tokio::test(start_paused = true)]
async fn unseen_unavailable_completions_keep_terminal_identity_reason_and_saving_visible() {
    for (id, kind, outcome, persistence, reason, expected) in [
        (
            "internal",
            "internal_error",
            serde_json::Value::Null,
            serde_json::Value::Null,
            "internal_error",
            "internal",
        ),
        (
            "unsaved",
            "reported",
            json!("completed"),
            json!("failed"),
            "expired",
            "not saved",
        ),
        (
            "large",
            "reported",
            json!("failed"),
            json!("failed"),
            "too_large",
            "too large",
        ),
    ] {
        let mut h = Harness::opened(display_read(0, 0)).await;
        #[cfg(feature = "e2e-observe")]
        let trace = tempfile::NamedTempFile::new().unwrap();
        #[cfg(feature = "e2e-observe")]
        h.app.start_observer(trace.reopen().unwrap());
        h.fake.script("turn.result",json!({"turn":turn(id),"load":crate::testing::LOAD,"availability":"unavailable","unavailable_reason":reason,"persistence":persistence,"items":[],"total":0,"next_cursor":null}));
        h.control_event(json!({"completions":{"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":id,"completion_kind":kind,"outcome_kind":outcome,"persistence":persistence,"availability":"unavailable","unavailable_reason":reason}]}})).await;
        h.until(|app| app.resident.current().unwrap().recovery.settled_through == 1)
            .await;
        h.app.notices.clear();
        let screen = h.screen(120, 30).to_lowercase();
        assert!(
            screen.contains(id),
            "persistent terminal identity is missing: {screen}"
        );
        assert!(
            screen.contains(expected),
            "persistent reason/saving is missing: {screen}"
        );
        assert!(
            screen.contains("unavailable"),
            "body unavailability is not visible: {screen}"
        );
        if persistence.is_null() {
            assert!(
                screen.contains("save unknown"),
                "an unknown save must not look saved: {screen}"
            );
            assert!(
                screen.contains("unknown outcome"),
                "an absent report must not imply a completed model outcome: {screen}"
            );
        }
        assert!(
            h.blocks()
                .iter()
                .any(|block| matches!(block,Block::TurnEnd { loop_id,.. } if loop_id == id))
        );
        assert!(
            !h.session().is_closed(id),
            "a terminal placeholder is not a canonical empty result"
        );
        #[cfg(feature = "e2e-observe")]
        {
            let rows: Vec<Value> = std::fs::read_to_string(trace.path())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let status = rows
                .iter()
                .find(|row| row["event"] == "recovery_status")
                .unwrap();
            assert_eq!(status["loop_id"], id);
            assert_eq!(status["status"], "unavailable");
            assert_eq!(status["canonical"], false);
            assert!(status["terminal"]["block_index"].is_u64());
            assert!(!rows.iter().any(|row| row["event"] == "canonical_install"));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn an_unseen_partial_completion_has_a_persistent_noncanonical_terminal() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    #[cfg(feature = "e2e-observe")]
    let trace = tempfile::NamedTempFile::new().unwrap();
    #[cfg(feature = "e2e-observe")]
    h.app.start_observer(trace.reopen().unwrap());
    let directory:crate::protocol::CompletionDirectory = serde_json::from_value(json!({"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":"partial-loop","completion_kind":"reported","outcome_kind":"completed","persistence":"failed","availability":"live"}]})).unwrap();
    let session = h.session_mut();
    session.catalog = directory.clone();
    session.recovery.discover(&directory);

    h.op(SessionOp::Recovered(
        crate::app::recovery::RecoveryOutcome {
            seq: 1,
            turn: crate::protocol::TurnRef::new("s1", "partial-loop"),

            result: Ok(crate::app::read::TurnRead::Partial(Box::new(
                crate::app::read::ReadPartial::Oversized {
                    index: 0,
                    total_bytes: 8_388_609,
                },
            ))),
        },
    ));
    h.app.notices.clear();
    let screen = h.screen(120, 30).to_lowercase();
    for text in ["partial-loop", "partial", "not saved", "too large"] {
        assert!(
            screen.contains(text),
            "{text} is missing from persistent UI: {screen}"
        );
    }
    assert!(!h.session().is_closed("partial-loop"));
    #[cfg(feature = "e2e-observe")]
    {
        let rows: Vec<Value> = std::fs::read_to_string(trace.path())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let status = rows
            .iter()
            .find(|row| row["event"] == "recovery_status")
            .unwrap();
        assert_eq!(status["status"], "partial");
        assert_eq!(status["terminal"]["saving"], "not saved");
        assert_eq!(status["canonical"], false);
        assert!(!rows.iter().any(|row| row["event"] == "canonical_install"));
    }
}

#[tokio::test(start_paused = true)]
async fn unavailable_regions_keep_sequence_order_and_late_received_body_before_their_terminal() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let response = |id| json!({"turn":turn(id),"load":crate::testing::LOAD,"availability":"unavailable","unavailable_reason":"expired","persistence":"failed","items":[],"total":0,"next_cursor":null});
    h.fake
        .script_sequence("turn.result", vec![response("A"), response("B")]);
    h.control_event(json!({"completions":{"through_seq":2,"retained_from_seq":1,"truncated":false,"entries":[
        {"seq":1,"loop_id":"A","completion_kind":"reported","outcome_kind":"completed","persistence":"failed","availability":"unavailable"},
        {"seq":2,"loop_id":"B","completion_kind":"reported","outcome_kind":"failed","persistence":"failed","availability":"unavailable"}
    ]}})).await;
    h.until(|app| app.resident.current().unwrap().recovery.settled_through == 2)
        .await;
    h.emit(
        "output_delta",
        json!({"turn":turn("A"),"request_index":0,"channel":"text","delta":"late received A"}),
    )
    .await;
    h.until(|app| {
        app.transcript_blocks()
            .iter()
            .any(|block| matches!(block,Block::Assistant { text,.. } if text == "late received A"))
    })
    .await;
    let order: Vec<_> = h
        .blocks()
        .iter()
        .filter_map(|block| block.loop_id())
        .collect();
    assert_eq!(order, ["A", "A", "B"]);
    assert_eq!(turn_ends(&h), ["A", "B"]);
    assert!(!h.session().is_closed("A"));
}

#[tokio::test(start_paused = true)]
async fn a_partial_history_region_keeps_late_body_before_its_existing_terminal() {
    let mut history = page(
        2,
        0,
        vec![user(0, "A", "prompt A"), assistant(1, "A", "tail A")],
    );
    history["records"] = json!([{"loop_id":"A","outcome":completed()}]);
    let mut h = Harness::opened(history).await;
    let directory = serde_json::from_value(json!({"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[
        {"seq":1,"loop_id":"A","completion_kind":"reported","outcome_kind":"completed","persistence":"persisted","availability":"stored"}
    ]})).unwrap();
    let session = h.session_mut();
    session.catalog = directory;
    session.recovery.discover(&session.catalog);

    h.op(SessionOp::Recovered(
        crate::app::recovery::RecoveryOutcome {
            seq: 1,

            turn: crate::protocol::TurnRef::new("s1", "A"),
            result: Ok(crate::app::read::TurnRead::Partial(Box::new(
                crate::app::read::ReadPartial::WorkspaceLimit,
            ))),
        },
    ));
    h.emit(
        "output_delta",
        json!({"turn":turn("A"),"request_index":1,"channel":"text","delta":"late retained A"}),
    )
    .await;
    h.until(|app|app.transcript_blocks().iter().any(|block|matches!(block,Block::Assistant {key,text,..} if key.request_index == 1 && text == "late retained A"))).await;
    assert!(
        matches!(h.blocks().last(),Some(Block::TurnEnd {loop_id,saving:Some(crate::transcript::Saving::Saved),..}) if loop_id == "A")
    );
    assert_eq!(h.texts(), ["prompt A", "tail A", "late retained A"]);
    assert!(!h.session().is_closed("A"));
}

#[tokio::test(start_paused = true)]
async fn a_normal_completed_turn_keeps_its_late_tail_until_canonical_repair_then_rejects_duplicates()
 {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_held("turn.wait", waited("A", completed()));
    h.fake
        .script_held("session.state@s1", json!({"session_id":"s1"}));
    h.running_state("A", entry("iA", "steer", "prompt A"));
    h.emit(
        "output_delta",
        json!({"turn":turn("A"),"request_index":0,"channel":"text","delta":"prefix"}),
    )
    .await;
    h.until(|app| {
        app.transcript_blocks()
            .iter()
            .any(|block| matches!(block,Block::Assistant{text,..} if text=="prefix"))
    })
    .await;
    h.op(SessionOp::Waited(Ok(serde_json::from_value(waited(
        "A",
        completed(),
    ))
    .unwrap())));
    h.emit(
        "output_delta",
        json!({"turn":turn("A"),"request_index":0,"channel":"text","delta":" tail"}),
    )
    .await;
    h.until(|app| {
        app.transcript_blocks()
            .iter()
            .any(|block| matches!(block,Block::Assistant{text,..} if text=="prefix tail"))
    })
    .await;
    let canonical = completed_result(
        "A",
        vec![
            user(0, "A", "prompt A"),
            assistant(1, "A", "prefix tail repaired"),
        ],
    );
    h.fake.script_held("turn.result", canonical);
    h.control_event(json!({"completions":{"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":"A","input_id":"iA","completion_kind":"reported","outcome_kind":"completed","persistence":"persisted","availability":"stored"}]}})).await;
    h.settle().await;
    h.fake.wait_for_calls("turn.result", 1).await;
    assert!(!h.session().is_closed("A"));
    assert_eq!(h.texts(), ["prompt A", "prefix tail"]);
    h.fake.release("turn.result");
    h.until(|app| app.resident.current().unwrap().is_closed("A"))
        .await;
    assert_eq!(h.texts(), ["prompt A", "prefix tail repaired"]);
    let before = format!("{:?}", h.blocks());
    for (request, channel, text) in [
        (0, "text", " tail"),
        (0, "reasoning", "late thinking"),
        (99, "text", "late new request"),
    ] {
        h.emit(
            "output_delta",
            json!({"turn":turn("A"),"request_index":request,"channel":channel,"delta":text}),
        )
        .await;
    }
    h.settle().await;
    assert_eq!(
        format!("{:?}", h.blocks()),
        before,
        "canonical installation closes every body channel, not just the last streamed key"
    );
    assert_eq!(turn_ends(&h), ["A"]);
    assert_eq!(h.session().recovery.settled_through, 1);
    assert_eq!(
        h.calls("turn.result").len(),
        1,
        "normal no-gap completion still reconciles exactly once"
    );
}

#[tokio::test(start_paused = true)]
async fn a_tool_record_saved_after_canonical_install_updates_only_its_auxiliary_facts() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let result = turn_result(
        "A",
        vec![
            user(0, "A", "prompt A"),
            assistant_tool(1, "A", "call-A", "bash"),
            assistant(2, "A", "answer A"),
        ],
    );
    h.fake.script("turn.result", result);
    h.fake.script("tool.read",json!({"execution":{"tool_ref":{"session_id":"s1","loop_id":"A","request_index":0,"tool_call_id":"call-A"},"state":"succeeded","recording":"memory_only","result_truncated":false}}));
    h.control_event(json!({"completions":{"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":"A","completion_kind":"reported","outcome_kind":"completed","persistence":"persisted","availability":"stored"}]}})).await;
    h.until(|app| app.resident.current().unwrap().is_closed("A"))
        .await;
    let card = || crate::protocol::ToolRef {
        session_id: "s1".into(),
        loop_id: "A".into(),
        request_index: 0,
        tool_call_id: "call-A".into(),
    };
    assert_eq!(
        h.session().transcript.find_tool(&card()).unwrap().recording,
        Some(crate::protocol::ToolRecordingState::MemoryOnly)
    );
    let before = h.texts();
    let reads = h.calls("tool.read").len();
    let result_reads = h.calls("turn.result").len();
    for (loop_id, call_id, load) in [
        ("other-loop", "call-A", crate::testing::LOAD),
        ("A", "other-call", crate::testing::LOAD),
        ("A", "call-A", "old-load"),
    ] {
        h.emit_no_meta("tool_execution",json!({"meta":{"session_id":"s1","load":load,"dropped_before":0},"turn":turn(loop_id),"data":{"tool_ref":{"session_id":"s1","loop_id":loop_id,"request_index":0,"tool_call_id":call_id},"state":"succeeded","recording":"saved","result_truncated":false}})).await;
    }
    h.settle().await;
    assert_eq!(
        h.session().transcript.find_tool(&card()).unwrap().recording,
        Some(crate::protocol::ToolRecordingState::MemoryOnly)
    );
    // The Agent publishes its auxiliary commit as ToolExecution(recording=saved).
    h.emit("tool_execution",json!({"turn":turn("A"),"data":{"tool_ref":{"session_id":"s1","loop_id":"A","request_index":0,"tool_call_id":"call-A"},"state":"succeeded","recording":"saved","finished_at":"2026-10-31T00:00:01Z","started_at":"2026-10-31T00:00:00Z","output_line_count":7,"result_truncated":false}})).await;
    h.settle().await;
    let call = h.session().transcript.find_tool(&card()).unwrap();
    assert_eq!(
        call.recording,
        Some(crate::protocol::ToolRecordingState::Saved),
        "the late auxiliary save event must update the already closed card"
    );
    assert_eq!(call.output_lines, Some(7));
    assert_eq!(call.duration, Some(Duration::from_secs(1)));
    assert_eq!(call.state, ToolState::Succeeded);
    assert_eq!(h.texts(), before);
    assert_eq!(
        h.blocks()
            .iter()
            .filter(|block| matches!(block, Block::Tool(_)))
            .count(),
        1
    );
    assert_eq!(turn_ends(&h), ["A"]);
    assert_eq!(
        h.calls("tool.read").len(),
        reads,
        "the actual late commit event, not query backfill, updates the card"
    );
    assert_eq!(h.calls("turn.result").len(), result_reads);
    assert!(h.session().is_closed("A"));
}

#[cfg(feature = "e2e-observe")]
#[tokio::test(start_paused = true)]
async fn canonical_replacement_keeps_identified_equal_steers_and_reports_the_actual_verified_read_count()
 {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let trace = tempfile::NamedTempFile::new().unwrap();
    h.app.start_observer(trace.reopen().unwrap());
    h.fake.script_held("turn.wait", waited("A", completed()));
    h.running_state("A", entry("input-A", "steer", "prompt A"));
    h.emit(
        "output_delta",
        json!({"turn":turn("A"),"request_index":0,"channel":"text","delta":"body A0"}),
    )
    .await;
    h.emit(
        "tool_started",
        json!({"turn":turn("A"),"request_index":0,"tool_call_id":"call-A","tool_name":"bash"}),
    )
    .await;
    h.emit("input_delivered",json!({"turn":turn("A"),"items":[{"id":"same-1","request_index":1,"text":"same"},{"id":"same-2","request_index":1,"text":"same"}]})).await;
    h.emit(
        "output_delta",
        json!({"turn":turn("A"),"request_index":1,"channel":"text","delta":"body A1"}),
    )
    .await;
    h.settle().await;
    let result = completed_result(
        "A",
        vec![
            user(0, "A", "prompt A"),
            chunk(
                1,
                json!({"type":"assistant","data":{"loop_id":"A","request_index":0,"content":[{"type":"text","data":"body A0"},{"type":"tool_call","data":{"tool_call_id":"call-A","name":"bash"}}]}}),
            ),
            chunk(
                2,
                json!({"type":"tool_result","data":{"loop_id":"A","request_index":0,"call_id":"call-A","outcome":"success","output":{"content":"result"}}}),
            ),
            chunk(
                3,
                json!({"type":"user","data":{"loop_id":"A","kind":"steering","input":{"text":"same"}}}),
            ),
            chunk(
                4,
                json!({"type":"user","data":{"loop_id":"A","kind":"steering","input":{"text":"same"}}}),
            ),
            chunk(
                5,
                json!({"type":"assistant","data":{"loop_id":"A","request_index":1,"content":[{"type":"text","data":"body A1"}]}}),
            ),
        ],
    );
    h.fake.script("turn.result", result);
    h.control_event(json!({"completions":{"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":"A","input_id":"input-A","completion_kind":"reported","outcome_kind":"completed","persistence":"persisted","availability":"stored"}]}})).await;
    h.until(|app| app.resident.current().unwrap().is_closed("A"))
        .await;
    h.app.observe_checkpoint(false);
    let rows: Vec<Value> = std::fs::read_to_string(trace.path())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let installed = rows
        .iter()
        .find(|row| row["event"] == "canonical_install")
        .unwrap();
    assert_eq!(installed["read_item_count"], 6);
    assert_eq!(
        installed["parts"].as_array().unwrap().len(),
        3,
        "the read count is not inferred from visible parts"
    );
    let view = rows
        .iter()
        .rev()
        .find(|row| row["event"] == "canonical_view")
        .unwrap();
    let steers: Vec<_> = view["inventory"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["user_kind"] == "steering")
        .map(|row| (row["input_id"].as_str(), row["request_index"].as_u64()))
        .collect();
    assert_eq!(
        steers,
        [(Some("same-1"), Some(1)), (Some("same-2"), Some(1))],
        "canonical repair must retain the original identified source rows at their proven boundaries"
    );
    assert_eq!(view["delivered_inputs"].as_array().unwrap().len(), 2);
    let first_delivery = rows
        .iter()
        .find(|row| {
            row["event"] == "canonical_view"
                && row["delivered_inputs"]
                    .as_array()
                    .is_some_and(|inputs| inputs.len() == 2)
        })
        .unwrap();
    assert!(
        first_delivery["event_seq"].as_u64().unwrap() < installed["event_seq"].as_u64().unwrap(),
        "the source delivery was actually observed before canonical repair"
    );
}

#[tokio::test(start_paused = true)]
async fn a_late_wait_fallback_reads_the_original_load_instead_of_unbound_history() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_error(
        "turn.wait",
        -32013,
        "wait result is outside the retained window",
        "turn_not_found",
    );
    let result = completed_result(
        "A",
        vec![user(0, "A", "prompt A"), assistant(1, "A", "answer A")],
    );
    h.fake.script("turn.result", result);
    h.running_state("A", entry("input-A", "steer", "prompt A"));
    h.until(|app| {
        app.transcript_blocks()
            .iter()
            .any(|block| matches!(block,Block::TurnEnd{loop_id,..} if loop_id=="A"))
    })
    .await;
    let calls = h.calls("turn.result");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["loop_id"], "A");
    assert_eq!(
        calls[0]["load"],
        crate::testing::LOAD,
        "the resident wait fallback must retain its original load identity"
    );
}
