//! On-demand Host directory browsing. Picking a file inserts only a text reference.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use minicore_host_protocol::{DirectoryEntry, DirectoryListRequest, DirectoryListResult};

use super::{App, Msg, Overlay, Session, tasks};

#[derive(Debug, Default)]
pub struct FileBrowser {
    pub directory: String,
    pub filter: String,
    pub entries: Vec<DirectoryEntry>,
    pub pending: Option<(u64, String)>,
    pub error: Option<String>,
    draft_version: u64,
    cursor: usize,
}

impl FileBrowser {
    pub fn rows(&self) -> Vec<&DirectoryEntry> {
        let query = self.filter.to_lowercase();
        self.entries
            .iter()
            .filter(|entry| entry.name.to_lowercase().contains(&query))
            .collect()
    }
}

/// Paths are references, not shell arguments or attachments. Quote whitespace
/// and escapes so a path with spaces remains visibly one reference.
pub(crate) fn path_reference(path: &Path, workspace: &Path) -> String {
    let path = path
        .strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy();
    if path
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control() || matches!(ch, '"' | '\\'))
    {
        format!(
            "@{}",
            serde_json::to_string(path.as_ref()).expect("path string")
        )
    } else {
        format!("@{path}")
    }
}

impl App {
    fn reference_workspace(&self) -> &Path {
        self.resident
            .current()
            .and_then(|session| session.info.config.workspace.as_ref())
            .map_or(self.workspace.as_path(), |workspace| {
                Path::new(&workspace.path)
            })
    }

    pub(super) fn open_files(&mut self) {
        self.composer.place_cursor(self.composer.cursor(), false);
        let directory = self.reference_workspace().to_string_lossy().into_owned();
        self.overlay = Overlay::Files {
            selected: 0,
            browser: FileBrowser {
                directory: directory.clone(),
                draft_version: self.composer.version(),
                cursor: self.composer.cursor(),
                ..FileBrowser::default()
            },
        };
        self.browse_files(directory);
    }

    fn browse_files(&mut self, path: String) {
        let request = self.next_operation();
        let Overlay::Files { browser, .. } = &mut self.overlay else {
            return;
        };
        browser.pending = Some((request, browser.filter.clone()));
        browser.error = None;
        let client = self.client.clone();
        let workspace_session = self.resident.current().map(Session::address);
        self.spawn_msg(async move {
            let result = async {
                let mut entries = Vec::new();
                let mut cursor = None;
                loop {
                    let mut page = client
                        .workspace_list(
                            DirectoryListRequest {
                                path: path.clone(),
                                hidden: false,
                                include_files: true,
                                workspace_session: workspace_session.clone(),
                                cursor,
                                limit: 500,
                            },
                            tasks::options(),
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    entries.append(&mut page.entries);
                    cursor = page.next_cursor.take();
                    if cursor.is_none() {
                        page.entries = entries;
                        break Ok(page);
                    }
                }
            }
            .await;
            Msg::Files(request, result)
        });
    }

    pub(super) fn on_files(&mut self, request: u64, result: Result<DirectoryListResult, String>) {
        let Overlay::Files { selected, browser } = &mut self.overlay else {
            return;
        };
        if browser
            .pending
            .as_ref()
            .is_none_or(|(id, _)| *id != request)
        {
            return;
        }
        let (_, query) = browser.pending.take().unwrap();
        match result {
            Err(error) => browser.error = Some(error),
            Ok(result) if result.is_file => {
                // The user may have corrected the path while its lookup was pending.
                if browser.filter == query {
                    self.pick_file(result.path);
                }
            }
            Ok(mut result) => {
                result
                    .entries
                    .sort_by(|a, b| (a.is_file, &a.name).cmp(&(b.is_file, &b.name)));
                if let Some(parent) = result.parent {
                    result.entries.insert(
                        0,
                        DirectoryEntry {
                            name: "..".into(),
                            path: parent,
                            is_file: false,
                        },
                    );
                }
                browser.directory = result.path;
                browser.entries = result.entries;
                if browser.filter == query {
                    browser.filter.clear();
                }
                *selected = 0;
            }
        }
    }

    pub(super) fn edit_file_filter(&mut self, text: &str) {
        let Overlay::Files { selected, browser } = &mut self.overlay else {
            return;
        };
        let text = text.replace(['\r', '\n'], " ");
        browser.filter.push_str(&crate::wrap::sanitize(&text));
        browser.error = None;
        *selected = 0;
    }

    pub(super) fn on_files_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Up => self.overlay_move(-1),
            KeyCode::Down => self.overlay_move(1),
            KeyCode::PageUp => self.overlay_move(-8),
            KeyCode::PageDown => self.overlay_move(8),
            KeyCode::Enter | KeyCode::Tab if key.modifiers.is_empty() => self.accept_file(),
            KeyCode::Char(ch) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.edit_file_filter(&ch.to_string())
            }
            KeyCode::Backspace | KeyCode::Char('u') if !ctrl || key.code == KeyCode::Char('u') => {
                if let Overlay::Files { selected, browser } = &mut self.overlay {
                    if ctrl {
                        browser.filter.clear();
                    } else {
                        browser.filter.pop();
                    }
                    browser.error = None;
                    *selected = 0;
                }
            }
            _ => {}
        }
    }

    pub(super) fn accept_file(&mut self) {
        let Overlay::Files { selected, browser } = &self.overlay else {
            return;
        };
        if browser.pending.is_some() {
            return;
        }
        if Path::new(&browser.filter).is_absolute() {
            self.browse_files(browser.filter.clone());
            return;
        }
        let Some(entry) = browser.rows().get(*selected).copied() else {
            return;
        };
        let path = entry.path.clone();
        if entry.is_file {
            self.pick_file(path);
        } else {
            self.browse_files(path);
        }
    }

    fn pick_file(&mut self, path: String) {
        let Overlay::Files { browser, .. } = std::mem::replace(&mut self.overlay, Overlay::None)
        else {
            return;
        };
        if self.composer.version() != browser.draft_version
            || self.composer.cursor() != browser.cursor
        {
            return;
        }
        if self.composer.text()[..self.composer.cursor()].ends_with('@') {
            self.composer.backspace();
        }
        self.insert_path_references(&[PathBuf::from(path)]);
    }

    /// Shared by the chooser and explicit clipboard file/image paste. This
    /// neither reads a file nor grants Host access to its location.
    pub(crate) fn insert_path_references(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let mut text = paths
            .iter()
            .map(|path| path_reference(path, self.reference_workspace()))
            .collect::<Vec<_>>()
            .join(" ");
        let start = self
            .composer
            .selection()
            .map_or(self.composer.cursor(), |range| range.start);
        if self.composer.text()[..start]
            .chars()
            .next_back()
            .is_some_and(|ch| !ch.is_whitespace())
        {
            text.insert(0, ' ');
        }
        text.push(' ');
        self.composer.insert(&text);
    }
}
