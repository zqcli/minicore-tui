//! Conversation search state (spec §17.1/§17.2).
//!
//! The panel is pure data: the query, the scope the user explicitly chose,
//! bounded match summaries, and an explicit coverage report. It never holds a
//! scanned body: only ≤ [`MAX_SEARCH_MATCHES`] summaries survive a scan, and a
//! scan that could not cover everything says so instead of claiming a global
//! no-match.

use std::sync::Arc;

use crate::state::view::SectionKind;

/// The most match summaries one scan keeps (spec §17.1).
pub const MAX_SEARCH_MATCHES: usize = 500;

/// Which content a search covers (spec §17.1). `Loaded` is the default and
/// never touches the remote session; `FullSession` is an explicit choice that
/// starts a pinned `session.read` scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum SearchScope {
    #[default]
    Loaded,
    FullSession,
}

impl SearchScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Loaded => "loaded content",
            Self::FullSession => "full session",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Loaded => Self::FullSession,
            Self::FullSession => Self::Loaded,
        }
    }
}

/// The kind of transcript content one match came from. This mirrors the
/// rendered section kinds so a jump can target the right section.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchSource {
    Prompt,
    Steering,
    AssistantText,
    Thinking,
    ToolName,
    ToolResult,
    Summary,
}

impl SearchSource {
    pub fn section_kind(self) -> SectionKind {
        match self {
            Self::Prompt | Self::Steering => SectionKind::User,
            Self::AssistantText => SectionKind::AssistantText,
            Self::Thinking => SectionKind::Thinking,
            Self::ToolName | Self::ToolResult => SectionKind::Tool,
            Self::Summary => SectionKind::Summary,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Steering => "steer",
            Self::AssistantText => "reply",
            Self::Thinking => "thinking",
            Self::ToolName => "tool",
            Self::ToolResult => "tool result",
            Self::Summary => "summary",
        }
    }

    /// Steering is a separate message type: it is searchable but is never a
    /// prompt-jump target (spec §17.2).
    pub fn is_prompt_jump(self) -> bool {
        self == Self::Prompt
    }
}

/// One bounded match summary. The body is never retained; `preview` is a short
/// excerpt around the hit and `source_offset` is the byte offset of the match
/// inside the section's logical source, which the scroll anchor uses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchMatch {
    pub index: Option<usize>,
    pub source: SearchSource,
    pub loop_id: Option<String>,
    pub request_index: Option<u32>,
    pub ordinal: u32,
    pub tool_call_id: Option<String>,
    pub preview: String,
    pub source_offset: usize,
    pub byte_range: std::ops::Range<usize>,
}

/// What a scan actually covered. `complete` is only true when every readable
/// item was scanned; large/undecodable items and a stopped scan keep it false
/// so the panel can never report a global no-match (spec §17.1).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchCoverage {
    pub loaded_items: usize,
    pub total_items: usize,
    pub scanned_items: usize,
    pub large_items: usize,
    pub failed_items: usize,
    pub truncated: bool,
    pub records_truncated: bool,
    pub stopped: bool,
    pub complete: bool,
}

impl SearchCoverage {
    /// A one-line honest scope statement shared by the panel and notices.
    pub fn label(&self, scope: SearchScope) -> String {
        let mut parts = vec![scope.label().to_owned()];
        match scope {
            SearchScope::Loaded => {
                if self.loaded_items > 0 {
                    parts.push(format!(
                        "{} of {} loaded items",
                        self.loaded_items, self.total_items
                    ));
                }
            }
            SearchScope::FullSession => {
                if self.total_items == 0 {
                    parts.push("scanning".to_owned());
                } else {
                    parts.push(format!(
                        "scanned {}/{}",
                        self.scanned_items, self.total_items
                    ));
                }
            }
        }
        if self.large_items > 0 {
            parts.push(format!("{} large item(s) not searched", self.large_items));
        }
        if self.failed_items > 0 {
            parts.push(format!("{} item(s) could not be read", self.failed_items));
        }
        if self.records_truncated {
            parts.push("turn records truncated".to_owned());
        }
        if self.stopped {
            parts.push("stopped early — coverage is partial".to_owned());
        }
        if self.truncated {
            parts.push(format!("first {MAX_SEARCH_MATCHES} matches shown"));
        }
        if self.complete {
            parts.push("complete".to_owned());
        }
        parts.join(" · ")
    }
}

