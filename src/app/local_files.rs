//! Serial, coalesced writes for local preferences and accepted prompt history.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::{Msg, NoticeLevel, Tx};
use crate::{config::ThemeKind, prompt_history};

#[derive(Default)]
struct Pending {
    history: Option<(PathBuf, Vec<prompt_history::Record>)>,
    theme: Option<(PathBuf, ThemeKind)>,
    reload: Option<(Option<crate::config::Home>, bool)>,
    closing: bool,
}

#[derive(Default)]
pub struct LocalFiles {
    pending: Arc<Mutex<Pending>>,
    notify: Arc<Notify>,
    task: Option<JoinHandle<()>>,
}

impl LocalFiles {
    pub fn history(&mut self, tx: &Tx, path: PathBuf, records: Vec<prompt_history::Record>) {
        self.pending.lock().unwrap().history = Some((path, records));
        self.wake(tx);
    }

    pub fn theme(&mut self, tx: &Tx, path: PathBuf, theme: ThemeKind) {
        self.pending.lock().unwrap().theme = Some((path, theme));
        self.wake(tx);
    }

    pub fn reload(&mut self, tx: &Tx, home: Option<crate::config::Home>, composed: bool) {
        self.pending.lock().unwrap().reload = Some((home, composed));
        self.wake(tx);
    }

    fn wake(&mut self, tx: &Tx) {
        if self.task.is_none() {
            let (pending, notify, tx) = (self.pending.clone(), self.notify.clone(), tx.clone());
            self.task = Some(tokio::spawn(async move {
                loop {
                    notify.notified().await;
                    let work = std::mem::take(&mut *pending.lock().unwrap());
                    let closing = work.closing;
                    let result = tokio::task::spawn_blocking(move || {
                        if let Some((path, records)) = work.history {
                            // Recall history remains best effort, as on the synchronous path.
                            let _ = prompt_history::save(&path, &records);
                        }
                        let theme = work.theme.map_or(Ok(()), |(path, theme)| {
                            crate::config::save_theme(&path, theme)
                        });
                        let reload = work.reload.map(|(home, composed)| {
                            let templates = super::templates::load(home.as_ref());
                            let result =
                                home.as_ref().filter(|_| composed).map_or(Ok(()), |home| {
                                    crate::config::load(home)
                                        .and_then(|loaded| loaded.write(home))
                                        .map(drop)
                                        .map_err(|error| error.to_string())
                                });
                            (templates, result)
                        });
                        (theme, reload)
                    })
                    .await;
                    match result {
                        Ok((theme, reload)) => {
                            if let Err(message) = theme {
                                let _ = tx.send(Msg::Notice(NoticeLevel::Error, message));
                            }
                            if let Some((templates, result)) = reload {
                                let _ = tx.send(Msg::Configured(templates, result));
                            }
                        }
                        Err(error) => {
                            let _ = tx.send(Msg::Notice(NoticeLevel::Error, error.to_string()));
                        }
                    }
                    if closing {
                        break;
                    }
                }
            }));
        }
        self.notify.notify_one();
    }

    pub async fn finish(&mut self) {
        if let Some(task) = &mut self.task {
            self.pending.lock().unwrap().closing = true;
            self.notify.notify_one();
            let _ = task.await;
            self.task = None;
        }
    }
}

impl Drop for LocalFiles {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn finishing_flushes_the_latest_accepted_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let mut files = LocalFiles::default();
        for text in ["first", "latest"] {
            files.history(
                &tx,
                path.clone(),
                vec![prompt_history::Record {
                    text: text.into(),
                    pastes: Vec::new(),
                }],
            );
        }
        files.finish().await;
        assert_eq!(prompt_history::load(&path)[0].text, "latest");
    }

    #[tokio::test]
    async fn cancelling_a_flush_keeps_the_worker_owned_until_drop() {
        let (done, ended) = tokio::sync::oneshot::channel::<()>();
        let mut files = LocalFiles {
            task: Some(tokio::spawn(async move {
                let _done = done;
                std::future::pending::<()>().await;
            })),
            pending: Arc::default(),
            notify: Arc::default(),
        };
        {
            let mut finishing = std::pin::pin!(files.finish());
            assert!(futures_util::poll!(finishing.as_mut()).is_pending());
        }
        drop(files);
        assert!(crate::testing::real_bounded(ended).await.is_err());
    }
}
