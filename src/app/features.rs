//! Feature logic that belongs to the reducer but is not part of the core turn
//! flow: session auto-titles, find-in-transcript, text selection, the picker
//! row builders and the interactive prompt's key handling.

use crate::protocol::{SessionInfo, ToolRef};
use crate::transcript::Block;

use super::Overlay;
use super::commands;
use super::session::InteractionKind;

/// Session titles come from the first prompt so the picker is readable: the
/// first meaningful line, cut at a word boundary near 60 cells.
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
    if title.is_empty() {
        return Some(first.chars().take(60).collect());
    }
    Some(if title == first {
        title
    } else {
        format!("{title}…")
    })
}

/// A session is titled once, after its first turn is accepted.
pub fn needs_auto_title(info: &SessionInfo) -> bool {
    info.title
        .as_deref()
        .is_none_or(|title| title.trim().is_empty())
}

/// The palette entries, as `(name, description)` for the help view.
pub fn command_rows() -> Vec<(&'static str, &'static str)> {
    commands::COMMANDS
        .iter()
        .map(|command| (command.name, command.description))
        .collect()
}

/// One transcript match: the block it is in and the case-insensitive byte range
/// of its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FindMatch {
    pub block: usize,
    pub start: usize,
    pub len: usize,
}

/// The searchable text of one block, if it has any.
fn searchable(block: &Block) -> Option<&str> {
    match block {
        Block::User { text, .. } => Some(text),
        Block::Assistant { text, .. } => Some(text),
        Block::Notice { text, .. } => Some(text),
        Block::Tool(call) => Some(&call.detail),
        Block::Compaction { summary, .. } => summary.as_deref(),
        Block::TurnEnd { .. } | Block::EarlierHistory { .. } => None,
    }
}

/// Case-insensitive literal occurrences in block text. Each block yields at
/// most one match, so repeated navigation moves through the conversation
/// instead of stopping on every occurrence inside a long answer.
pub fn find_matches(blocks: &[Block], query: &str) -> Vec<FindMatch> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let needle = query.to_lowercase();
    let mut matches = Vec::new();
    for (block, content) in blocks.iter().enumerate() {
        let Some(text) = searchable(content) else {
            continue;
        };
        if let Some(start) = text.to_lowercase().find(&needle) {
            matches.push(FindMatch {
                block,
                start,
                len: query.len(),
            });
        }
    }
    matches
}

/// Matches in a block slice; the same as [`find_matches`] for callers that
/// already hold the blocks and cannot re-borrow the session.
pub fn find_matches_in(blocks: &[Block], query: &str) -> Vec<FindMatch> {
    find_matches(blocks, query)
}

/// Advance through the matches, wrapping at either end.
pub fn step_match(current: usize, total: usize, forward: bool) -> usize {
    if total == 0 {
        return 0;
    }
    if forward {
        (current + 1) % total
    } else {
        (current + total - 1) % total
    }
}

/// The tool call under a pointer row, for click-to-expand and click-to-open.
pub fn tool_at(overlay: &Overlay) -> Option<&ToolRef> {
    match overlay {
        Overlay::Tool(key) => Some(key),
        _ => None,
    }
}

/// Rows a pending interaction offers: index 0 is "allow" for approvals.
pub fn interaction_options(kind: &InteractionKind) -> Vec<String> {
    match kind {
        InteractionKind::Approval => vec!["Allow once".into(), "Deny".into()],
        InteractionKind::Choice(choices) => choices.clone(),
    }
}

/// What Ctrl+D in the sessions picker should do, decided from the row and the
/// armed session so the key handler stays a thin shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteStep {
    /// Do nothing; there is no row.
    Ignore,
    /// The session is the one running in this TUI.
    Resident,
    /// First press: remember the id and ask again.
    Arm,
    /// Second press on the same id: delete it.
    Delete,
}

pub fn delete_step(armed: Option<&str>, target: &str, resident: bool) -> DeleteStep {
    if resident {
        return DeleteStep::Resident;
    }
    if armed == Some(target) {
        DeleteStep::Delete
    } else {
        DeleteStep::Arm
    }
}

