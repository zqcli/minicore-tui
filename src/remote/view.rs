//! Semantic state and revision-indexed transcript updates for each phone.

use std::path::Path;

use serde_json::{Value, json};

use super::{PhoneView, fs};

use crate::app::session::context_percent;
use crate::app::{App, InteractionKind, LiveStatus, QueuedKind, Session};
use crate::protocol::read::UserKind;
use crate::transcript::{Block, RequestKey, ToolState};

/// What one connection was last sent.
#[derive(Default)]
pub struct View {
    /// `None` until the first snapshot, then the viewed session's epoch.
    epoch: Option<Option<u64>>,
    revisions: Vec<u64>,
    state: String,
    streaming: Vec<Option<Streaming>>,
}

/// The catalog part of `State`, rebuilt only when the catalog or the listener changes.
#[derive(Default)]
pub struct Catalog {
    generation: Option<(u64, u64)>,
    value: Value,
}

impl Catalog {
    fn refresh(app: &mut App) {
        let generation = (app.catalog_generation, app.remote.generation);
        if app.remote.catalog.generation != Some(generation) {
            app.remote.catalog = Self {
                generation: Some(generation),
                value: catalog(app),
            };
        }
    }
}

/// This sync's `State`, shared by connections viewing the same session or draft settings.
pub struct Shared {
    pub connections: Vec<u64>,
    view: PhoneView,
    loading: bool,
    state: Value,
    serialized: String,
}

impl Shared {
    pub fn build(app: &mut App) -> Vec<Self> {
        Catalog::refresh(app);
        let mut groups: Vec<Self> = Vec::new();
        for phone in &app.remote.phones {
            let loading = app.loading_session
                && app
                    .remote
                    .opening
                    .as_ref()
                    .is_some_and(|opening| *opening == phone.connection.id);
            let group = match groups
                .iter_mut()
                .find(|group| group.view == phone.view && group.loading == loading)
            {
                Some(group) => group,
                None => {
                    groups.push(Self::new(app, phone.view.clone(), loading));
                    groups.last_mut().unwrap()
                }
            };
            group.connections.push(phone.connection.id);
        }
        groups
    }

