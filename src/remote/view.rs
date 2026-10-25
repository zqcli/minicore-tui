//! Semantic state and revision-indexed transcript updates for one phone.

use std::path::Path;

use serde_json::{Value, json};

use super::fs;

use crate::app::session::context_percent;
use crate::app::{App, InteractionKind, LiveStatus, QueuedKind};
use crate::protocol::read::UserKind;
use crate::transcript::{Block, RequestKey, ToolState};

#[derive(Default)]
pub struct View {
    connection: Option<u64>,
    epoch: Option<u64>,
    revisions: Vec<u64>,
    state: String,
    streaming: Vec<Option<Streaming>>,
    catalog_generation: Option<(u64, u64)>,
    catalog: Value,
}

struct Streaming {
    key: RequestKey,
    text: usize,
    reasoning: usize,
}

impl Streaming {
    fn of(block: &Block) -> Option<Self> {
        match block {
            Block::Assistant {
                key,
                text,
                reasoning,
                streaming: true,
                ..
            } => Some(Self {
                key: key.clone(),
                text: text.len(),
                reasoning: reasoning.len(),
            }),
            _ => None,
        }
    }

    fn append(&self, block: &Block) -> Option<Value> {
        let Block::Assistant {
            key,
            text,
            reasoning,
            streaming: true,
            ..
        } = block
        else {
            return None;
        };
        if *key != self.key {
            return None;
        }
        // Live deltas only append; durable replacements always have streaming=false.
        let text = text.get(self.text..)?;
        let reasoning = reasoning.get(self.reasoning..)?;
        let mut append = json!({});
        if !text.is_empty() {
            append["text"] = json!(text);
        }
        if !reasoning.is_empty() {
            append["reasoning"] = json!(reasoning);
        }
        Some(append)
    }
}

impl View {
    pub fn sync(&mut self, app: &App) -> Vec<Value> {
        let Some(phone) = &app.remote.connection else {
            return Vec::new();
        };
        let epoch = app.session.as_ref().map(|session| session.epoch);
        let revisions = app
            .session
            .as_ref()
            .map_or(&[][..], |session| session.transcript.revisions());
        let generation = (app.catalog_generation, app.remote.generation);
        if self.catalog_generation != Some(generation) {
            self.catalog = catalog(app);
            self.catalog_generation = Some(generation);
        }
        let state = state(app, &self.catalog);
        let serialized = state.to_string();
        let mut messages = Vec::new();
        if self.connection != Some(phone.id) || self.epoch != epoch {
            messages.push(json!({"t":"snapshot", "state":state, "blocks":app.transcript_blocks().iter().map(block).collect::<Vec<_>>()}));
            self.streaming = app.transcript_blocks().iter().map(Streaming::of).collect();
        } else {
            self.streaming.resize_with(revisions.len(), || None);
            let mut upserts = Vec::new();
            for (i, revision) in revisions.iter().enumerate() {
                if self.revisions.get(i) == Some(revision) {
                    continue;
                }
                let current = &app.transcript_blocks()[i];
                let append = self.streaming[i]
                    .as_ref()
                    .and_then(|sent| sent.append(current));
                match append {
                    Some(append) if append.as_object().unwrap().is_empty() => {}
                    Some(append) => upserts.push(json!({"i":i, "append":append})),
                    None => upserts.push(json!({"i":i, "block":block(current)})),
                }
                self.streaming[i] = Streaming::of(current);
            }
            if !upserts.is_empty() || revisions.len() != self.revisions.len() {
                messages.push(json!({"t":"blocks", "len":revisions.len(), "upserts":upserts}));
            }
            if serialized != self.state {
                messages.push(json!({"t":"state", "state":state}));
            }
        }
        self.connection = Some(phone.id);
        self.epoch = epoch;
        self.revisions.clear();
        self.revisions.extend_from_slice(revisions);
        self.state = serialized;
        messages
    }
}

