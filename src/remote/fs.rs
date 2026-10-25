use std::collections::BinaryHeap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Root {
    pub path: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Entry {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Listing {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FsError {
    #[error("Path is outside the configured roots")]
    OutsideRoots,
    #[error("Path was not found")]
    NotFound,
    #[error("Path is not a directory")]
    NotADirectory,
    #[error("Invalid directory name")]
    InvalidName,
    #[error("An entry with that name already exists")]
    Exists,
    #[error("Filesystem error: {0}")]
    Io(String),
}

impl FsError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::OutsideRoots => "outside_roots",
            Self::NotFound => "not_found",
            Self::NotADirectory => "not_a_directory",
            Self::InvalidName => "invalid_name",
            Self::Exists => "exists",
            Self::Io(_) => "io",
        }
    }
}

impl From<io::Error> for FsError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::NotFound => Self::NotFound,
            io::ErrorKind::NotADirectory => Self::NotADirectory,
            io::ErrorKind::AlreadyExists => Self::Exists,
            _ => Self::Io(error.to_string()),
        }
    }
}

pub const MAX_ENTRIES: usize = 1000;

pub fn roots(roots: &[PathBuf]) -> Vec<Root> {
    roots
        .iter()
        .map(|path| Root {
            path: display(path),
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| display(path)),
        })
        .collect()
}

pub fn list(roots: &[PathBuf], path: &str, hidden: bool) -> Result<Listing, FsError> {
    let path = canonical_within(roots, Path::new(path))?;
    let mut entries = BinaryHeap::new();
    let mut truncated = false;
    for entry in fs::read_dir(&path)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !hidden && name.starts_with('.') {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() && !file_type.is_symlink() {
            continue;
        }
        let Ok(target) = canonical_within(roots, &entry.path()) else {
            continue;
        };
        if !fs::metadata(&target).is_ok_and(|metadata| metadata.is_dir()) {
            continue;
        }
        // Retain the first page in sorted order without storing an unbounded directory.
        entries.push((name.to_lowercase(), name, display(&target)));
        if entries.len() > MAX_ENTRIES {
            entries.pop();
            truncated = true;
        }
    }
    Ok(Listing {
        parent: path.parent().filter(|p| within(roots, p)).map(display),
        path: display(&path),
        entries: entries
            .into_sorted_vec()
            .into_iter()
            .map(|(_, name, path)| Entry { name, path })
            .collect(),
        truncated,
    })
}

pub fn mkdir(roots: &[PathBuf], parent: &str, name: &str) -> Result<String, FsError> {
    if name.is_empty()
        || name.len() > 255
        || matches!(name, "." | "..")
        || name.contains(['/', '\\', '\0'])
        || name.ends_with(['.', ' '])
        || reserved_name(name)
        || (name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && name.as_bytes().get(1) == Some(&b':'))
    {
        return Err(FsError::InvalidName);
    }
    let parent = canonical_within(roots, Path::new(parent))?;
    let path = parent.join(name);
    fs::create_dir(&path)?;
    Ok(display(&canonical_within(roots, &path)?))
}

fn reserved_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap().to_ascii_lowercase();
    matches!(stem.as_str(), "con" | "nul" | "aux" | "prn")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

pub fn canonical_within(roots: &[PathBuf], path: &Path) -> Result<PathBuf, FsError> {
    let path = fs::canonicalize(path)?;
    if within(roots, &path) {
        Ok(path)
    } else {
        Err(FsError::OutsideRoots)
    }
}

pub fn within(roots: &[PathBuf], path: &Path) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

pub fn display(path: &Path) -> String {
    let path = path.to_string_lossy();
    #[cfg(windows)]
    return strip_verbatim_prefix(&path);
    #[cfg(not(windows))]
    path.into_owned()
}

