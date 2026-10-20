//! The prompt history file: one JSON record per line, written atomically.

use std::ops::Range;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A prompt larger than this stays in memory only, so one huge paste cannot
/// bloat a file that is rewritten on every submit.
const MAX_RECORD_BYTES: usize = 32 << 10;

/// A prompt and the byte ranges of its paste chips.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pastes: Vec<Range<usize>>,
}

/// Reads the records of `path`; a missing file or a damaged line gives none.
pub fn load(path: &Path) -> Vec<Record> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Replaces `path` with `records` through a temporary file, so a crash leaves
/// either the old history or the new one.
pub fn save(path: &Path, records: &[Record]) -> std::io::Result<()> {
    let mut out = String::new();
    for record in records.iter().filter(|r| r.text.len() <= MAX_RECORD_BYTES) {
        out.push_str(&serde_json::to_string(record).map_err(std::io::Error::other)?);
        out.push('\n');
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, out)?;
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(text: &str) -> Record {
        Record {
            text: text.into(),
            pastes: Vec::new(),
        }
    }

    #[test]
    fn records_round_trip_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state").join("history.jsonl");
        let records = vec![
            record("one\ntwo \"quoted\""),
            Record {
                text: "see pasted".into(),
                pastes: vec![0..3, 4..10],
            },
        ];
        save(&path, &records).unwrap();
        assert_eq!(load(&path), records);
        assert!(!path.with_extension("tmp").exists(), "no temp file is left");
    }

    #[test]
    fn a_missing_file_and_a_damaged_line_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        assert!(load(&path).is_empty());
        std::fs::write(
            &path,
            "{\"text\":\"kept\"}\nnot json\n{\"text\":\"also\"}\n",
        )
        .unwrap();
        assert_eq!(load(&path), vec![record("kept"), record("also")]);
    }

    #[test]
    fn a_huge_prompt_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let huge = "x".repeat(MAX_RECORD_BYTES + 1);
        save(&path, &[record("small"), record(&huge)]).unwrap();
        assert_eq!(load(&path), vec![record("small")]);
    }
}
