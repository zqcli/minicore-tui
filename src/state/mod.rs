//! Pure app state (development spec r2). These modules hold data and tiny
//! helpers only: every mutation happens inside `App::update` (`src/app.rs`),
//! which is the single entry point for state changes.

pub mod catalog;
pub mod composer;
pub mod selection;
pub mod session;
pub mod tool;
pub mod transcript;
pub mod turn;

pub use catalog::CatalogState;
pub use composer::Composer;
pub use selection::{
    Dock, NewSessionField, NewSessionState, SELECTOR_PAGE, SelectorKind, SelectorState,
};
pub use session::{
    ConfigUpdateState, PendingConfigUpdate, ScrollState, SessionId, SessionView, SessionsState,
};
pub use tool::{LiveTool, ToolStatus};
pub use transcript::{
    AssistantBlock, AssistantPart, PreparedTranscriptCache, SummaryBlock, ToolBlock, ToolExpansion,
    TranscriptBlock, TranscriptCacheKey, TranscriptRenderCache, TranscriptState, UserBlock,
};
pub use turn::{
    LiveLoop, LiveRequest, LocalSubmissionId, PendingSteer, PendingSteerState, UnsavedLoop,
};