/// Progress of the explicit full-session scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchStatus {
    /// The local (loaded) scan is running in its owned worker.
    ScanningLoaded,
    /// A pinned `session.read` scan is running.
    ScanningFull,
    /// The scan finished; `coverage.complete` says how far it got.
    Ready,
    /// The user stopped the scan.
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchPanelMode {
    /// The one-line query input at the bottom owns the keyboard.
    Input,
    /// The result list owns the keyboard; the query is editable with `/`.
    Results,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchPanelState {
    /// The session this panel searched. Switching sessions never retargets an
    /// open panel; the panel is closed with the search it belongs to.
    pub session_id: String,
    pub session_epoch: u64,
    pub query: String,
    pub query_cursor: usize,
    pub scope: SearchScope,
    pub mode: SearchPanelMode,
    pub matches: Vec<SearchMatch>,
    pub cursor: usize,
    pub coverage: SearchCoverage,
    pub status: SearchStatus,
    /// Generation of the scan that produced `matches`; a late result with an
    /// older generation never installs (spec §17.1).
    pub generation: u64,
    pub error: Option<String>,
}

impl Default for SearchPanelState {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            session_epoch: 0,
            query: String::new(),
            query_cursor: 0,
            scope: SearchScope::Loaded,
            mode: SearchPanelMode::Input,
            matches: Vec::new(),
            cursor: 0,
            coverage: SearchCoverage::default(),
            status: SearchStatus::Ready,
            generation: 0,
            error: None,
        }
    }
}

impl SearchPanelState {
    pub fn new(session_id: String, session_epoch: u64, query: String, scope: SearchScope) -> Self {
        let cursor = query.chars().count();
        Self {
            session_id,
            session_epoch,
            query,
            query_cursor: cursor,
            scope,
            mode: if cursor == 0 {
                SearchPanelMode::Input
            } else {
                SearchPanelMode::Results
            },
            ..Self::default()
        }
    }

    pub fn has_query(&self) -> bool {
        !self.query.trim().is_empty()
    }

    pub fn scanning(&self) -> bool {
        matches!(
            self.status,
            SearchStatus::ScanningLoaded | SearchStatus::ScanningFull
        )
    }

    /// Whether more matches may be appended (the cap also bounds memory).
    pub fn has_match_room(&self) -> bool {
        self.matches.len() < MAX_SEARCH_MATCHES
    }

    pub fn selected(&self) -> Option<&SearchMatch> {
        self.matches.get(self.cursor)
    }

    pub fn move_cursor(&mut self, delta: i32) {
        if self.matches.is_empty() {
            self.cursor = 0;
            return;
        }
        let len = self.matches.len() as i64;
        let next = (self.cursor as i64 + i64::from(delta)).clamp(0, len - 1);
        self.cursor = next as usize;
    }

    /// Next/previous match with wrap-around, used by `n`/`p`.
    pub fn step_cursor(&mut self, delta: i32) {
        if self.matches.is_empty() {
            return;
        }
        let len = self.matches.len() as i64;
        let next = (self.cursor as i64 + i64::from(delta)).rem_euclid(len);
        self.cursor = next as usize;
    }

    /// A one-line coverage/status statement for the panel.
    pub fn status_label(&self) -> String {
        if let Some(error) = &self.error {
            return error.clone();
        }
        if !self.has_query() {
            return "type a literal and press Enter".to_owned();
        }
        if self.matches.is_empty() {
            return match self.status {
                SearchStatus::ScanningLoaded | SearchStatus::ScanningFull => {
                    format!("searching {}", self.coverage.label(self.scope))
                }
                SearchStatus::Stopped => format!("stopped · {}", self.coverage.label(self.scope)),
                SearchStatus::Ready => {
                    if self.coverage.complete {
                        format!("no matches · {}", self.coverage.label(self.scope))
                    } else {
                        format!("no matches yet · {}", self.coverage.label(self.scope))
                    }
                }
            };
        }
        format!(
            "{} match(es) · {}",
            self.matches.len(),
            self.coverage.label(self.scope)
        )
    }
}

