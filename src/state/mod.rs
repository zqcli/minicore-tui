//! Pure app state (development spec 12). These modules hold data and tiny
//! helpers only: every mutation happens inside `App::update` (`src/app.rs`),
//! which is the single entry point for state changes.

pub mod catalog;
pub mod composer;
pub mod session;
pub mod tool;
pub mod transcript;
pub mod turn;

pub use catalog::CatalogState;
pub use composer::Composer;
pub use session::{ScrollState, SessionId, SessionView, SessionsState};
pub use tool::{LiveTool, ToolStatus};
pub use transcript::{
    AssistantBlock, AssistantPart, SummaryBlock, TerminalBlock, ToolBlock, TranscriptBlock,
    TranscriptRenderCache, TranscriptState, UserBlock,
};
pub use turn::{LiveTurn, LocalSubmissionId};
