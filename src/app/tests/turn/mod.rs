//! Turn control, streamed bodies and completion recovery.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use serde_json::{Value, json};

use super::harness::*;
use crate::app::{Action, NoticeLevel, Scroll, SessionOp};
use crate::protocol::read::UserKind;
use crate::protocol::stamp::{now_millis, unix_millis};
use crate::transcript::{Block, ChangeSummary, ToolState};

mod body;
mod control;
mod recovery;

fn turn_ends(h: &Harness) -> Vec<String> {
    h.blocks()
        .iter()
        .filter_map(|block| match block {
            Block::TurnEnd { loop_id, .. } => Some(loop_id.clone()),
            _ => None,
        })
        .collect()
}