    fn new(app: &App, view: PhoneView, loading: bool) -> Self {
        let state = state(app, &app.remote.catalog.value, &view, loading);
        Self {
            connections: Vec::new(),
            view,
            loading,
            serialized: state.to_string(),
            state,
        }
    }
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
    pub fn sync(&mut self, session: Option<&Session>, shared: &Shared) -> Vec<Value> {
        let epoch = session.map(|session| session.epoch);
        let revisions = session.map_or(&[][..], |session| session.transcript.revisions());
        let blocks = session.map_or(&[][..], |session| session.transcript.blocks());
        let mut messages = Vec::new();
        if self.epoch != Some(epoch) {
            messages.push(json!({"t":"snapshot", "state":shared.state, "blocks":blocks.iter().map(block).collect::<Vec<_>>()}));
            self.streaming = blocks.iter().map(Streaming::of).collect();
        } else {
            self.streaming.resize_with(revisions.len(), || None);
            let mut upserts = Vec::new();
            for (i, revision) in revisions.iter().enumerate() {
                if self.revisions.get(i) == Some(revision) {
                    continue;
                }
                let current = &blocks[i];
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
            if shared.serialized != self.state {
                messages.push(json!({"t":"state", "state":shared.state}));
            }
        }
        self.epoch = Some(epoch);
        self.revisions.clear();
        self.revisions.extend_from_slice(revisions);
        self.state.clone_from(&shared.serialized);
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

fn state(app: &App, catalog: &Value, view: &PhoneView, loading: bool) -> Value {
    let session = view
        .session_id()
        .map(|id| app.live(id).expect("a phone views a live session"));
    let (workspace, profile, model, reasoning) = match view {
        PhoneView::Session(_) => {
            let session = session.unwrap();
            (
                Path::new(&session.info.workspace),
                Some(session.info.profile.as_str()),
                Some(session.info.model.as_str()),
                Some(session.info.reasoning),
            )
        }
        PhoneView::Draft(draft) => (
            draft.workspace.as_path(),
            draft
                .launch
                .profile
                .as_deref()
                .or(app.default_profile.as_deref()),
            draft
                .launch
                .model_id(&app.profiles, app.default_profile.as_deref()),
            draft
                .launch
                .reasoning(&app.profiles, app.default_profile.as_deref()),
        ),
    };
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
    let (running, approval) = app.other_counts(current);
    json!({
        "session":session.map(|s| json!({"id":s.info.session_id, "title":crate::app::overlay::session_label(&s.info, session), "workspace":fs::display(Path::new(&s.info.workspace)), "profile":s.info.profile, "model":s.info.model, "reasoning":s.info.reasoning})),
        "draft":{"workspace":fs::display(workspace), "profile":profile, "model":model, "reasoning":reasoning},
        "status":{"label":session.and_then(Session::status_label), "busy":session.is_some_and(Session::is_busy), "started_ms":live.map(|live| live.started_ms), "interrupting":live.is_some_and(|live| live.interrupting), "compacting":session.is_some_and(Session::compacting), "loading":loading},
        "usage":{"input":session.map_or(0, |s| s.usage.input), "output":session.map_or(0, |s| s.usage.output), "cached":session.map_or(0, |s| s.usage.cached)},
        "context_percent":session.and_then(context_percent), "interaction":interaction, "queue":queue,
        "older":session.is_some_and(|s| s.loaded_from > 0), "sessions":sessions,
        "background":{"running":running, "approval":approval},
        "devices":app.remote.phones.iter().map(|phone| json!({"name":phone.connection.device.name, "session":phone.view.session_id()})).collect::<Vec<_>>(),
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
    use tokio::sync::mpsc;

    fn sync(view: &mut View, app: &mut App) -> Vec<Value> {
        Catalog::refresh(app);
        let shared = Shared::new(app, PhoneView::foreground(app), false);
        view.sync(app.session.as_ref(), &shared)
    }

    fn connected_app() -> (crate::testing::FakeAgent, App) {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (fake, mut app) = crate::testing::FakeAgent::start(tx);
        app.remote.config = Some(crate::config::Remote {
            bind: "127.0.0.1:0".parse().unwrap(),
            url: None,
            roots: vec![app.workspace.clone()],
            allow_insecure: false,
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
        let snapshot = sync(&mut view, &mut app);
        let draft = &snapshot[0]["state"]["draft"];
        assert_eq!(draft["profile"], "coding");
        assert_eq!(draft["model"], "main");
        assert_eq!(draft["reasoning"], "medium");
        assert!(snapshot[0]["state"].get("now_ms").is_none());
        assert!(snapshot[0]["state"]["status"].get("now_ms").is_none());
        tokio::time::advance(std::time::Duration::from_secs(96)).await;
        assert!(sync(&mut view, &mut app).is_empty());

        app.launch.profile = Some("ask".into());
        let changed = sync(&mut view, &mut app);
        assert_eq!(changed[0]["t"], "state");
        assert_eq!(changed[0]["state"]["draft"]["profile"], "ask");
        assert_eq!(changed[0]["state"]["draft"]["model"], "quick");
        assert_eq!(changed[0]["state"]["draft"]["reasoning"], "low");

        app.launch.model = Some("chosen".into());
        app.launch.reasoning = Some(crate::protocol::Reasoning::High);
        let changed = sync(&mut view, &mut app);
        assert_eq!(changed[0]["state"]["draft"]["model"], "chosen");
        assert_eq!(changed[0]["state"]["draft"]["reasoning"], "high");
        assert_eq!(changed[0]["state"]["draft"]["profile"], "ask");

        app.launch = crate::app::LaunchSettings::default();
        app.default_profile = None;
        let changed = sync(&mut view, &mut app);
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
        assert_eq!(sync(&mut view, &mut app)[0]["t"], "snapshot");
        tokio::time::advance(std::time::Duration::from_secs(60)).await;
        assert!(sync(&mut view, &mut app).is_empty());
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
            sync(&mut view, &mut app),
            [json!({"t":"blocks", "len":1, "upserts":[{"i":0, "append":{"text":" world"}}]})]
        );
        if let Block::Assistant { reasoning, .. } =
            app.session.as_mut().unwrap().transcript.assistant_mut(&key)
        {
            reasoning.push('え');
        }
        assert_eq!(
            sync(&mut view, &mut app)[0]["upserts"][0]["append"],
            json!({"reasoning":"え"})
        );
        if let Some(Block::Assistant { streaming, .. }) =
            app.session.as_mut().unwrap().transcript.block_mut(0)
        {
            *streaming = false;
        }
        let messages = sync(&mut view, &mut app);
        assert_eq!(messages[0]["upserts"][0]["block"]["text"], "hello world");
        assert_eq!(messages[0]["upserts"][0]["block"]["streaming"], false);
        assert!(sync(&mut view, &mut app).is_empty());
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
        sync(&mut view, &mut app);
        app.session
            .as_mut()
            .unwrap()
            .transcript
            .insert(0, user("old"));
        let messages = sync(&mut view, &mut app);
        assert_eq!(messages[0]["upserts"].as_array().unwrap().len(), 2);
        assert_eq!(messages[0]["upserts"][0]["block"]["text"], "old");
        assert_eq!(messages[0]["upserts"][1]["block"]["text"], "new");
        app.session.as_mut().unwrap().transcript.remove(1);
        assert_eq!(
            sync(&mut view, &mut app),
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
                assert_eq!(
                    sync(&mut view, &mut app)[0]["blocks"]
                        .as_array()
                        .unwrap()
                        .len(),
                    5000
                );
                let cached = app.remote.catalog.value["sessions"]
                    .as_array()
                    .unwrap()
                    .as_ptr();
                for _ in 0..10 {
                    assert!(sync(&mut view, &mut app).is_empty());
                }
                assert_eq!(
                    app.remote.catalog.value["sessions"]
                        .as_array()
                        .unwrap()
                        .as_ptr(),
                    cached
                );
                app.sessions[0].title = Some("renamed".into());
                app.catalog_generation += 1;
                assert_eq!(sync(&mut view, &mut app)[0]["t"], "state");
                assert_ne!(
                    app.remote.catalog.value["sessions"]
                        .as_array()
                        .unwrap()
                        .as_ptr(),
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
        let state = state(&app, &catalog(&app), &PhoneView::foreground(&app), false);
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
        let mut view = View::default();
        assert_eq!(sync(&mut view, &mut app)[0]["t"], "snapshot");
        assert!(sync(&mut view, &mut app).is_empty());
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
        assert_eq!(sync(&mut view, &mut app)[0]["t"], "snapshot");
        if let Some(Block::User { text, .. }) =
            app.session.as_mut().unwrap().transcript.block_mut(1)
        {
            *text = "changed".into();
        }
        let messages = sync(&mut view, &mut app);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0],
            json!({"t":"blocks", "len":2, "upserts":[{"i":1, "block":{"type":"user", "kind":"prompt", "text":"changed", "at":null}}]})
        );
        assert!(sync(&mut view, &mut app).is_empty());
        app.session.as_mut().unwrap().epoch = 2;
        assert_eq!(sync(&mut view, &mut app)[0]["t"], "snapshot");
        app.session = None;
        assert_eq!(sync(&mut view, &mut app)[0]["t"], "snapshot");
    }
}
