//! Folder import: iterative, bounded traversal that never follows links inside folders.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::converter::{self, Category};

pub struct Limits {
    pub max_files: usize,
    pub max_dirs: usize,
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_files: 100_000,
            max_dirs: 20_000,
            max_depth: 64,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Skip {
    Unsupported,
    Link,
    TooDeep,
}

pub enum Event {
    File(PathBuf, Category),
    Skipped(Skip),
    Error(PathBuf, io::Error),
    /// A budget was exhausted; nothing further is emitted.
    Stopped(String),
}

/// Walk `roots` depth-first in sorted order. Roots are followed even when they are
/// links (the user picked them); links found inside folders are reported, not followed.
pub fn scan(
    roots: Vec<PathBuf>,
    limits: &Limits,
    cancel: &AtomicBool,
    mut emit: impl FnMut(Event),
) {
    let mut stack: Vec<(PathBuf, usize)> = roots.into_iter().rev().map(|r| (r, 0)).collect();
    let (mut files, mut dirs) = (0, 0);
    while let Some((path, depth)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let metadata = if depth == 0 {
            fs::metadata(&path)
        } else {
            fs::symlink_metadata(&path)
        };
        let metadata = match metadata {
            Ok(metadata) => metadata,
            Err(error) => {
                emit(Event::Error(path, error));
                continue;
            }
        };
        // Only symlinks and junctions count; other reparse points (e.g. cloud files) do not.
        if metadata.file_type().is_symlink() {
            emit(Event::Skipped(Skip::Link));
        } else if metadata.is_dir() {
            if depth >= limits.max_depth {
                emit(Event::Skipped(Skip::TooDeep));
                continue;
            }
            if dirs == limits.max_dirs {
                emit(Event::Stopped(format!(
                    "import stopped: folder limit of {} reached",
                    limits.max_dirs
                )));
                return;
            }
            dirs += 1;
            let entries = match fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(error) => {
                    emit(Event::Error(path, error));
                    continue;
                }
            };
            let mut children = Vec::new();
            for entry in entries {
                match entry {
                    Ok(entry) => children.push(entry.path()),
                    Err(error) => emit(Event::Error(path.clone(), error)),
                }
            }
            children.sort();
            stack.extend(children.into_iter().rev().map(|child| (child, depth + 1)));
        } else if !metadata.is_file() {
            emit(Event::Error(path, io::Error::other("not a regular file")));
        } else if let Some(category) = converter::detect(&path) {
            if files == limits.max_files {
                emit(Event::Stopped(format!(
                    "import stopped: file limit of {} reached",
                    limits.max_files
                )));
                return;
            }
            files += 1;
            emit(Event::File(path, category));
        } else {
            emit(Event::Skipped(Skip::Unsupported));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct Report {
        files: Vec<PathBuf>,
        skipped: Vec<Skip>,
        errors: Vec<PathBuf>,
        stopped: Option<String>,
    }

    fn run(roots: Vec<PathBuf>, limits: &Limits) -> Report {
        let mut report = Report::default();
        scan(
            roots,
            limits,
            &AtomicBool::new(false),
            |event| match event {
                Event::File(path, _) => report.files.push(path),
                Event::Skipped(reason) => report.skipped.push(reason),
                Event::Error(path, _) => report.errors.push(path),
                Event::Stopped(message) => report.stopped = Some(message),
            },
        );
        report
    }

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"{}").unwrap();
    }

    fn dir_symlink(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        let result = std::os::unix::fs::symlink(target, link);
        #[cfg(windows)]
        let result = std::os::windows::fs::symlink_dir(target, link);
        if let Err(error) = result {
            #[cfg(windows)]
            if error.raw_os_error() == Some(1314) {
                // CI sets this so a missing privilege fails instead of skipping.
                assert!(
                    std::env::var_os("CONVERTCRAFT_REQUIRE_SYMLINKS").is_none(),
                    "CONVERTCRAFT_REQUIRE_SYMLINKS is set but symlink creation is not permitted"
                );
                eprintln!("symlink test requires Windows Developer Mode or symlink privilege");
                return false;
            }
            panic!("cannot create test symlink: {error}");
        }
        true
    }

    #[test]
    fn walks_in_sorted_order_and_counts_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("b.json"));
        touch(&dir.path().join("a/c.png"));
        touch(&dir.path().join("notes.txt"));
        let report = run(vec![dir.path().to_owned()], &Limits::default());
        assert_eq!(
            report.files,
            [dir.path().join("a/c.png"), dir.path().join("b.json")]
        );
        assert_eq!(report.skipped, [Skip::Unsupported]);
        assert!(report.errors.is_empty() && report.stopped.is_none());
    }

    #[test]
    fn links_back_to_the_folder_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("one.json"));
        if !dir_symlink(dir.path(), &dir.path().join("a"))
            || !dir_symlink(dir.path(), &dir.path().join("b"))
        {
            return;
        }
        let start = Instant::now();
        let report = run(vec![dir.path().to_owned()], &Limits::default());
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(report.files, [dir.path().join("one.json")]);
        assert_eq!(report.skipped, [Skip::Link, Skip::Link]);
    }

    #[cfg(windows)]
    #[test]
    fn junction_loops_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("one.json"));
        for name in ["a", "b"] {
            let status = std::process::Command::new("cmd")
                .arg("/C")
                .arg("mklink")
                .arg("/J")
                .arg(dir.path().join(name))
                .arg(dir.path())
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "mklink /J failed");
        }
        let report = run(vec![dir.path().to_owned()], &Limits::default());
        assert_eq!(report.files, [dir.path().join("one.json")]);
        assert_eq!(report.skipped, [Skip::Link, Skip::Link]);
    }

    #[test]
    fn a_linked_root_is_followed() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("real/one.json"));
        let link = dir.path().join("link");
        if !dir_symlink(&dir.path().join("real"), &link) {
            return;
        }
        let report = run(vec![link.clone()], &Limits::default());
        assert_eq!(report.files, [link.join("one.json")]);
    }

    #[test]
    fn errors_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let report = run(vec![missing.clone()], &Limits::default());
        assert_eq!(report.errors, [missing]);
        assert!(report.files.is_empty());
    }

    #[test]
    fn budgets_stop_or_skip() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["1.json", "2.json", "3.json"] {
            touch(&dir.path().join(name));
        }
        touch(&dir.path().join("x/y/deep.json"));
        let limits = Limits {
            max_files: 2,
            ..Limits::default()
        };
        let report = run(vec![dir.path().to_owned()], &limits);
        assert_eq!(report.files.len(), 2);
        assert!(report.stopped.unwrap().contains("file limit of 2"));

        let limits = Limits {
            max_depth: 2,
            ..Limits::default()
        };
        let report = run(vec![dir.path().to_owned()], &limits);
        assert_eq!(report.files.len(), 3);
        assert_eq!(report.skipped, [Skip::TooDeep]);

        let limits = Limits {
            max_dirs: 1,
            ..Limits::default()
        };
        let report = run(vec![dir.path().to_owned()], &limits);
        assert!(report.stopped.unwrap().contains("folder limit of 1"));
    }

    #[test]
    fn cancellation_stops_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("one.json"));
        let mut events = 0;
        scan(
            vec![dir.path().to_owned()],
            &Limits::default(),
            &AtomicBool::new(true),
            |_| events += 1,
        );
        assert_eq!(events, 0);
    }
}