/// The loaded-content snapshot a scan walks: durable blocks are shared by
/// `Arc`, live text is copied and bounded by the running turn.
pub struct LoadedScanSnapshot {
    pub blocks: Arc<Vec<Arc<crate::state::transcript::TranscriptBlock>>>,
    pub live: Vec<crate::jobs::LiveScanText>,
    pub known_items: usize,
    pub total_items: usize,
}

/// One literal scan of already-loaded content. The needle is owned once and
/// every source is scanned in place; no scanned body is retained.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanSpec {
    pub needle: String,
    pub include_thinking: bool,
}

pub struct ScanPlan<'a> {
    pub needle: &'a str,
    pub include_thinking: bool,
    pub collector: ScanCollector,
}

#[derive(Default)]
pub struct ScanCollector {
    pub matches: Vec<SearchMatch>,
    pub truncated: bool,
}

impl<'a> ScanPlan<'a> {
    pub fn new(needle: &'a str, include_thinking: bool) -> Self {
        Self {
            needle,
            include_thinking,
            collector: ScanCollector::default(),
        }
    }

    pub fn matches(&self) -> &[SearchMatch] {
        &self.collector.matches
    }

    pub fn truncated(&self) -> bool {
        self.collector.truncated
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        index: Option<usize>,
        source: SearchSource,
        loop_id: Option<String>,
        request_index: Option<u32>,
        ordinal: u32,
        tool_call_id: Option<String>,
        text: &str,
    ) {
        for offset in literal_match_offsets(text, self.needle) {
            if self.collector.matches.len() >= MAX_SEARCH_MATCHES {
                self.collector.truncated = true;
                return;
            }
            self.collector.matches.push(SearchMatch {
                index,
                source,
                loop_id: loop_id.clone(),
                request_index,
                ordinal,
                tool_call_id: tool_call_id.clone(),
                preview: match_preview(text, offset, self.needle.len()),
                source_offset: offset,
                byte_range: offset..offset.saturating_add(self.needle.len()),
            });
        }
    }

    /// Scans one captured live-loop text piece.
    pub fn push_live(&mut self, text: &crate::jobs::LiveScanText) {
        self.push(
            text.index,
            text.source,
            text.loop_id.clone(),
            text.request_index,
            text.ordinal,
            text.tool_call_id.clone(),
            &text.text,
        );
    }

