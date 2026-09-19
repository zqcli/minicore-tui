//! The single main-area detail and explicit keyboard focus. No page stack.
use crate::{
    protocol::ToolDataStreamWire as Stream,
    state::tool::{StreamView, ToolKey},
};
use std::{
    ops::Range,
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

#[derive(Debug, Default)]
pub enum MainView {
    #[default]
    Conversation,
    ToolDetail(Box<ToolDetailState>),
    FilePreview(Box<crate::state::workspace::FilePreviewState>),
    Changes(Box<crate::state::changes::ChangesState>),
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Main,
    #[default]
    Editor,
    Dock,
    Search,
    Confirmation,
}

#[derive(Debug)]
pub struct ToolDetailState {
    pub key: ToolKey,
    pub conversation_scroll: Option<crate::state::session::ScrollState>,
    pub epoch: u64,
    pub generation: u64,
    pub tab: Stream,
    pub read: bool,
    pub initialized: bool,
    pub due: Option<Instant>,
    pub last_query: Option<Instant>,
    pub error: Option<String>,
    pub streams: [StreamView; 4],
    pub scroll: [usize; 4],
    pub follow: [bool; 4],
    pub scrollbar_grab: Option<usize>,
    pub layout: Option<ToolTextLayout>,
    pub layout_pending: Option<ToolLayoutIdentity>,
}
impl ToolDetailState {
    pub fn new(key: ToolKey, epoch: u64, generation: u64, now: Instant) -> Self {
        Self {
            key,
            conversation_scroll: None,
            epoch,
            generation,
            tab: Stream::Output,
            read: false,
            initialized: false,
            due: Some(now),
            last_query: None,
            error: None,
            streams: [
                StreamView::new(Stream::Input),
                StreamView::new(Stream::Output),
                StreamView::new(Stream::Stdout),
                StreamView::new(Stream::Stderr),
            ],
            scroll: [0; 4],
            follow: [true; 4],
            scrollbar_grab: None,
            layout: None,
            layout_pending: None,
        }
    }
    pub fn stream(&self) -> &StreamView {
        &self.streams[self.tab.index()]
    }
    pub fn offset(&self, height: usize) -> usize {
        let max = self
            .layout
            .as_ref()
            .map_or(0, |layout| layout.rows.len())
            .saturating_sub(height);
        if self.follow[self.tab.index()] {
            max
        } else {
            self.scroll[self.tab.index()].min(max)
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolLayoutIdentity {
    pub generation: u64,
    pub stream: Stream,
    pub revision: u64,
    pub width: u16,
}
pub struct ToolLayoutRequest {
    pub identity: ToolLayoutIdentity,
    pub stream: StreamView,
    pub cancel: Arc<AtomicBool>,
}
pub struct ToolTextLayout {
    pub identity: ToolLayoutIdentity,
    pub text: Arc<str>,
    pub rows: Vec<Range<usize>>,
}
impl std::fmt::Debug for ToolTextLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolTextLayout")
            .field("identity", &self.identity)
            .field("text_bytes", &self.text.len())
            .field("rows", &self.rows.len())
            .finish()
    }
}
impl ToolTextLayout {
    pub fn retained_bytes(&self) -> usize {
        self.text.len() + self.rows.capacity() * std::mem::size_of::<Range<usize>>()
    }
    /// Runs on the existing serialized layout worker. Index ranges rather than
    /// one allocated styled Line per row keep a million-newline stream bounded.
    pub fn build(request: ToolLayoutRequest) -> Option<Self> {
        use std::sync::atomic::Ordering;
        use unicode_segmentation::UnicodeSegmentation;
        use unicode_width::UnicodeWidthStr;
        let text = request.stream.display_text().replace('\t', "    ");
        let mut rows = Vec::new();
        let mut start = 0;
        let mut cells = 0;
        let width = usize::from(request.identity.width).max(1);
        for (offset, grapheme) in text.grapheme_indices(true) {
            if request.cancel.load(Ordering::Relaxed) {
                return None;
            }
            if grapheme == "\n" {
                rows.push(start..offset);
                start = offset + 1;
                cells = 0;
            } else {
                let size = grapheme.width();
                if cells + size > width && offset > start {
                    rows.push(start..offset);
                    start = offset;
                    cells = 0;
                }
                cells += size;
            }
        }
        rows.push(start..text.len());
        Some(Self {
            identity: request.identity,
            text: Arc::from(text),
            rows,
        })
    }
}