#[cfg(any(windows, test))]
fn strip_verbatim_prefix(path: &str) -> String {
    if let Some(path) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{path}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(path).to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn roots_use_component_names_and_display_filesystem_roots() {
        let temp = tempdir().unwrap();
        let path = fs::canonicalize(temp.path()).unwrap();
        let filesystem_root = path.ancestors().last().unwrap().to_path_buf();
        assert_eq!(
            roots(&[path.clone(), filesystem_root.clone()]),
            vec![
                Root {
                    path: display(&path),
                    name: path.file_name().unwrap().to_string_lossy().into_owned(),
                },
                Root {
                    path: display(&filesystem_root),
                    name: display(&filesystem_root),
                },
            ]
        );
    }

    #[test]
    fn outside_paths_and_parent_traversal_are_rejected() {
        let temp = tempdir().unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        let root = base.join("root");
        fs::create_dir(&root).unwrap();
        let roots = [root.clone()];
        for path in [
            base,
            root.join(".."),
            root.join("..").join("root").join(".."),
        ] {
            assert_eq!(canonical_within(&roots, &path), Err(FsError::OutsideRoots));
            assert_eq!(
                list(&roots, &display(&path), true),
                Err(FsError::OutsideRoots)
            );
            assert_eq!(
                mkdir(&roots, &display(&path), "child"),
                Err(FsError::OutsideRoots)
            );
        }
    }

    #[test]
    fn missing_paths_and_files_have_distinct_errors() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let roots = [root.clone()];
        let missing = root.join("missing");
        let file = root.join("file.txt");
        fs::write(&file, "").unwrap();
        assert_eq!(canonical_within(&roots, &missing), Err(FsError::NotFound));
        assert_eq!(
            list(&roots, &display(&missing), false),
            Err(FsError::NotFound)
        );
        assert_eq!(
            mkdir(&roots, &display(&missing), "child"),
            Err(FsError::NotFound)
        );
        assert_eq!(
            list(&roots, &display(&file), false),
            Err(FsError::NotADirectory)
        );
        assert_eq!(
            mkdir(&roots, &display(&file), "child"),
            Err(FsError::NotADirectory)
        );
    }

    #[test]
    fn listing_filters_files_and_hidden_names_and_sorts_without_case() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        for name in ["zebra", "Beta", "alpha", ".hidden"] {
            fs::create_dir(root.join(name)).unwrap();
        }
        fs::write(root.join("file.txt"), "").unwrap();
        let roots = [root.clone()];
        for (hidden, expected) in [
            (false, vec!["alpha", "Beta", "zebra"]),
            (true, vec![".hidden", "alpha", "Beta", "zebra"]),
        ] {
            let listing = list(&roots, &display(&root), hidden).unwrap();
            assert_eq!(listing.path, display(&root));
            assert_eq!(listing.parent, None);
            assert!(!listing.truncated);
            assert_eq!(
                listing.entries,
                expected
                    .into_iter()
                    .map(|name| Entry {
                        name: name.to_owned(),
                        path: display(&root.join(name)),
                    })
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn parent_stops_at_the_configured_root() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let child = root.join("child");
        fs::create_dir(&child).unwrap();
        let roots = [root.clone()];
        assert_eq!(list(&roots, &display(&root), false).unwrap().parent, None);
        let listing = list(&roots, &display(&child.join(".")), false).unwrap();
        assert_eq!(listing.path, display(&child));
        assert_eq!(listing.parent, Some(display(&root)));
    }

    #[test]
    fn listing_keeps_the_first_sorted_page_and_reports_overflow() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let roots = [root.clone()];
        for index in (0..MAX_ENTRIES).rev() {
            fs::create_dir(root.join(format!("dir{index:04}"))).unwrap();
        }
        let full = list(&roots, &display(&root), false).unwrap();
        assert!(!full.truncated);
        assert_eq!(full.entries.len(), MAX_ENTRIES);
        fs::create_dir(root.join("aaa")).unwrap();
        let overflow = list(&roots, &display(&root), false).unwrap();
        assert!(overflow.truncated);
        assert_eq!(overflow.entries.len(), MAX_ENTRIES);
        assert_eq!(overflow.entries[0].name, "aaa");
        assert_eq!(&overflow.entries[1..], &full.entries[..MAX_ENTRIES - 1]);
    }

    #[test]
    fn mkdir_creates_one_directory_and_rejects_existing_entries() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let roots = [root.clone()];
        let created = mkdir(&roots, &display(&root), "child").unwrap();
        assert_eq!(
            created,
            display(&fs::canonicalize(root.join("child")).unwrap())
        );
        assert!(root.join("child").is_dir());
        fs::write(root.join("file.txt"), "").unwrap();
        for name in ["child", "file.txt"] {
            assert_eq!(mkdir(&roots, &display(&root), name), Err(FsError::Exists));
        }
    }

    #[test]
    fn mkdir_rejects_invalid_components_and_counts_bytes() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let roots = [root.clone()];
        for name in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "a\0b",
            "C:",
            "c:child",
            "Z:child",
            &"a".repeat(256),
            &"é".repeat(128),
        ] {
            assert_eq!(
                mkdir(&roots, &display(&root), name),
                Err(FsError::InvalidName),
                "{name:?}"
            );
        }
        let name = "a".repeat(255);
        assert_eq!(
            mkdir(&roots, &display(&root), &name).unwrap(),
            display(&root.join(name))
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    }

    #[test]
    fn mkdir_refuses_portable_reserved_names_without_creating_entries() {
        let temp = tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut names = vec![
            "con".to_owned(),
            "NUL.txt".into(),
            "aux".into(),
            "PrN.log".into(),
            "trailing.".into(),
            "trailing ".into(),
        ];
        for prefix in ["COM", "lpt"] {
            for number in 1..=9 {
                names.push(format!("{prefix}{number}"));
                names.push(format!("{prefix}{number}.txt"));
            }
        }
        for name in names {
            assert_eq!(
                mkdir(std::slice::from_ref(&root), &display(&root), &name),
                Err(FsError::InvalidName)
            );
        }
        assert_eq!(fs::read_dir(root).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn looping_symlinks_do_not_hide_readable_directories() {
        let temp = tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir(root.join("visible")).unwrap();
        std::os::unix::fs::symlink("loop", root.join("loop")).unwrap();
        let listing = list(std::slice::from_ref(&root), &display(&root), false).unwrap();
        assert_eq!(
            listing.entries,
            [Entry {
                name: "visible".into(),
                path: display(&root.join("visible"))
            }]
        );
    }

    #[test]
    fn within_matches_components_not_string_prefixes() {
        let root = PathBuf::from("a").join("b");
        let roots = [root.clone()];
        assert!(within(&roots, &root));
        assert!(within(&roots, &root.join("child")));
        assert!(!within(&roots, &PathBuf::from("a").join("bc")));
        assert!(!within(&roots, Path::new("a")));
        assert!(!within(&[], &root));
    }

    #[test]
    fn windows_verbatim_paths_keep_drive_and_unc_meaning() {
        for (input, expected) in [
            (r"\\?\C:\work\folder", r"C:\work\folder"),
            (r"\\?\C:\", r"C:\"),
            (r"\\?\UNC\server\share\folder", r"\\server\share\folder"),
            (r"C:\work\folder", r"C:\work\folder"),
            (r"\\server\share", r"\\server\share"),
        ] {
            assert_eq!(strip_verbatim_prefix(input), expected);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_accepts_both_verbatim_and_display_paths() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let roots = [root.clone()];
        for input in [root.to_string_lossy().into_owned(), display(&root)] {
            assert_eq!(canonical_within(&roots, Path::new(&input)).unwrap(), root);
            assert_eq!(list(&roots, &input, false).unwrap().path, display(&root));
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_only_list_directory_targets_within_roots() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        let root = base.join("root");
        let inside = root.join("inside");
        let outside = base.join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&inside).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(root.join("file.txt"), "").unwrap();
        symlink(&outside, root.join("escape")).unwrap();
        symlink(&inside, root.join("alias")).unwrap();
        symlink(root.join("file.txt"), root.join("file-link")).unwrap();
        symlink(root.join("missing"), root.join("broken")).unwrap();
        let roots = [root.clone()];
        assert_eq!(
            list(&roots, &display(&root), true).unwrap().entries,
            vec![
                Entry {
                    name: "alias".into(),
                    path: display(&inside)
                },
                Entry {
                    name: "inside".into(),
                    path: display(&inside)
                },
            ]
        );
        let escape = root.join("escape");
        assert_eq!(
            canonical_within(&roots, &escape),
            Err(FsError::OutsideRoots)
        );
        assert_eq!(
            list(&roots, &display(&escape), false),
            Err(FsError::OutsideRoots)
        );
        assert_eq!(
            mkdir(&roots, &display(&escape), "child"),
            Err(FsError::OutsideRoots)
        );
        assert!(!outside.join("child").exists());
        let listing = list(&roots, &display(&root.join("alias")), false).unwrap();
        assert_eq!(listing.path, display(&inside));
        assert_eq!(listing.parent, Some(display(&root)));
    }
}
