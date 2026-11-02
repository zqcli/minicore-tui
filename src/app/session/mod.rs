//! A resident session and the shared identities and coordinates of its content.

mod body;
mod completion;
mod history;

use std::collections::{BTreeSet, VecDeque};
use std::time::Duration;

use crate::protocol::read::UserKind;
use crate::protocol::stamp::{now_millis, unix_millis};
use crate::protocol::{
    AgentEvent, BlockReason, CompactionProgress, ContextUsage, Queue, SessionContext, SessionInfo,
    SessionPresentation, SessionSnapshot, SnapshotInteraction, TurnRef, Usage, UsageTotals,
};
use crate::transcript::{Block, Transcript};

use super::Scroll;

/// How soon the footer's figures may be asked for again while a turn runs.
const PRESENTATION_EVERY: Duration = Duration::from_secs(2);
/// Recent completions and canonical installs retained independently.
const COMPLETIONS: usize = 256;
/// Turn regions remembered for a completion that may still arrive.
const RECONCILE_REGIONS: usize = 256;
pub const EARLY_BYTES: usize = 2 * 1024 * 1024;
pub const EARLY_TURNS: usize = 8;

pub struct Session {
    #[cfg(feature = "e2e-observe")]
    pub observed_installs: Vec<super::observe::Installed>,
    pub info: SessionInfo,
    /// Identifies this live session; results posted for another one are stale.
    pub epoch: u64,
    pub transcript: Transcript,
    pub live: Option<LiveTurn>,
    ended_live: VecDeque<LiveTurn>,
    pub(super) auto_title_asked: bool,
    /// The Agent's queue as last reported; the Agent owns it.
    pub queue: Queue,
    early: VecDeque<(AgentEvent, usize)>,
    pub early_bytes: usize,
    #[cfg(feature = "e2e-observe")]
    pub early_peak_bytes: usize,
    pub early_gap: bool,
    pub usage: UsageTotals,
    pub presentation: Option<SessionPresentation>,
    pub branch: Option<String>,
    /// When `presentation` was last asked for, and whether that answer is still due.
    pub presentation_asked: Option<tokio::time::Instant>,
    pub presentation_in_flight: bool,
    pub context: Option<SessionContext>,
    pub control_in_flight: bool,
    pub control_due: bool,
    /// The resident instance this session was opened with.
    pub load: String,
    /// Completion discovery watermarks and pending bodies for that instance.
    pub recovery: crate::app::recovery::RecoveryState,
    pub recovery_tasks: crate::app::recovery::RecoveryTasks,
    pub(super) tasks: super::tasks::Tasks,
    pub body_recovery: VecDeque<BodyRecovery>,
    /// The Agent's execution as the last snapshot observed it, which differs
    /// from `live` while a waited turn has not answered yet.
    pub execution: Option<crate::protocol::SessionExecution>,
    pub catalog: crate::protocol::CompletionDirectory,
    /// Completion order and whether each body is already canonical.
    completions: VecDeque<CompletionMark>,
    pub(super) delivered: VecDeque<DeliveredIdentity>,
    /// A turn a control snapshot adopted, which needs its own wait registered.
    pub adopted: Option<TurnRef>,
    /// A recovered turn whose ordering anchor is not loaded, so its position
    /// among the unsaved turns is unknown.
    pub ordering_gap: bool,
    /// Why the Agent blocks this session, if it does.
    pub block_reason: Option<BlockReason>,
    /// When `session.context` was last asked for while a turn ran.
    pub context_asked: Option<tokio::time::Instant>,
    /// The latest turn-start compaction attempt `session.context` reported.
    auto_seen: Option<String>,
    pub compaction: Option<CompactionProgress>,
    /// Coverage of the current summary from `session.context`; a summary
    /// replayed from history carries none of its own.
    pub summary_coverage: Option<(usize, usize)>,
    pub interaction: Option<PendingInteraction>,
    pub approval_seen: Option<String>,
    pub interaction_editor: super::composer::Composer,
    /// When the pending prompt starts taking answer keys; a stray key before then moves it back.
    pub armed_at: tokio::time::Instant,
    /// Tools whose approvals the user allowed for the rest of this session.
    pub allowed_tools: BTreeSet<String>,
    /// The display projection's first item; older items are behind the
    /// compaction summary.
    pub first_item: usize,
    /// The oldest source item loaded.
    pub loaded_from: usize,
    /// The exclusive end of the contiguous window that starts at `loaded_from`.
    loaded_end: usize,
    pub loading_older: bool,
    /// Pages still to load behind the summary until the latest answer shows.
    pub backfill: u8,
    /// The finished turns whose durable items will replace their streamed
    /// regions, with where each region starts.
    pub reconcile_from: Vec<(String, usize)>,
    /// The composer text this session had when it left the foreground.
    pub draft: Option<String>,
    /// Refused texts awaiting local control because no remote recipient received them.
    pub returned: Vec<String>,
    /// A turn ended while this session was in the background.
    pub unseen: bool,
    /// The view this session had when it left the foreground.
    pub scroll: Scroll,
}

