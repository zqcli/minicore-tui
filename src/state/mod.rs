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
pub mod view;

pub use catalog::CatalogState;
pub use composer::{Composer, PasteRange};
pub use selection::{
    Dock, NewSessionField, NewSessionState, SELECTOR_PAGE, SelectorKind, SelectorState,
};
pub use session::{
    ConfigUpdateState, PendingConfigUpdate, ScrollState, SessionId, SessionView, SessionsState,
};
pub use tool::{LiveTool, ToolKey, ToolPresentationState, ToolStatus};
pub use transcript::{
    AssistantBlock, AssistantPart, SummaryBlock, ToolBlock, ToolExpansion, TranscriptBlock,
    TranscriptState, UserBlock,
};
pub use turn::{
    AppliedSteer, LiveLoop, LivePart, LiveRequest, LocalSubmissionId, PendingSteer,
    PendingSteerState, SteerQueueItem, SteerQueueState, SteerReceiptObserved, UnsavedLoop,
};
pub use view::{
    ConversationSelection, CopyRange, FoldOverride, PreparedConversation, ReasoningKey, SectionId,
    SectionKind, SectionRange, SelectionGranularity, SelectionPoint,
};
