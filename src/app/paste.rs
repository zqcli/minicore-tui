//! Explicit native clipboard reads stay attached to the editor that requested them.

use std::ops::Range;

use minicore_host_protocol::SessionRef;

use super::{App, Msg, NoticeLevel, Overlay, Session};
use crate::clipboard::PastePayload;

#[derive(Debug, PartialEq, Eq)]
pub enum PasteTarget {
    Composer {
        session: Option<SessionRef>,
        version: u64,
        cursor: usize,
        selection: Option<Range<usize>>,
    },
    Files {
        id: u64,
        filter: String,
    },
}

impl App {
    pub(super) fn clipboard_target(&self) -> Option<PasteTarget> {
        match &self.overlay {
            Overlay::None | Overlay::Palette { .. }
                if !self.has_interaction() && self.fatal.is_none() =>
            {
                Some(PasteTarget::Composer {
                    session: self.resident.current().map(Session::address),
                    version: self.composer.version(),
                    cursor: self.composer.cursor(),
                    selection: self.composer.selection(),
                })
            }
            Overlay::Files { browser, .. } => Some(PasteTarget::Files {
                id: browser.id,
                filter: browser.filter.clone(),
            }),
            _ => None,
        }
    }

    pub(super) fn paste_clipboard(&mut self) {
        let Some(target) = self.clipboard_target() else {
            return;
        };
        if matches!(target, PasteTarget::Composer { .. }) {
            self.composer
                .place_cursor(self.composer.cursor(), self.composer.selection().is_some());
        }
        self.spawn_msg(
            async move { Msg::ClipboardPasted(target, crate::clipboard::paste().await) },
        );
    }

    pub(super) fn on_clipboard_pasted(
        &mut self,
        target: PasteTarget,
        result: Result<PastePayload, String>,
    ) {
        // Do not paste into a different Session, draft, cursor or reopened picker.
        if self.clipboard_target().as_ref() != Some(&target) {
            return;
        }
        match result {
            Err(error) => self.notice(NoticeLevel::Error, error),
            Ok(PastePayload::Text(text)) => self.on_paste(&text),
            Ok(PastePayload::Paths(paths)) => match target {
                PasteTarget::Composer { .. } => {
                    self.overlay = Overlay::None;
                    self.insert_path_references(&paths);
                }
                PasteTarget::Files { .. } if paths.len() == 1 => {
                    self.edit_file_filter(&paths[0].to_string_lossy())
                }
                PasteTarget::Files { .. } => self.notice(
                    NoticeLevel::Usage,
                    "Paste one path here, or close the browser to paste multiple files",
                ),
            },
        }
    }
}
