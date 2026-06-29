use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use notify::{
    EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{ModifyKind, RenameMode},
};
use tokio::sync::mpsc;

pub struct RepositoryWatcher {
    _watcher: RecommendedWatcher,
}

impl RepositoryWatcher {
    pub fn start(root: &Path, tx: mpsc::UnboundedSender<PathBuf>) -> Result<Self> {
        let root = root.to_path_buf();
        let callback_root = root.clone();
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                let Ok(event) = result else { return };
                let relevant = matches!(
                    event.kind,
                    EventKind::Create(_)
                        | EventKind::Remove(_)
                        | EventKind::Modify(ModifyKind::Data(_))
                        | EventKind::Modify(ModifyKind::Name(
                            RenameMode::Any | RenameMode::Both | RenameMode::From | RenameMode::To
                        ))
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
                    if let Ok(relative) = path.strip_prefix(&callback_root) {
                        let _ = tx.send(relative.to_path_buf());
                    }
                }
            })
            .context("failed to initialize filesystem watcher")?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .with_context(|| format!("failed to watch {}", root.display()))?;
        Ok(Self { _watcher: watcher })
    }
}
