//! Semantic state and revision-indexed transcript updates for one phone.

use serde_json::{Value, json};

use crate::app::session::context_percent;
use crate::app::{App, InteractionKind, QueuedKind};
use crate::protocol::read::UserKind;
use crate::transcript::{Block, ToolState};

#[derive(Default)]
pub struct View {
    connection: Option<u64>,
    epoch: Option<u64>,
    revisions: Vec<u64>,
    state: String,
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
        let state = state(app);
        let serialized = state.to_string();
        let mut messages = Vec::new();
        if self.connection != Some(phone.id) || self.epoch != epoch {
            messages.push(json!({"t":"snapshot", "state":state, "blocks":app.transcript_blocks().iter().map(block).collect::<Vec<_>>()}));
        } else {
            let upserts: Vec<_> = revisions
                .iter()
                .enumerate()
                .filter(|(i, rev)| self.revisions.get(*i) != Some(rev))
                .map(|(i, _)| json!({"i":i, "block":block(&app.transcript_blocks()[i])}))
                .collect();
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

pub fn state(app: &App) -> Value {
    let session = app.session.as_ref();
    let live = session.and_then(|s| s.live.as_ref());
    let current = session.map(|s| s.info.session_id.as_str());
    let mut sessions: Vec<_> = app
        .sessions
        .iter()
        .filter(|info| {
            app.remote
                .roots()
                .iter()
                .any(|root| std::path::Path::new(&info.workspace).starts_with(root))
        })
        .collect();
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let sessions: Vec<_> = sessions.into_iter().take(50).map(|info| json!({
        "id":info.session_id, "title":crate::app::overlay::session_label(info, session),
        "workspace":info.workspace, "updated_at":info.updated_at, "current":Some(info.session_id.as_str()) == current,
    })).collect();
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
    json!({
        "session":session.map(|s| json!({"id":s.info.session_id, "title":crate::app::overlay::session_label(&s.info, session), "workspace":s.info.workspace, "profile":s.info.profile, "model":s.info.model, "reasoning":s.info.reasoning})),
        "draft":{"workspace":app.workspace, "profile":app.launch.profile.as_ref().or(app.default_profile.as_ref()), "model":app.launch.model, "reasoning":app.launch.reasoning},
        "status":{"label":app.status_label(), "busy":app.is_busy(), "started_ms":live.map(|live| live.started_ms), "interrupting":live.is_some_and(|live| live.interrupting), "compacting":app.compacting(), "loading":app.loading_session},
        "usage":{"input":session.map_or(0, |s| s.usage.input), "output":session.map_or(0, |s| s.usage.output), "cached":session.map_or(0, |s| s.usage.cached)},
        "context_percent":session.and_then(context_percent), "interaction":interaction, "queue":queue,
        "older":session.is_some_and(|s| s.loaded_from > 0), "sessions":sessions,
        "models":app.models.iter().map(|model| json!({"id":model.id, "reasoning":model.supported_reasoning})).collect::<Vec<_>>(),
        "profiles":app.profiles.iter().map(|profile| &profile.id).collect::<Vec<_>>()
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

    #[tokio::test(start_paused = true)]
    async fn only_changed_indices_are_upserted_and_residency_replaces_the_projection() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (_fake, mut app) = crate::testing::FakeAgent::start(tx);
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