/// What a live session is doing, for the picker, the footer and the phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveStatus {
    Idle,
    Running,
    Approval,
    Compacting,
}

pub struct BodyRecovery {
    pub loop_id: String,
    pub status: super::recovery::RecoveryStatus,
    pub message: String,
}

pub(super) struct DeliveredIdentity {
    #[cfg(feature = "e2e-observe")]
    pub loop_id: String,
    pub input_id: String,
    #[cfg(feature = "e2e-observe")]
    pub request_index: u32,
}

struct CompletionMark {
    loop_id: String,
    seq: u64,
    canonical: bool,
}

pub struct LiveTurn {
    pub(super) completion_background: Option<bool>,
    pub turn: TurnRef,
    pub started: tokio::time::Instant,
    pub started_ms: u64,
    pub requests: u32,
    pub phase: Phase,
    /// Index in the transcript where this turn's blocks start.
    pub start: usize,
    /// The tool running now and its detail, for the status line.
    pub running_tool: Option<(String, String)>,
    /// Esc asked for cancellation and `turn.wait` has not answered yet.
    pub interrupting: bool,
    pub accounted_usage: Usage,
    pub usage_final: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Thinking,
    Writing,
    Waiting,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Thinking => "Thinking",
            Self::Writing => "Writing",
            Self::Waiting => "Waiting for model",
        }
    }
}

impl LiveTurn {
    pub fn new(turn: TurnRef, start: usize) -> Self {
        Self {
            completion_background: None,
            turn,
            started: tokio::time::Instant::now(),
            started_ms: crate::protocol::stamp::now_millis(),
            requests: 1,
            phase: Phase::Thinking,
            start,
            running_tool: None,
            interrupting: false,
            accounted_usage: Usage::default(),
            usage_final: false,
        }
    }
}

/// How long a prompt waits after it appears, or after a stray key, before it takes an answer.
pub const ARM_AFTER: Duration = Duration::from_millis(500);
pub const TEXT_ANSWER_MAX_BYTES: usize = 8192;

#[derive(Clone)]
pub struct PendingInteraction {
    pub turn: TurnRef,
    pub interaction_id: String,
    /// The call asking; its card says what would run.
    pub tool_call_id: String,
    pub tool_name: String,
    pub prompt: String,
    pub kind: InteractionKind,
    pub selected: usize,
}

#[derive(Clone)]
pub enum InteractionKind {
    Approval,
    Choice(Vec<String>),
    Text,
}

impl PendingInteraction {
    /// The interaction a snapshot carries, with the turn and request that asked.
    pub fn from_snapshot(snapshot: &SessionSnapshot, pending: &SnapshotInteraction) -> Self {
        let turn = TurnRef::new(&snapshot.session_id, &pending.loop_id);
        Self::from_wire(turn, &pending.interaction)
    }

