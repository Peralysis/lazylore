use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{
    EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{ModifyKind, RenameMode},
};
use tokio::sync::mpsc;

/// Names lore itself reads for ignore rules (current + legacy), checked in
/// this order against the repository root.
const IGNORE_FILE_NAMES: [&str; 2] = [".loreignore", ".urcignore"];

pub struct RepositoryWatcher {
    _watcher: RecommendedWatcher,
}

/// Build a gitignore-style matcher from whichever of lore's ignore files
/// exist at the repository root. Missing files are simply skipped, so this
/// always succeeds (falling back to an empty, match-nothing set).
fn build_matcher(root: &Path) -> Gitignore {
    let mut builder = GitignoreBuilder::new(root);
    for name in IGNORE_FILE_NAMES {
        let candidate = root.join(name);
        if candidate.exists() {
            // Errors here mean the file was unreadable; treat it the same as
            // absent rather than failing watch setup over a bad ignore file.
            let _ = builder.add(&candidate);
        }
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

fn is_ignore_file(relative: &Path) -> bool {
    IGNORE_FILE_NAMES
        .iter()
        .any(|name| relative == Path::new(name))
}

impl RepositoryWatcher {
    pub fn start(root: &Path, tx: mpsc::UnboundedSender<PathBuf>) -> Result<Self> {
        let root = root.to_path_buf();
        let callback_root = root.clone();
        let matcher = Mutex::new(build_matcher(&root));
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                let Ok(event) = result else { return };
                // Windows' ReadDirectoryChangesW backend commonly reports
                // content writes as Modify(Any) rather than Modify(Data(_)),
                // so both must be accepted or edits go unnoticed until a
                // manual refresh. Metadata-only changes (e.g. atime) are
                // deliberately excluded to avoid noise.
                let relevant = matches!(
                    event.kind,
                    EventKind::Any
                        | EventKind::Create(_)
                        | EventKind::Remove(_)
                        | EventKind::Modify(
                            ModifyKind::Any
                                | ModifyKind::Data(_)
                                | ModifyKind::Name(
                                    RenameMode::Any
                                        | RenameMode::Both
                                        | RenameMode::From
                                        | RenameMode::To
                                )
                                | ModifyKind::Other
                        )
                );
                if !relevant {
                    return;
                }
                for path in event.paths {
                    if path
                        .components()
                        .any(|component| component.as_os_str() == ".lore")
                    {
                        continue;
                    }
                    let Ok(relative) = path.strip_prefix(&callback_root) else {
                        continue;
                    };
                    // Keep ignore rules current if the ignore file itself
                    // just changed, so edits take effect without a restart.
                    if is_ignore_file(relative) {
                        let mut guard = matcher.lock().unwrap_or_else(|e| e.into_inner());
                        *guard = build_matcher(&callback_root);
                    }
                    let ignored = {
                        let guard = matcher.lock().unwrap_or_else(|e| e.into_inner());
                        // `_or_any_parents` so a directory-level pattern (e.g.
                        // `target/`) also covers files changing underneath it,
                        // matching git/lore's usual ignore semantics.
                        guard
                            .matched_path_or_any_parents(relative, path.is_dir())
                            .is_ignore()
                    };
                    if ignored {
                        continue;
                    }
                    let _ = tx.send(relative.to_path_buf());
                }
            })
            .context("failed to initialize filesystem watcher")?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .with_context(|| format!("failed to watch {}", root.display()))?;
        Ok(Self { _watcher: watcher })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn matcher_ignores_patterns_from_loreignore() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".loreignore"), "target/\n*.log\n").unwrap();

        let matcher = build_matcher(dir.path());

        assert!(
            matcher
                .matched_path_or_any_parents("target/debug/build.rs", false)
                .is_ignore()
        );
        assert!(matcher.matched("output.log", false).is_ignore());
        assert!(!matcher.matched("src/main.rs", false).is_ignore());
    }

    #[test]
    fn matcher_is_empty_without_an_ignore_file() {
        let dir = tempfile::tempdir().unwrap();

        let matcher = build_matcher(dir.path());

        assert!(!matcher.matched("anything.txt", false).is_ignore());
    }

    #[test]
    fn matcher_honors_legacy_urcignore() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".urcignore"), "*.tmp\n").unwrap();

        let matcher = build_matcher(dir.path());

        assert!(matcher.matched("scratch.tmp", false).is_ignore());
    }

    #[test]
    fn is_ignore_file_matches_known_names() {
        assert!(is_ignore_file(Path::new(".loreignore")));
        assert!(is_ignore_file(Path::new(".urcignore")));
        assert!(!is_ignore_file(Path::new("src/main.rs")));
    }
}