fn catalog(app: &App) -> Value {
    let mut sessions: Vec<_> = app
        .sessions
        .iter()
        .filter(|info| fs::within(app.remote.roots(), Path::new(&info.workspace)))
        .collect();
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let sessions: Vec<_> = sessions
        .into_iter()
        .take(50)
        .map(|info| {
            json!({
                "id":info.session_id, "title":crate::app::overlay::session_label(info, None),
                "workspace":fs::display(Path::new(&info.workspace)), "updated_at":info.updated_at,
            })
        })
        .collect();
    json!({"sessions":sessions,
        "models":app.models.iter().map(|model| json!({"id":model.id, "reasoning":model.supported_reasoning})).collect::<Vec<_>>(),
        "profiles":app.profiles.iter().map(|profile| &profile.id).collect::<Vec<_>>()})
}

fn state(app: &App, catalog: &Value) -> Value {
    let session = app.session.as_ref();
    let live = session.and_then(|s| s.live.as_ref());
    let current = session.map(|s| s.info.session_id.as_str());
    let mut sessions = catalog["sessions"].clone();
    for info in sessions.as_array_mut().unwrap() {
        let resident = info["id"].as_str() == current;
        info["current"] = json!(resident);
        // Live marks change with every turn, so they stay out of the cached catalog.
        match info["id"].as_str().and_then(|id| app.live(id)) {
            Some(live) => {
                info["live"] = json!(true);
                info["status"] = json!(match live.live_status() {
                    LiveStatus::Idle => "idle",
                    LiveStatus::Running => "running",
                    LiveStatus::Approval => "approval",
                    LiveStatus::Compacting => "compacting",
                });
                info["started_ms"] = json!(live.live.as_ref().map(|turn| turn.started_ms));
                info["unseen"] = json!(live.unseen);
            }
            None => info["live"] = json!(false),
        }
        if let Some(session) = session.filter(|_| resident) {
            info["title"] = json!(crate::app::overlay::session_label(
                &session.info,
                Some(session)
            ));
        }
    }
    let interaction = session.and_then(|s| s.interaction.as_ref().map(|interaction| {
        let call = s.transcript.find_tool_by_call_id(&interaction.tool_call_id);
        json!({"id":interaction.interaction_id,
            "kind":match interaction.kind { InteractionKind::Approval => "approval", InteractionKind::Choice(_) => "choice" },
            "tool":interaction.tool_name, "detail":call.map_or("", |call| call.detail.as_str()),
            "input":call.and_then(|call| call.expanded_input.as_deref()),
            "prompt":interaction.prompt, "options":interaction.options()})
    }));
    let queue: Vec<_> = session.into_iter().flat_map(|s| &s.queue).map(|queued| json!({
        "kind":match queued.kind { QueuedKind::Steer => "steer", QueuedKind::Follow => "follow" }, "text":queued.text,
    })).collect();
    let (running, approval) = app.background_counts();
    json!({
        "session":session.map(|s| json!({"id":s.info.session_id, "title":crate::app::overlay::session_label(&s.info, session), "workspace":fs::display(Path::new(&s.info.workspace)), "profile":s.info.profile, "model":s.info.model, "reasoning":s.info.reasoning})),
        "draft":{"workspace":fs::display(&app.workspace), "profile":app.launch.profile.as_ref().or(app.default_profile.as_ref()), "model":app.model_id(), "reasoning":app.reasoning()},
        "status":{"label":app.status_label(), "busy":app.is_busy(), "started_ms":live.map(|live| live.started_ms), "interrupting":live.is_some_and(|live| live.interrupting), "compacting":app.compacting(), "loading":app.loading_session},
        "usage":{"input":session.map_or(0, |s| s.usage.input), "output":session.map_or(0, |s| s.usage.output), "cached":session.map_or(0, |s| s.usage.cached)},
        "context_percent":session.and_then(context_percent), "interaction":interaction, "queue":queue,
        "older":session.is_some_and(|s| s.loaded_from > 0), "sessions":sessions,
        "background":{"running":running, "approval":approval},
        "models":catalog["models"], "profiles":catalog["profiles"]
    })
}