    /// Scans one durable block. Assistant part ordinals use the same
    /// per-kind counters as the rendered section inputs, so a match can be
    /// anchored to the exact section it came from.
    pub fn scan_block(&mut self, block: &crate::state::transcript::TranscriptBlock) {
        use crate::state::transcript::{AssistantPart, TranscriptBlock};
        match block {
            TranscriptBlock::User(user) => {
                let source = if user.kind == crate::protocol::UserMessageKindWire::Steering {
                    SearchSource::Steering
                } else {
                    SearchSource::Prompt
                };
                self.push(
                    user.index,
                    source,
                    user.loop_id.clone(),
                    None,
                    0,
                    None,
                    &user.text,
                );
            }
            TranscriptBlock::Assistant(assistant) => {
                let mut reasoning_ordinal = 0u32;
                let mut text_ordinal = 0u32;
                for part in &assistant.parts {
                    match part {
                        AssistantPart::Text(text) => {
                            self.push(
                                Some(assistant.index),
                                SearchSource::AssistantText,
                                Some(assistant.loop_id.clone()),
                                Some(assistant.request_index),
                                text_ordinal,
                                None,
                                text,
                            );
                            text_ordinal += 1;
                        }
                        AssistantPart::Reasoning(text) => {
                            if self.include_thinking {
                                self.push(
                                    Some(assistant.index),
                                    SearchSource::Thinking,
                                    Some(assistant.loop_id.clone()),
                                    Some(assistant.request_index),
                                    reasoning_ordinal,
                                    None,
                                    text,
                                );
                            }
                            reasoning_ordinal += 1;
                        }
                        AssistantPart::ToolCall(_) => {}
                    }
                }
            }
            TranscriptBlock::Tool(tool) => {
                self.push(
                    tool.index,
                    SearchSource::ToolName,
                    Some(tool.loop_id.clone()),
                    Some(tool.request_index),
                    0,
                    Some(tool.tool_call_id.clone()),
                    &tool.name,
                );
                if let Some(result) = tool.result.as_deref() {
                    self.push(
                        tool.index,
                        SearchSource::ToolResult,
                        Some(tool.loop_id.clone()),
                        Some(tool.request_index),
                        0,
                        Some(tool.tool_call_id.clone()),
                        result,
                    );
                }
            }
            TranscriptBlock::Summary(summary) => {
                self.push(
                    Some(summary.index),
                    SearchSource::Summary,
                    None,
                    None,
                    0,
                    None,
                    &summary.content,
                );
            }
            TranscriptBlock::HistoryPlaceholder(_) => {}
        }
    }

    /// Scans one decoded Runtime history item. Used by the owned decode worker
    /// so a full-session scan never parses or walks large text on the App
    /// thread.
    pub fn scan_item(&mut self, index: usize, item: &crate::protocol::read::RawHistoryItem) {
        use crate::protocol::read::{RuntimeAssistantPart, RuntimeItem, RuntimeUserKind};
        match &item.item {
            RuntimeItem::User(user) => {
                let source = match user.kind {
                    RuntimeUserKind::Prompt => SearchSource::Prompt,
                    RuntimeUserKind::Steering => SearchSource::Steering,
                };
                self.push(
                    Some(index),
                    source,
                    Some(user.loop_id.clone()),
                    None,
                    0,
                    None,
                    &user.input.text,
                );
            }
            RuntimeItem::Assistant(assistant) => {
                let mut reasoning_ordinal = 0u32;
                let mut text_ordinal = 0u32;
                for part in &assistant.content {
                    match part {
                        RuntimeAssistantPart::Text(text) => {
                            self.push(
                                Some(index),
                                SearchSource::AssistantText,
                                Some(assistant.loop_id.clone()),
                                Some(assistant.request_index),
                                text_ordinal,
                                None,
                                text,
                            );
                            text_ordinal += 1;
                        }
                        RuntimeAssistantPart::Reasoning { text, summary, .. } => {
                            if self.include_thinking {
                                if let Some(text) = text.as_deref().or(summary.as_deref()) {
                                    self.push(
                                        Some(index),
                                        SearchSource::Thinking,
                                        Some(assistant.loop_id.clone()),
                                        Some(assistant.request_index),
                                        reasoning_ordinal,
                                        None,
                                        text,
                                    );
                                }
                            }
                            reasoning_ordinal += 1;
                        }
                        RuntimeAssistantPart::ToolCall { .. } => {}
                    }
                }
            }
            RuntimeItem::ToolResult(result) => {
                self.push(
                    Some(index),
                    SearchSource::ToolName,
                    Some(result.loop_id.clone()),
                    Some(result.request_index),
                    0,
                    Some(result.call_id.clone()),
                    &result.tool_name,
                );
                self.push(
                    Some(index),
                    SearchSource::ToolResult,
                    Some(result.loop_id.clone()),
                    Some(result.request_index),
                    0,
                    Some(result.call_id.clone()),
                    &result.output.content,
                );
            }
            RuntimeItem::Summary(summary) => {
                self.push(
                    Some(index),
                    SearchSource::Summary,
                    None,
                    None,
                    0,
                    None,
                    &summary.content,
                );
            }
        }
    }
}