/// The default `/export` path: `./minicore-<session>-<date>.md`, in the current
/// directory so the file lands where the user is working.
pub fn export_path(session_id: &str) -> String {
    let short = session_id.strip_prefix("ses_").unwrap_or(session_id);
    format!("minicore-{short}-{}.md", commands::today())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::read::UserKind;

    fn user(text: &str) -> Block {
        Block::User {
            text: text.into(),
            kind: UserKind::Prompt,
        }
    }

    fn assistant(text: &str) -> Block {
        Block::Assistant {
            key: crate::transcript::RequestKey {
                loop_id: "l".into(),
                request_index: 0,
            },
            reasoning: String::new(),
            text: text.into(),
            streaming: false,
        }
    }

    #[test]
    fn titles_take_the_first_meaningful_line() {
        assert_eq!(
            auto_title("\n\n# Fix the parser\nand more"),
            Some("Fix the parser".into())
        );
        assert_eq!(auto_title("   \n \n"), None);
        assert_eq!(auto_title(""), None);
    }

    #[test]
    fn long_titles_cut_at_a_word_boundary() {
        let prompt = "Refactor the parser so that it can recover from a missing brace and report the exact line";
        let title = auto_title(prompt).unwrap();
        assert!(
            unicode_width::UnicodeWidthStr::width(title.as_str()) <= 61,
            "{title:?}"
        );
        assert!(title.ends_with('…'), "{title:?}");
        assert!(title.starts_with("Refactor the parser"), "{title:?}");
    }

    #[test]
    fn an_unbreakable_first_line_is_cut_at_sixty_characters() {
        assert_eq!(auto_title(&"a".repeat(200)).unwrap().chars().count(), 60);
    }

    #[test]
    fn a_session_with_a_title_is_not_retitled() {
        let mut info = SessionInfo {
            session_id: "s".into(),
            title: Some("named".into()),
            profile: String::new(),
            workspace: String::new(),
            model: String::new(),
            reasoning: crate::protocol::Reasoning::Medium,
            loaded: true,
            created_at: String::new(),
            updated_at: String::new(),
        };
        assert!(!needs_auto_title(&info));
        info.title = None;
        assert!(needs_auto_title(&info));
        info.title = Some("   ".into());
        assert!(needs_auto_title(&info));
    }

    #[test]
    fn find_matches_are_case_insensitive_and_block_scoped() {
        let blocks = [
            user("Fix the parser"),
            assistant("The parser now works"),
            user("next"),
        ];
        let matches = find_matches(&blocks, "PARSER");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].block, 0);
        assert_eq!(matches[0].start, 8);
        assert_eq!(matches[0].len, 6);
        assert_eq!(matches[1].block, 1);
    }

    #[test]
    fn one_match_per_block_keeps_navigation_moving() {
        let blocks = [assistant("tool tool tool")];
        assert_eq!(find_matches(&blocks, "tool").len(), 1);
    }

    #[test]
    fn an_empty_query_matches_nothing() {
        let blocks = [user("text")];
        assert!(find_matches(&blocks, "").is_empty());
        assert!(find_matches(&blocks, "   ").is_empty());
    }

    #[test]
    fn matches_wrap_in_both_directions() {
        assert_eq!(step_match(0, 3, true), 1);
        assert_eq!(step_match(2, 3, true), 0);
        assert_eq!(step_match(0, 3, false), 2);
        assert_eq!(step_match(0, 0, true), 0);
    }

    #[test]
    fn notices_and_tool_details_are_searchable() {
        let blocks = [
            Block::Notice {
                level: crate::transcript::NoticeLevel::Error,
                text: "compaction failed".into(),
            },
            Block::Tool(Box::new(crate::transcript::ToolCall::new(
                ToolRef {
                    session_id: "s".into(),
                    loop_id: "l".into(),
                    request_index: 0,
                    tool_call_id: "c".into(),
                },
                "bash",
            ))),
        ];
        assert_eq!(find_matches(&blocks, "compaction").len(), 1);
    }

    #[test]
    fn the_export_path_names_the_session_and_the_day() {
        let path = export_path("ses_abc123");
        assert!(path.starts_with("minicore-abc123-"), "{path}");
        assert!(path.ends_with(".md"), "{path}");
    }

    #[test]
    fn deleting_a_session_takes_two_presses_and_never_the_resident_one() {
        assert_eq!(delete_step(None, "a", true), DeleteStep::Resident);
        assert_eq!(
            delete_step(Some("a"), "a", true),
            DeleteStep::Resident,
            "a second press must not delete the running session"
        );
        assert_eq!(delete_step(None, "a", false), DeleteStep::Arm);
        assert_eq!(delete_step(Some("b"), "a", false), DeleteStep::Arm);
        assert_eq!(delete_step(Some("a"), "a", false), DeleteStep::Delete);
    }

    #[test]
    fn approvals_offer_allow_then_deny() {
        assert_eq!(
            interaction_options(&InteractionKind::Approval),
            vec!["Allow once".to_owned(), "Deny".to_owned()]
        );
        assert_eq!(
            interaction_options(&InteractionKind::Choice(vec!["a".into()])),
            vec!["a".to_owned()]
        );
    }
}