    pub fn from_wire(turn: TurnRef, interaction: &crate::protocol::PendingInteraction) -> Self {
        let data = interaction.kind.get("data");
        let field = |name: &str| data.and_then(|data| data.get(name));
        let prompt = field("prompt")
            .and_then(|prompt| prompt.as_str())
            .unwrap_or("The agent needs an answer")
            .to_owned();
        let choices: Vec<String> = field("choices")
            .and_then(|choices| choices.as_array())
            .map(|choices| {
                choices
                    .iter()
                    .filter_map(|choice| choice.get("text")?.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let kind = match interaction.kind.get("type").and_then(|kind| kind.as_str()) {
            Some("approval") => InteractionKind::Approval,
            Some("tool_input") => match field("answer_kind").and_then(|kind| kind.as_str()) {
                Some("text") => InteractionKind::Text,
                _ => InteractionKind::Choice(choices),
            },
            _ => InteractionKind::Approval,
        };
        Self {
            turn,
            interaction_id: interaction.interaction_id.clone(),
            tool_call_id: interaction.tool_call_id.clone(),
            tool_name: interaction.tool_name.clone(),
            prompt,
            kind,
            selected: 0,
        }
    }

    /// The rows the prompt offers; for an approval, row 0 allows once, row 1
    /// denies and row 2 allows the tool for the session.
    pub fn options(&self) -> Vec<String> {
        match &self.kind {
            InteractionKind::Approval => vec![
                "Allow once".into(),
                "Deny".into(),
                format!("Allow {} this session", self.tool_name),
            ],
            InteractionKind::Choice(choices) => choices.clone(),
            InteractionKind::Text => Vec::new(),
        }
    }
}

impl Session {
    pub fn new(info: SessionInfo, epoch: u64, load: String) -> Self {
        Self {
            #[cfg(feature = "e2e-observe")]
            observed_installs: Vec::new(),
            info,
            epoch,
            transcript: Transcript::default(),
            live: None,
            ended_live: VecDeque::new(),
            auto_title_asked: false,
            queue: Queue::default(),
            early: VecDeque::new(),
            early_bytes: 0,
            #[cfg(feature = "e2e-observe")]
            early_peak_bytes: 0,
            early_gap: false,
            usage: UsageTotals::default(),
            presentation: None,
            branch: None,
            presentation_asked: None,
            presentation_in_flight: false,
            context: None,
            control_in_flight: false,
            control_due: false,
            load,
            recovery: crate::app::recovery::RecoveryState::default(),
            recovery_tasks: crate::app::recovery::RecoveryTasks::default(),
            tasks: super::tasks::Tasks::default(),
            body_recovery: VecDeque::new(),
            execution: None,
            catalog: crate::protocol::CompletionDirectory::default(),
            completions: VecDeque::new(),
            delivered: VecDeque::new(),
            adopted: None,
            ordering_gap: false,
            block_reason: None,
            context_asked: None,
            auto_seen: None,
            compaction: None,
            summary_coverage: None,
            interaction: None,
            approval_seen: None,
            interaction_editor: super::composer::Composer::new(),
            armed_at: tokio::time::Instant::now(),
            allowed_tools: BTreeSet::new(),
            first_item: 0,
            loaded_from: 0,
            loaded_end: 0,
            loading_older: false,
            backfill: 0,
            reconcile_from: Vec::new(),
            draft: None,
            returned: Vec::new(),
            unseen: false,
            scroll: Scroll::Follow,
        }
    }

    fn tracked_turn(&self, loop_id: &str) -> Option<&LiveTurn> {
        self.live
            .iter()
            .chain(&self.ended_live)
            .find(|live| live.turn.loop_id == loop_id)
    }

    pub(super) fn tracked_turn_mut(&mut self, loop_id: &str) -> Option<&mut LiveTurn> {
        self.live
            .iter_mut()
            .chain(&mut self.ended_live)
            .find(|live| live.turn.loop_id == loop_id)
    }

    pub(super) fn retire_live(&mut self) {
        if let Some(live) = self.live.take() {
            self.ended_live.push_back(live);
            if self.ended_live.len() > RECONCILE_REGIONS {
                self.ended_live.pop_front();
            }
        }
    }

    pub fn is_busy(&self) -> bool {
        self.live.is_some()
            || self.execution.as_ref().is_some_and(|execution| {
                !matches!(execution, crate::protocol::SessionExecution::None)
            })
    }

    pub fn compacting(&self) -> bool {
        self.compaction.is_some()
    }

    /// The branch shared by the terminal and phone, from the last workspace observation.
    pub fn branch(&self) -> Option<&str> {
        self.branch
            .as_deref()
            .filter(|branch| !branch.trim().is_empty())
    }

    pub fn status_label(&self) -> Option<&'static str> {
        if self.compacting() {
            Some("Compacting context")
        } else if self
            .interaction
            .as_ref()
            .is_some_and(|pending| matches!(pending.kind, InteractionKind::Text))
        {
            Some("Waiting for input")
        } else if self.interaction.is_some() {
            Some("Waiting for approval")
        } else {
            self.phase_label()
        }
    }

    pub fn live_status(&self) -> LiveStatus {
        self.control_status()
    }

    /// The status line while a compaction runs. Inside a turn Esc ends the
    /// whole turn, which is the only way to stop that compaction.
    pub fn compaction_label(&self) -> Option<String> {
        let progress = self.compaction.as_ref()?;
        let key = match self.is_busy() {
            true => "interrupt",
            false => "cancel",
        };
        Some(format!(
            "Compacting context · {} (esc to {key})",
            progress.phase.label()
        ))
    }

    /// Whether the footer's figures may be asked for again mid-turn: no answer
    /// is on its way and the last ask is old enough.
    pub fn presentation_due(&self) -> bool {
        !self.presentation_in_flight
            && self
                .presentation_asked
                .is_none_or(|asked| asked.elapsed() >= PRESENTATION_EVERY)
    }

    /// The status-line phase while a turn runs.
    pub fn phase_label(&self) -> Option<&'static str> {
        self.live
            .as_ref()
            .map(|live| match live.interrupting {
                true => "Interrupting",
                false => live.phase.label(),
            })
            .or_else(|| self.execution.as_ref()?.phase().map(|phase| phase.label()))
    }

    /// The first prompt the user typed, as loaded.
    pub fn first_prompt(&self) -> Option<&str> {
        self.transcript
            .blocks()
            .iter()
            .find_map(|block| match block {
                Block::User {
                    text,
                    kind: UserKind::Prompt,
                    ..
                } => Some(text.as_str()),
                _ => None,
            })
    }

    /// The title an untitled session gets from its first prompt.
    pub fn auto_title(&self) -> Option<String> {
        let title = self.info.title.as_deref();
        let untitled = title.is_none_or(|title| title.trim().is_empty());
        untitled.then(|| self.first_prompt().and_then(auto_title))?
    }

    fn region_start(&self, loop_id: &str) -> Option<usize> {
        self.reconcile_from
            .iter()
            .find(|(id, _)| id == loop_id)
            .map(|(_, start)| *start)
    }

    /// Remembers a streamed region until its completion can replace it.
    pub fn record_region(&mut self, loop_id: &str, start: usize) {
        self.reconcile_from.retain(|(id, _)| id != loop_id);
        self.reconcile_from.push((loop_id.to_owned(), start));
        if self.reconcile_from.len() > RECONCILE_REGIONS {
            self.reconcile_from.remove(0);
        }
    }

    /// The loop the last control snapshot saw executing; it differs from
    /// `live` while a waited turn has not answered yet.
    pub fn observed_loop(&self) -> Option<&str> {
        self.execution
            .as_ref()
            .and_then(|execution| execution.loop_id())
    }

    fn boundaries_mut(&mut self) -> impl Iterator<Item = (&str, &mut usize)> {
        self.live
            .iter_mut()
            .chain(&mut self.ended_live)
            .map(|live| (live.turn.loop_id.as_str(), &mut live.start))
            .chain(
                self.reconcile_from
                    .iter_mut()
                    .map(|(id, at)| (id.as_str(), at)),
            )
    }

    /// Moves every boundary for a region `start..end` replaced by `grown` more
    /// blocks: before it stays, inside it lands on `start`, after it moves.
    fn rebase_range(&mut self, start: usize, end: usize, grown: isize) {
        let shift = |index: &mut usize| {
            *index = match *index {
                // Strictly inside the replaced region: the region's first block
                // is the nearest boundary that still exists.
                inside if inside > start && inside < end => start,
                // At or after the region's end: a successor that started there
                // keeps its own blocks, which now sit after the new body.
                after if after >= end => after.saturating_add_signed(grown),
                before => before,
            };
        };
        for (_, at) in self.boundaries_mut() {
            shift(at);
        }
    }

    fn region_end(&self, loop_id: &str) -> usize {
        let start = self
            .region_start(loop_id)
            .or_else(|| turn_bounds(self.transcript.blocks(), loop_id).map(|range| range.start))
            .unwrap_or(self.transcript.len());
        self.reconcile_from
            .iter()
            .filter(|(id, at)| id != loop_id && *at >= start)
            .map(|(_, at)| *at)
            .chain(self.transcript.blocks()[start..].iter().position(|block| matches!(block, Block::TurnEnd { loop_id: id, .. } if id == loop_id)).map(|at| start + at))
            .min()
            .unwrap_or(self.transcript.len())
    }

    /// Moves region boundaries after blocks were inserted or removed at `at`.
    fn rebase(&mut self, at: usize, delta: isize) {
        let shift = |index: &mut usize| {
            if *index >= at {
                *index = index.saturating_add_signed(delta);
            }
        };
        for (_, start) in self.boundaries_mut() {
            shift(start);
        }
    }
}

fn turn_bounds(blocks: &[Block], loop_id: &str) -> Option<std::ops::Range<usize>> {
    let end = blocks
        .iter()
        .position(|block| matches!(block, Block::TurnEnd { loop_id: id, .. } if id == loop_id))?
        + 1;
    let start = blocks[..end - 1]
        .iter()
        .rposition(|block| matches!(block, Block::TurnEnd { .. }))
        .map_or(0, |at| at + 1);
    Some(start..end)
}

/// The Agent's acceptance time of an input in Unix milliseconds, else now.
pub(super) fn accepted_millis(stamp: Option<&str>) -> u64 {
    stamp.and_then(unix_millis).unwrap_or_else(now_millis)
}

/// The first meaningful line of a prompt, cut at a word boundary near 60
/// cells.
pub fn auto_title(prompt: &str) -> Option<String> {
    let first = prompt.lines().find(|line| !line.trim().is_empty())?.trim();
    let first = first.trim_start_matches(['#', '>', '-', '*', ' ']);
    if first.is_empty() {
        return None;
    }
    let mut title = String::new();
    let mut cells = 0;
    for word in first.split_whitespace() {
        let width = unicode_width::UnicodeWidthStr::width(word) + usize::from(!title.is_empty());
        if cells + width > 60 {
            break;
        }
        if !title.is_empty() {
            title.push(' ');
        }
        title.push_str(word);
        cells += width;
    }
    Some(if title.is_empty() {
        first.chars().take(60).collect()
    } else if title == first {
        title
    } else {
        format!("{title}…")
    })
}

pub(crate) fn input_budget(context: Option<&SessionContext>) -> Option<u64> {
    context?
        .budget
        .as_ref()?
        .input_budget_tokens
        .filter(|budget| *budget > 0)
}

/// The last reported context usage as a percentage of its input budget or model window.
pub fn context_percent(session: &Session) -> Option<f64> {
    let usage = session.presentation.as_ref()?.context.as_ref()?;
    reported_percent(usage, session.context.as_ref())
}

/// Reported usage as a percentage of the input budget in `context`, else of the model window.
pub fn reported_percent(usage: &ContextUsage, context: Option<&SessionContext>) -> Option<f64> {
    let tokens = usage.tokens?;
    match input_budget(context) {
        Some(budget) => Some(tokens as f64 / budget as f64 * 100.0),
        None => usage.percent.or_else(|| {
            usage
                .window
                .filter(|window| *window > 0)
                .map(|window| tokens as f64 / window as f64 * 100.0)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{DeliveredInput, InputMode, QueueItem, TurnWaitResult};

    fn session() -> Session {
        Session::new(
            serde_json::from_value(crate::testing::session_info("s")).unwrap(),
            1,
            crate::testing::LOAD.into(),
        )
    }

    fn item(id: &str, mode: InputMode, text: &str) -> QueueItem {
        QueueItem {
            id: id.into(),
            mode,
            text: text.into(),
            accepted_at: None,
        }
    }

    fn delivered(id: &str, text: &str) -> DeliveredInput {
        DeliveredInput {
            id: id.into(),
            text: text.into(),
            accepted_at: None,
            request_index: 1,
        }
    }

    fn steering(session: &Session) -> Vec<&str> {
        session
            .transcript
            .blocks()
            .iter()
            .filter_map(|block| match block {
                Block::User {
                    text,
                    kind: UserKind::Steering,
                    ..
                } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn delivered_inputs_project_body_without_writing_the_snapshot_queue() {
        let mut session = session();
        session.live = Some(LiveTurn::new(TurnRef::new("s", "l"), 0));
        session.queue = Queue {
            items: vec![
                item("a", InputMode::Steer, "first"),
                item("b", InputMode::Steer, "second"),
                item("c", InputMode::FollowUp, "later"),
            ],
            paused: false,
        };
        session.deliver(
            TurnRef::new("s", "l"),
            vec![delivered("a", "first"), delivered("b", "second")],
        );
        assert_eq!(steering(&session), ["first", "second"]);
        let queued: Vec<&str> = session.queue.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(queued, ["a", "b", "c"]);
    }

    #[test]
    fn inputs_delivered_before_the_turn_is_live_are_placed_when_it_starts() {
        let mut session = session();
        session.deliver(TurnRef::new("s", "l"), vec![delivered("a", "early")]);
        assert!(steering(&session).is_empty());
        session.live = Some(LiveTurn::new(TurnRef::new("s", "l"), 0));
        session.place_early("other");
        assert!(steering(&session).is_empty(), "another turn's inputs wait");
        session.place_early("l");
        assert_eq!(steering(&session), ["early"]);
        session.place_early("l");
        assert_eq!(steering(&session), ["early"], "placed once");
    }

    #[test]
    fn a_turn_ends_once_and_leaves_the_queue_to_the_agent() {
        let mut session = session();
        session.live = Some(LiveTurn::new(TurnRef::new("s", "l"), 0));
        session.queue = Queue {
            items: vec![item("a", InputMode::Steer, "steer")],
            paused: false,
        };
        let wait: TurnWaitResult = serde_json::from_value(serde_json::json!({
            "turn": {"session_id": "s", "loop_id": "l"}, "outcome": {"type": "completed"}
        }))
        .unwrap();
        assert!(session.end_turn(&wait));
        assert!(!session.end_turn(&wait), "the turn ends once");
        assert_eq!(session.queue.items.len(), 1);
    }

    #[test]
    fn titles_take_the_first_meaningful_line() {
        assert_eq!(
            auto_title("\n\n# Fix the parser\nand more"),
            Some("Fix the parser".into())
        );
        assert_eq!(auto_title("   \n \n"), None);
        assert_eq!(auto_title(&"a".repeat(200)).unwrap().chars().count(), 60);
    }

    #[test]
    fn long_titles_cut_at_a_word_boundary() {
        let title = auto_title(
            "Refactor the parser so that it can recover from a missing brace and report the line",
        )
        .unwrap();
        assert!(title.starts_with("Refactor the parser"), "{title:?}");
        assert!(title.ends_with('…'), "{title:?}");
        assert!(unicode_width::UnicodeWidthStr::width(title.as_str()) <= 61);
    }

    #[test]
    fn only_an_untitled_session_gets_an_auto_title() {
        let mut session = session();
        session.transcript.push(Block::User {
            text: "fix it".into(),
            kind: UserKind::Prompt,
            at: None,
        });
        assert_eq!(session.auto_title().as_deref(), Some("fix it"));
        session.info.title = Some("named".into());
        assert_eq!(session.auto_title(), None);
        session.info.title = Some("  ".into());
        assert_eq!(session.auto_title().as_deref(), Some("fix it"));
    }
}