pub fn block(block: &Block) -> Value {
    match block {
        Block::User { text, kind, at } => {
            json!({"type":"user", "text":text, "kind":match kind { UserKind::Prompt => "prompt", UserKind::Steering => "steering", UserKind::Unknown => "unknown" }, "at":at})
        }
        Block::Assistant {
            key,
            reasoning,
            text,
            streaming,
            ..
        } => {
            json!({"type":"assistant", "loop_id":key.loop_id, "request":key.request_index, "reasoning":reasoning, "text":text, "streaming":streaming})
        }
        Block::Tool(call) => {
            json!({"type":"tool", "call_id":call.key.tool_call_id, "name":call.name, "detail":call.detail, "input":call.expanded_input, "input_lines":call.input_lines,
            "state":match call.state { ToolState::Pending => "pending", ToolState::Running => "running", ToolState::Succeeded => "succeeded", ToolState::Failed => "failed", ToolState::Denied => "denied", ToolState::Cancelled => "cancelled" },
            "exit_code":call.exit_code, "duration_ms":call.duration.map(|d| d.as_millis() as u64), "output":call.output, "output_dropped":call.output_dropped, "output_lines":call.output_lines, "truncated":call.truncated})
        }
        Block::TurnEnd {
            loop_id,
            outcome,
            duration,
            requests,
            tools,
            usage,
            changes,
        } => {
            json!({"type":"turn_end", "loop_id":loop_id, "outcome":json!(outcome)["type"], "duration_ms":duration.map(|d| d.as_millis() as u64), "requests":requests, "tools":tools,
            "usage":{"input":usage.input_tokens.unwrap_or(0), "output":usage.output_tokens.unwrap_or(0), "cached":usage.cache_read_tokens.unwrap_or(0)},
            "changes":changes.map(|changes| json!({"files":changes.files, "added":changes.lines.map(|l| l.0), "removed":changes.lines.map(|l| l.1)}))})
        }
        Block::Compaction {
            before,
            after,
            summary,
            ..
        } => json!({"type":"compaction", "before":before, "after":after, "summary":summary}),
        Block::EarlierHistory { hidden_items } => {
            json!({"type":"earlier", "hidden_items":hidden_items})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::{Connection, auth::Device};
    use tokio::sync::{mpsc, watch};

    fn connected_app() -> (crate::testing::FakeAgent, App) {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (fake, mut app) = crate::testing::FakeAgent::start(tx);
        app.remote.config = Some(crate::config::Remote {
            bind: "127.0.0.1:0".parse().unwrap(),
            url: None,
            roots: vec![app.workspace.clone()],
            allow_insecure: false,
        });
        let (outbound, _output) = mpsc::channel(512);
        let (close, _closing) = watch::channel(None);
        app.remote.connection = Some(Connection {
            id: 1,
            generation: 0,
            device: Device {
                name: "phone".into(),
                token_sha256: "hash".into(),
                paired_at: String::new(),
            },
            outbound,
            close,
        });
        app.session = Some(crate::app::Session::new(
            serde_json::from_value(crate::testing::session_info("s1")).unwrap(),
            1,
        ));
        (fake, app)
    }

    #[tokio::test(start_paused = true)]
    async fn draft_settings_resolve_profile_defaults_and_explicit_overrides_without_clock_churn() {
        let (_fake, mut app) = connected_app();
        app.session = None;
        app.profiles = serde_json::from_value(json!([
            {"id":"coding", "model":"main", "reasoning":"medium"},
            {"id":"ask", "model":"quick", "reasoning":"low"}
        ]))
        .unwrap();
        app.default_profile = Some("coding".into());
        let mut view = View::default();
        let snapshot = view.sync(&app);
        let draft = &snapshot[0]["state"]["draft"];
        assert_eq!(draft["profile"], "coding");
        assert_eq!(draft["model"], "main");
        assert_eq!(draft["reasoning"], "medium");
        assert!(snapshot[0]["state"].get("now_ms").is_none());
        assert!(snapshot[0]["state"]["status"].get("now_ms").is_none());
        tokio::time::advance(std::time::Duration::from_secs(96)).await;
        assert!(view.sync(&app).is_empty());

        app.launch.profile = Some("ask".into());
        let changed = view.sync(&app);
        assert_eq!(changed[0]["t"], "state");
        assert_eq!(changed[0]["state"]["draft"]["profile"], "ask");
        assert_eq!(changed[0]["state"]["draft"]["model"], "quick");
        assert_eq!(changed[0]["state"]["draft"]["reasoning"], "low");

        app.launch.model = Some("chosen".into());
        app.launch.reasoning = Some(crate::protocol::Reasoning::High);
        let changed = view.sync(&app);
        assert_eq!(changed[0]["state"]["draft"]["model"], "chosen");
        assert_eq!(changed[0]["state"]["draft"]["reasoning"], "high");
        assert_eq!(changed[0]["state"]["draft"]["profile"], "ask");

        app.launch = crate::app::LaunchSettings::default();
        app.default_profile = None;
        let changed = view.sync(&app);
        for field in ["profile", "model", "reasoning"] {
            assert!(changed[0]["state"]["draft"][field].is_null());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn streaming_sends_only_suffixes_then_one_full_finished_block() {
        let (_fake, mut app) = connected_app();
        let key = RequestKey {
            loop_id: "loop".into(),
            request_index: 0,
        };
        let session = app.session.as_mut().unwrap();
        session.live = Some(crate::app::LiveTurn::new(
            crate::protocol::TurnRef::new("s1", "loop"),
            0,
        ));
        session.transcript.push(Block::Assistant {
            key: key.clone(),
            text: "hello".into(),
            reasoning: "考".into(),
            streaming: true,
            expanded: None,
        });
        let started = session.live.as_ref().unwrap().started_ms;
        let mut view = View::default();
        assert_eq!(view.sync(&app)[0]["t"], "snapshot");
        tokio::time::advance(std::time::Duration::from_secs(60)).await;
        assert!(view.sync(&app).is_empty());
        assert_eq!(
            app.session
                .as_ref()
                .unwrap()
                .live
                .as_ref()
                .unwrap()
                .started_ms,
            started
        );
        if let Block::Assistant { text, .. } =
            app.session.as_mut().unwrap().transcript.assistant_mut(&key)
        {
            text.push_str(" world");
        }
        assert_eq!(
            view.sync(&app),
            [json!({"t":"blocks", "len":1, "upserts":[{"i":0, "append":{"text":" world"}}]})]
        );
        if let Block::Assistant { reasoning, .. } =
            app.session.as_mut().unwrap().transcript.assistant_mut(&key)
        {
            reasoning.push('え');
        }
        assert_eq!(
            view.sync(&app)[0]["upserts"][0]["append"],
            json!({"reasoning":"え"})
        );
        if let Some(Block::Assistant { streaming, .. }) =
            app.session.as_mut().unwrap().transcript.block_mut(0)
        {
            *streaming = false;
        }
        let messages = view.sync(&app);
        assert_eq!(messages[0]["upserts"][0]["block"]["text"], "hello world");
        assert_eq!(messages[0]["upserts"][0]["block"]["streaming"], false);
        assert!(view.sync(&app).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn prepending_and_truncating_preserve_index_semantics() {
        let (_fake, mut app) = connected_app();
        let user = |text: &str| Block::User {
            text: text.into(),
            kind: UserKind::Prompt,
            at: None,
        };
        app.session.as_mut().unwrap().transcript.push(user("new"));
        let mut view = View::default();
        view.sync(&app);
        app.session
            .as_mut()
            .unwrap()
            .transcript
            .insert(0, user("old"));
        let messages = view.sync(&app);
        assert_eq!(messages[0]["upserts"].as_array().unwrap().len(), 2);
        assert_eq!(messages[0]["upserts"][0]["block"]["text"], "old");
        assert_eq!(messages[0]["upserts"][1]["block"]["text"], "new");
        app.session.as_mut().unwrap().transcript.remove(1);
        assert_eq!(
            view.sync(&app),
            [json!({"t":"blocks", "len":1, "upserts":[]})]
        );
    }

    #[test]
    fn unchanged_large_sessions_emit_nothing_and_reuse_the_catalog() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let (_fake, mut app) = connected_app();
                app.remote.config = Some(crate::config::Remote {
                    bind: "127.0.0.1:0".parse().unwrap(),
                    url: None,
                    roots: vec![app.workspace.clone()],
                    allow_insecure: false,
                });
                for i in 0..500 {
                    let mut info = crate::testing::session_info(&format!("s{i}"));
                    info["workspace"] = json!(app.workspace);
                    app.sessions.push(serde_json::from_value(info).unwrap());
                }
                for i in 0..5000 {
                    app.session.as_mut().unwrap().transcript.push(Block::User {
                        text: format!("message {i}"),
                        kind: UserKind::Prompt,
                        at: None,
                    });
                }
                let mut view = View::default();
                assert_eq!(view.sync(&app)[0]["blocks"].as_array().unwrap().len(), 5000);
                let cached = view.catalog["sessions"].as_array().unwrap().as_ptr();
                for _ in 0..10 {
                    assert!(view.sync(&app).is_empty());
                }
                assert_eq!(
                    view.catalog["sessions"].as_array().unwrap().as_ptr(),
                    cached
                );
                app.sessions[0].title = Some("renamed".into());
                app.catalog_generation += 1;
                assert_eq!(view.sync(&app)[0]["t"], "state");
                assert_ne!(
                    view.catalog["sessions"].as_array().unwrap().as_ptr(),
                    cached
                );
            });
    }

    #[cfg(windows)]
    #[tokio::test(start_paused = true)]
    async fn phone_workspaces_omit_verbatim_prefixes_without_changing_root_checks() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (_fake, mut app) = crate::testing::FakeAgent::start(tx);
        app.workspace = std::path::PathBuf::from(r"\\?\C:\work");
        app.remote.config = Some(crate::config::Remote {
            bind: "127.0.0.1:0".parse().unwrap(),
            url: None,
            roots: vec![app.workspace.clone()],
            allow_insecure: false,
        });
        let mut info = crate::testing::session_info("s1");
        info["workspace"] = json!(app.workspace);
        let info: crate::protocol::SessionInfo = serde_json::from_value(info).unwrap();
        app.sessions.push(info.clone());
        app.session = Some(crate::app::Session::new(info, 1));
        let state = state(&app, &catalog(&app));
        for workspace in [
            &state["draft"]["workspace"],
            &state["session"]["workspace"],
            &state["sessions"][0]["workspace"],
        ] {
            assert_eq!(workspace, r"C:\work");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn only_changed_indices_are_upserted_and_residency_replaces_the_projection() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (_fake, mut app) = crate::testing::FakeAgent::start(tx);
        app.remote.config = Some(crate::config::Remote {
            bind: "127.0.0.1:0".parse().unwrap(),
            url: None,
            roots: vec![app.workspace.clone()],
            allow_insecure: false,
        });
        let (outbound, _output) = mpsc::channel(512);
        let (close, _closing) = watch::channel(None);
        app.remote.connection = Some(Connection {
            id: 1,
            generation: 0,
            device: Device {
                name: "phone".into(),
                token_sha256: "hash".into(),
                paired_at: String::new(),
            },
            outbound,
            close,
        });
        let mut view = View::default();
        assert_eq!(view.sync(&app)[0]["t"], "snapshot");
        assert!(view.sync(&app).is_empty());
        app.session = Some(crate::app::Session::new(
            serde_json::from_value(crate::testing::session_info("s1")).unwrap(),
            1,
        ));
        let transcript = &mut app.session.as_mut().unwrap().transcript;
        for text in ["one", "two"] {
            transcript.push(Block::User {
                text: text.into(),
                kind: UserKind::Prompt,
                at: None,
            });
        }
        assert_eq!(view.sync(&app)[0]["t"], "snapshot");
        if let Some(Block::User { text, .. }) =
            app.session.as_mut().unwrap().transcript.block_mut(1)
        {
            *text = "changed".into();
        }
        let messages = view.sync(&app);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0],
            json!({"t":"blocks", "len":2, "upserts":[{"i":1, "block":{"type":"user", "kind":"prompt", "text":"changed", "at":null}}]})
        );
        assert!(view.sync(&app).is_empty());
        app.session.as_mut().unwrap().epoch = 2;
        assert_eq!(view.sync(&app)[0]["t"], "snapshot");
        app.session = None;
        assert_eq!(view.sync(&app)[0]["t"], "snapshot");
    }
}