/// Case-insensitive literal match offsets in `hay`. ASCII needles use a
/// byte-window comparison without allocating; other needles compare lowercase
/// characters at char boundaries. Never indexes a non-boundary byte.
pub fn literal_match_offsets(hay: &str, needle: &str) -> Vec<usize> {
    if needle.is_empty() || hay.is_empty() {
        return Vec::new();
    }
    if needle.is_ascii() {
        let needle = needle.as_bytes();
        return hay
            .as_bytes()
            .windows(needle.len())
            .enumerate()
            .filter(|(offset, window)| {
                window.eq_ignore_ascii_case(needle) && hay.is_char_boundary(*offset)
            })
            .map(|(offset, _)| offset)
            .collect();
    }
    let mut out = Vec::new();
    for (offset, _) in hay.char_indices() {
        if case_insensitive_starts_with(&hay[offset..], needle) {
            out.push(offset);
        }
    }
    out
}

fn case_insensitive_starts_with(hay: &str, needle: &str) -> bool {
    let mut hay = hay.chars().flat_map(char::to_lowercase);
    for wanted in needle.chars().flat_map(char::to_lowercase) {
        match hay.next() {
            Some(found) if found == wanted => {}
            _ => return false,
        }
    }
    true
}

/// One bounded preview around `offset`, with the match itself included.
pub fn match_preview(hay: &str, offset: usize, length: usize) -> String {
    const AROUND: usize = 48;
    let start = floor_char_boundary(hay, offset.saturating_sub(AROUND));
    let end = ceil_char_boundary(hay, (offset + length + AROUND).min(hay.len()));
    let mut preview = String::new();
    if start > 0 {
        preview.push('…');
    }
    preview.push_str(&hay[start..end]);
    if end < hay.len() {
        preview.push('…');
    }
    preview
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

/// Runs one loaded-content scan to completion. This is the exact body the
/// owned worker executes on its blocking thread, shared so tests exercise the
/// production matcher instead of a copy.
pub fn run_local_scan(request: &crate::jobs::LocalScanRequest) -> crate::jobs::LocalScanOutcome {
    let mut plan = ScanPlan::new(&request.needle, request.include_thinking);
    for block in request.blocks.iter() {
        plan.scan_block(block);
    }
    for text in &request.live {
        plan.push_live(text);
    }
    crate::jobs::LocalScanOutcome {
        identity: request.identity.clone(),
        matches: plan.collector.matches,
        truncated: plan.collector.truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_and_unicode_literals_match_case_insensitively() {
        assert_eq!(
            literal_match_offsets("Hello world hello", "hello"),
            vec![0, 12]
        );
        // Case folding applies to non-ASCII too: both runs match.
        assert_eq!(literal_match_offsets("ÄÖÜ äöü", "äöü"), vec![0, 7]);
        // Non-ASCII haystack with an ASCII needle never indexes a boundary
        // inside a multi-byte character.
        assert_eq!(literal_match_offsets("日本語", "本"), vec![3]);
        assert_eq!(literal_match_offsets("日本語", "x"), Vec::<usize>::new());
        assert!(literal_match_offsets("abc", "").is_empty());
    }

    #[test]
    fn previews_stay_inside_char_boundaries() {
        let text = "é".repeat(100);
        let preview = match_preview(&text, 60, 2);
        assert!(preview.starts_with('…') && preview.ends_with('…'));
        assert!(preview.chars().all(|ch| ch == 'é' || ch == '…'));
    }

    #[test]
    fn coverage_never_claims_a_complete_scan_for_limited_runs() {
        let partial = SearchCoverage {
            loaded_items: 10,
            total_items: 40,
            scanned_items: 10,
            large_items: 2,
            stopped: true,
            ..SearchCoverage::default()
        };
        let label = partial.label(SearchScope::FullSession);
        assert!(label.contains("2 large item(s) not searched"), "{label}");
        assert!(label.contains("stopped early"), "{label}");
        assert!(!label.contains("complete"), "{label}");
        assert!(
            SearchCoverage::default()
                .label(SearchScope::Loaded)
                .contains("loaded content")
        );
    }
}
