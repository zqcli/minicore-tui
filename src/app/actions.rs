//! User-visible operations. Every variant is a plain instruction: the reducer
//! decides whether it applies, and the `tasks` module performs the IO.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Enter: send when idle, steer when a turn runs.
    Submit,
    /// Alt+Enter: queue a follow-up.
    QueueFollowUp,
    /// Send this text as a prompt (palette/command paths).
    Send(String),
    /// Esc on a running turn.
    Interrupt,
    OpenSession(String),
    NewSession,
    ReloadSessions,
    DeleteSession(String),
    Rename(String),
    SetModel(String),
    SetReasoning(crate::protocol::Reasoning),
    SetTheme(crate::theme::ThemeKind),
    ToggleTheme,
    Compact,
    CopyLastAnswer,
    Export(Option<String>),
    EditDraft,
    ReloadAgent,
    Answer(Answer),
    LoadOlder,
    RecallQueued,
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Allow,
    Deny,
    Choice(usize),
    Text(String),
}
