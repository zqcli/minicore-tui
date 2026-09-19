//! Pure app state (development spec r2). These modules hold data and tiny
//! helpers only: every mutation happens inside `App::update` (`src/app.rs`),
//! which is the single entry point for state changes.

pub mod catalog;
pub mod composer;
pub mod export;
pub mod search;
pub mod selection;
pub mod session;
pub mod settings;
pub mod tool;
pub mod transcript;
pub mod turn;
pub mod view;

pub use catalog::CatalogState;
pub use composer::{Composer, PasteRange};
pub use export::{ExportFormState, ExportLimitations, ExportPhase, ExportSpec};
pub use selection::{
    Dock, NewSessionField, NewSessionState, SelectorKind, SelectorState, SessionConfirmChoice,
    SessionPanelAction, SessionPanelMode, SessionSelectorState,
};
pub use session::{
    ConfigUpdateState, ManualCompactState, PendingConfigUpdate, ScrollState, SessionId,
    SessionView, SessionsState,
};
pub use settings::{SettingsField, SettingsState};
pub use tool::{LiveTool, ToolConflict, ToolFacts, ToolKey, ToolPresentationState, ToolStatus};
pub use transcript::{
    AssistantBlock, AssistantPart, HistoryPlaceholderBlock, SummaryBlock, ToolBlock, ToolExpansion,
    TranscriptBlock, TranscriptState, UserBlock,
};
pub use turn::{
    AppliedSteer, LiveLoop, LivePart, LiveRequest, LocalSubmissionId, OperationRef, PendingSteer,
    PendingSteerState, SteerQueueItem, SteerQueueState, SteerReceiptObserved, Submission,
    UnsavedLoop,
};
pub use view::{
    ConversationLayout, ConversationSelection, CopyIndex, CopyRange, CopyView, DurableCacheKey,
    FoldOverride, LayoutKey, PreparedConversation, PreparedDurable, ReasoningKey, SectionId,
    SectionIndex, SectionKind, SectionLayout, SectionRange, SectionView, SelectionGranularity,
    SelectionPoint,
};
