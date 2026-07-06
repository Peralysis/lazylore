use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::{process::Command, sync::mpsc};

use crate::{
    cache::RevisionCache,
    config::Config,
    lore::{CommandClass, CommandRequest, LoreClient, StreamMessage, event_error, event_summary},
    manifest::{CommandSpec, Safety, baseline_commands, merge_runtime_manifest, split_arguments},
    model::{
        AppState, Branch, BranchSync, BranchTab, Conflict, DiffMode, FileLock, FileStatus, Focus,
        LoreEvent, Revision, field_bool, field_string, field_u64,
    },
    tree::TreeEntry,
    watcher::RepositoryWatcher,
};

#[derive(Debug, Clone)]
pub enum PromptAction {
    Commit,
    Amend,
    NewBranch,
    ResetBranch { branch: String },
    Shell,
    CommandArguments(CommandSpec),
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    ResetFile(String),
    ArchiveBranch(String),
    SyncRevision(String),
    RevertRevision(String),
    ResetToRevision(String),
    RunCommand {
        spec: CommandSpec,
        args: Vec<String>,
    },
}

#[derive(Debug, Clone)]
pub enum Mode {
    Normal,
    Help,
    Palette {
        query: String,
        selected: usize,
    },
    Prompt {
        title: String,
        value: String,
        secret: bool,
        action: PromptAction,
    },
    Confirm {
        title: String,
        message: String,
        required: Option<String>,
        typed: String,
        action: ConfirmAction,
    },
}

/// State for the revision file-tree drill-down view.
/// When `Some`, the Revisions pane renders this tree instead of the revision list.
pub struct RevisionView {
    pub number: u64,
    /// Full hash of the revision being browsed.
    pub hash: String,
    /// First non-null parent hash, or `None` for the initial revision.
    pub parent: Option<String>,
    pub entries: Vec<TreeEntry>,
    pub collapsed: HashSet<String>,
    /// Index into `visible()`, not into `entries`.
    pub selected: usize,
}

impl RevisionView {
    pub fn visible(&self) -> Vec<&TreeEntry> {
        crate::tree::visible_entries(&self.entries, &self.collapsed)
    }
}

pub struct App {
    pub state: AppState,
    pub focus: Focus,
    pub diff_mode: DiffMode,
    pub mode: Mode,
    pub file_selected: usize,
    pub branch_selected: usize,
    pub branch_tab: BranchTab,
    pub revision_selected: usize,
    pub lock_selected: usize,
    pub main_scroll: u16,
    pub command_log_selected: usize,
    pub commands: Vec<CommandSpec>,
    pub lore_version: String,
    pub banner: String,
    pub copied_revision: Option<String>,
    pub revision_view: Option<RevisionView>,
    pub should_quit: bool,
    /// `true` when the user has explicitly forced offline mode (`--offline` or
    /// the `O` toggle). Suppresses all server-touching commands and the
    /// background reconnection poll.
    pub offline_forced: bool,
    pub config: Config,
    lore: LoreClient,
    stream_tx: mpsc::UnboundedSender<StreamMessage>,
    stream_rx: mpsc::UnboundedReceiver<StreamMessage>,
    watch_rx: mpsc::UnboundedReceiver<PathBuf>,
    _watcher: Option<RepositoryWatcher>,
    pending_paths: HashSet<PathBuf>,
    last_watch_event: Option<Instant>,
    /// When we were last performing an auto-offline reconnection probe.
    last_reconnect_probe: Option<Instant>,
}

fn make_banner() -> String {
    tui_banner::Banner::new("lazylore")
        .map(|b| b.style(tui_banner::Style::NeonCyber).render())
        .unwrap_or_default()
}

impl App {
    #[cfg(test)]
    pub(crate) fn test_fixture() -> Self {
        let config = Config::default();
        let lore = LoreClient::new("lore", ".");
        let (stream_tx, stream_rx) = mpsc::unbounded_channel();
        let (_watch_tx, watch_rx) = mpsc::unbounded_channel();
        let mut state = AppState::default();
        state.repository.root = PathBuf::from("/demo");
        state.repository.branch = "main".into();
        state.repository.revision = "0123456789abcdef".into();
        state.repository.revision_number = 42;
        state.repository.remote_available = true;
        state.repository.remote_authorized = true;
        state.repository.stale = true;
        state.files.push(FileStatus {
            path: "src/main.rs".into(),
            action: "modify".into(),
            dirty: true,
            size: 128,
            ..FileStatus::default()
        });
        state.branches.push(Branch {
            name: "main".into(),
            current: true,
            location: "local".into(),
            latest: "0123456789abcdef".into(),
            ..Branch::default()
        });
        state.revisions.push(Revision {
            hash: "0123456789abcdef".into(),
            number: 42,
            message: "Initial revision".into(),
            ..Revision::default()
        });
        state.locks.push(FileLock {
            path: "Content/Hero.uasset".into(),
            owner: "artist@example.com".into(),
            branch: "main".into(),
            locked: true,
        });
        state.preview = vec![
            "--- a/src/main.rs".into(),
            "+++ b/src/main.rs".into(),
            "+hello".into(),
        ];
        Self {
            state,
            focus: Focus::Files,
            diff_mode: DiffMode::Working,
            mode: Mode::Normal,
            file_selected: 0,
            branch_selected: 0,
            branch_tab: BranchTab::default(),
            revision_selected: 0,
            lock_selected: 0,
            main_scroll: 0,
            command_log_selected: 0,
            commands: baseline_commands(),
            lore_version: "0.8.4".into(),
            banner: make_banner(),
            copied_revision: None,
            revision_view: None,
            should_quit: false,
            offline_forced: false,
            config,
            lore,
            stream_tx,
            stream_rx,
            watch_rx,
            _watcher: None,
            pending_paths: HashSet::new(),
            last_watch_event: None,
            last_reconnect_probe: None,
        }
    }

    pub async fn new(repository: PathBuf, config: Config) -> Result<Self> {
        let cache = if config.cache.enabled {
            let dir = Config::cache_dir().map(|base| crate::cache::scope_dir(&base, &repository));
            RevisionCache::new(
                dir,
                config.cache.ttl(),
                config.cache.max_disk_bytes(),
                config.cache.max_memory_entries,
            )
        } else {
            RevisionCache::disabled()
        };
        // Prune once at startup so expired/over-budget entries from previous
        // sessions don't accumulate forever. Spawned so it never delays launch.
        tokio::spawn({
            let cache = cache.clone();
            async move { cache.prune().await }
        });
        let lore = LoreClient::new(config.general.lore_binary.clone(), &repository)
            .with_timeout(config.command_timeout())
            .with_cache(cache);
        let version = lore.validate().await?;
        let commands = match lore.markdown_help().await {
            Ok(markdown) => merge_runtime_manifest(baseline_commands(), &markdown),
            Err(_) => baseline_commands(),
        };
        let (stream_tx, stream_rx) = mpsc::unbounded_channel();
        let (watch_tx, watch_rx) = mpsc::unbounded_channel();
        let watcher = if config.general.watch_files {
            RepositoryWatcher::start(&repository, watch_tx).ok()
        } else {
            None
        };
        let offline_forced = config.general.offline;
        lore.set_offline(offline_forced);
        let mut app = Self {
            state: AppState::default(),
            focus: Focus::Files,
            diff_mode: DiffMode::Working,
            mode: Mode::Normal,
            file_selected: 0,
            branch_selected: 0,
            branch_tab: BranchTab::default(),
            revision_selected: 0,
            lock_selected: 0,
            main_scroll: 0,
            command_log_selected: 0,
            commands,
            lore_version: version.to_string(),
            banner: make_banner(),
            copied_revision: None,
            revision_view: None,
            should_quit: false,
            offline_forced,
            config,
            lore,
            stream_tx,
            stream_rx,
            watch_rx,
            _watcher: watcher,
            pending_paths: HashSet::new(),
            last_watch_event: None,
            last_reconnect_probe: None,
        };
        app.state.repository.root = repository;
        app.state.repository.stale = true;
        Ok(app)
    }

    /// Run the initial server-touching refresh. Split out of `new` so the
    /// terminal can show a loading screen while this (potentially slow) work
    /// runs, instead of blocking before the UI ever renders.
    pub async fn load_initial(&mut self) {
        self.refresh_all(self.config.general.scan_on_start).await;
    }

    /// Split, disjoint borrows of the stream and watch receivers so the
    /// event loop can `select!` on both directly (a second `&mut self`
    /// accessor call per receiver would conflict, since each borrows all of
    /// `App` as far as the borrow checker is concerned).
    pub fn receivers(
        &mut self,
    ) -> (
        &mut mpsc::UnboundedReceiver<StreamMessage>,
        &mut mpsc::UnboundedReceiver<PathBuf>,
    ) {
        (&mut self.stream_rx, &mut self.watch_rx)
    }

    pub fn drain_watcher(&mut self) {
        while let Ok(path) = self.watch_rx.try_recv() {
            self.note_path_change(path);
        }
    }

    pub fn note_path_change(&mut self, path: PathBuf) {
        if path.as_os_str().is_empty() {
            return;
        }
        self.pending_paths.insert(path);
        self.last_watch_event = Some(Instant::now());
    }

    pub async fn flush_watcher(&mut self) {
        if self.pending_paths.is_empty()
            || self
                .last_watch_event
                .is_some_and(|time| time.elapsed() < Duration::from_millis(300))
        {
            return;
        }
        let paths: Vec<String> = self
            .pending_paths
            .drain()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        self.last_watch_event = None;
        for chunk in paths.chunks(100) {
            let mut args = vec!["dirty".to_string()];
            args.extend(chunk.iter().cloned());
            if let Ok(output) = self
                .lore
                .capture(CommandRequest {
                    args,
                    class: CommandClass::Mutating,
                    cacheable: false,
                })
                .await
            {
                self.push_record(output.record);
            }
        }
        self.refresh_status(false).await;
    }

    /// Returns `true` when the server is considered unreachable. This is the
    /// case when the user has forced offline mode, or when the last `status`
    /// response reported `remoteAvailable = false`.
    pub fn is_offline(&self) -> bool {
        self.offline_forced || !self.state.repository.remote_available
    }

    /// Re-probe the server connection if currently offline. Returns `true` when
    /// the server is reachable (either already was, or just came back). Call
    /// this before attempting any operation that requires a live connection.
    async fn ensure_online(&mut self) -> bool {
        if !self.is_offline() {
            return true;
        }
        if !self.offline_forced {
            // Give Lore a genuine chance to reach the server instead of
            // short-circuiting on our own cached offline flag.
            self.lore.set_offline(false);
        }
        self.refresh_status(false).await;
        !self.is_offline()
    }

    /// If in auto-offline mode, fire a low-frequency reconnection probe and
    /// repopulate server-dependent data on reconnect. No-op when forced
    /// offline or already online.
    pub async fn maybe_reconnect(&mut self) {
        if self.offline_forced || !self.is_offline() {
            return;
        }
        let interval = self.config.reconnect_interval();
        if self
            .last_reconnect_probe
            .is_some_and(|t| t.elapsed() < interval)
        {
            return;
        }
        self.last_reconnect_probe = Some(Instant::now());
        // Force a real network attempt rather than trusting the cached
        // offline flag, otherwise we'd never notice the server came back.
        self.lore.set_offline(false);
        self.refresh_status(false).await;
        if !self.is_offline() {
            // We just came back online — repopulate server-dependent data.
            self.refresh_branches().await;
            self.refresh_locks().await;
            self.last_reconnect_probe = None;
        }
    }

    pub async fn refresh_all(&mut self, scan: bool) {
        self.refresh_status(scan).await;
        if self.state.repository_error.is_none() {
            // Skip server-dependent refreshes when offline; last-known data
            // remains in place until the connection is restored.
            if !self.is_offline() {
                self.refresh_branches().await;
                self.refresh_locks().await;
            }
            self.refresh_history(None).await;
            self.refresh_preview().await;
        }
    }

    async fn refresh_status(&mut self, scan: bool) {
        let args = if scan {
            vec!["status", "--scan"]
        } else {
            vec!["status"]
        };
        match self.lore.capture(CommandRequest::read(args)).await {
            Ok(output) => {
                self.push_record(output.record.clone());
                if output.timed_out {
                    // A timeout means the server is unreachable, not that the
                    // repository is broken. Keep last-known file state intact.
                    self.state.repository.remote_available = false;
                    // From here on, skip Lore's own network round-trip until
                    // we deliberately probe again — otherwise every refresh
                    // (including watcher-driven ones) pays the same timeout.
                    self.lore.set_offline(self.is_offline());
                    return;
                }
                if !output.record.success {
                    self.state.repository_error =
                        Some(command_error(&output.events, &output.record.stderr));
                    self.lore.set_offline(self.is_offline());
                    return;
                }
                self.state.repository_error = None;
                self.state.files.clear();
                self.state.conflicts.clear();
                for event in output.events {
                    match event.tag.as_str() {
                        "repositoryStatusRevision" => {
                            self.state.repository.repository =
                                field_string(&event.data, "repository");
                            self.state.repository.branch = field_string(&event.data, "branchName");
                            self.state.repository.branch_id = field_string(&event.data, "branch");
                            self.state.repository.revision = field_string(&event.data, "revision");
                            self.state.repository.revision_number =
                                field_u64(&event.data, "revisionNumber");
                            self.state.repository.staged_revision =
                                field_string(&event.data, "revisionStaged");
                            self.state.repository.remote_revision =
                                field_string(&event.data, "revisionRemote");
                            self.state.repository.local_ahead =
                                field_bool(&event.data, "isLocalAhead");
                            self.state.repository.remote_ahead =
                                field_bool(&event.data, "isRemoteAhead");
                            self.state.repository.remote_available =
                                field_bool(&event.data, "remoteAvailable");
                            self.state.repository.remote_authorized =
                                field_bool(&event.data, "remoteAuthorized");
                        }
                        "repositoryStatusFile" => {
                            let file = FileStatus {
                                path: field_string(&event.data, "path"),
                                from_path: field_string(&event.data, "fromPath"),
                                size: field_u64(&event.data, "size"),
                                action: field_string(&event.data, "action"),
                                node_type: field_string(&event.data, "type"),
                                staged: field_bool(&event.data, "flagStaged"),
                                dirty: field_bool(&event.data, "flagDirty"),
                                conflict: field_bool(&event.data, "flagConflict"),
                                unresolved: field_bool(&event.data, "flagConflictUnresolved"),
                            };
                            if file.conflict {
                                self.state.conflicts.push(Conflict {
                                    path: file.path.clone(),
                                    operation: "merge".into(),
                                    resolved: !file.unresolved,
                                });
                            }
                            self.state.files.push(file);
                        }
                        _ => {}
                    }
                }
                self.state.files.sort_by(|a, b| a.path.cmp(&b.path));
                self.file_selected = clamp_selection(self.file_selected, self.state.files.len());
                if scan {
                    self.state.repository.stale = false;
                }
                self.lore.set_offline(self.is_offline());
            }
            Err(error) => self.state.repository_error = Some(error.to_string()),
        }
    }

    async fn refresh_branches(&mut self) {
        let Ok(output) = self
            .lore
            .capture(CommandRequest::read(["branch", "list", "--archived"]))
            .await
        else {
            return;
        };
        self.push_record(output.record.clone());
        if !output.record.success {
            return;
        }
        self.state.branches = output
            .events
            .iter()
            .filter(|event| event.tag == "branchListEntry")
            .map(|event| Branch {
                name: field_string(&event.data, "name"),
                id: field_string(&event.data, "id"),
                location: field_string(&event.data, "location"),
                latest: field_string(&event.data, "latest"),
                creator: field_string(&event.data, "creator"),
                current: field_bool(&event.data, "isCurrent"),
                archived: field_bool(&event.data, "archived"),
            })
            .collect();
        self.state
            .branches
            .sort_by_key(|branch| (!branch.current, branch.archived, branch.name.clone()));
        let visible_count = self.visible_branches().len();
        self.branch_selected = clamp_selection(self.branch_selected, visible_count);
    }

    async fn refresh_history(&mut self, branch: Option<String>) {
        self.revision_view = None;
        let mut args = vec![
            "history".to_string(),
            self.config.general.history_page_size.to_string(),
        ];
        if let Some(branch) = branch {
            args.extend(["--branch".into(), branch]);
        }
        let Ok(output) = self.lore.capture(CommandRequest::read(args)).await else {
            return;
        };
        self.push_record(output.record.clone());
        if !output.record.success {
            return;
        }
        let mut last_message = String::new();
        let mut revisions: Vec<Revision> = Vec::new();
        for event in output.events {
            if event.tag == "metadata" {
                let key = field_string(&event.data, "key").to_ascii_lowercase();
                if key.contains("message") {
                    // Value is a tagged wrapper: {"tagName":"string","data":"actual text"}
                    last_message = event
                        .data
                        .get("value")
                        .and_then(|v| v.get("data"))
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                        .unwrap_or_else(|| field_string(&event.data, "value"));
                }
            } else if event.tag == "revisionHistoryEntry" {
                // Metadata arrives after its entry; flush the pending message into the
                // previous revision before pushing the new one.
                if let Some(prev) = revisions.last_mut()
                    && prev.message.is_empty()
                {
                    prev.message = std::mem::take(&mut last_message);
                }
                last_message.clear();
                let parents = event
                    .data
                    .get("parent")
                    .and_then(|v| v.as_array())
                    .map(|values| {
                        values
                            .iter()
                            .map(|v| match v {
                                serde_json::Value::String(text) => text.clone(),
                                other => other.to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                revisions.push(Revision {
                    hash: first_non_empty(&[
                        field_string(&event.data, "revision"),
                        field_string(&event.data, "signature"),
                    ]),
                    number: field_u64(&event.data, "revisionNumber"),
                    message: field_string(&event.data, "message"),
                    parents,
                });
            }
        }
        // Flush trailing metadata for the last revision
        if let Some(last) = revisions.last_mut()
            && last.message.is_empty()
        {
            last.message = last_message;
        }
        self.state.revisions = revisions;
        self.revision_selected =
            clamp_selection(self.revision_selected, self.state.revisions.len());
    }

    async fn refresh_locks(&mut self) {
        let Ok(output) = self
            .lore
            .capture(CommandRequest::read(["lock", "query"]))
            .await
        else {
            return;
        };
        self.push_record(output.record.clone());
        if !output.record.success {
            return;
        }
        self.state.locks = output
            .events
            .iter()
            .filter(|event| event.tag == "lockFileQuery" || event.tag == "lockFileStatus")
            .map(|event| FileLock {
                path: field_string(&event.data, "path"),
                branch: field_string(&event.data, "branch"),
                owner: first_non_empty(&[
                    field_string(&event.data, "owner"),
                    field_string(&event.data, "ownerId"),
                ]),
                locked: true,
            })
            .collect();
        self.lock_selected = clamp_selection(self.lock_selected, self.state.locks.len());
    }

    async fn refresh_preview(&mut self) {
        if self.focus == Focus::CommandLog {
            return;
        }
        self.main_scroll = 0;
        match self.focus {
            Focus::Files | Focus::Main => self.preview_file().await,
            Focus::Branches => {
                if let Some(branch) = self.selected_branch() {
                    self.state.preview = vec![
                        format!("Branch: {}", branch.name),
                        format!("Location: {}", branch.location),
                        format!("Latest: {}", branch.latest),
                        format!("Creator: {}", branch.creator),
                        format!("Current: {}", branch.current),
                        format!("Archived: {}", branch.archived),
                    ];
                }
            }
            Focus::Revisions => {
                if self.revision_view.is_some() {
                    self.preview_revision_file().await;
                } else if let Some(revision) = self.state.revisions.get(self.revision_selected) {
                    let args = vec![
                        "revision".into(),
                        "info".into(),
                        revision.hash.clone(),
                        "--delta".into(),
                    ];
                    // Keyed on an immutable revision hash: safe to memoize.
                    if let Ok(output) = self.lore.capture(CommandRequest::cached_read(args)).await {
                        self.push_record(output.record);
                        self.state.preview = build_revision_readout(&output.events);
                    }
                }
            }
            Focus::Locks => {
                if let Some(lock) = self.state.locks.get(self.lock_selected) {
                    self.state.preview = vec![
                        format!("Path: {}", lock.path),
                        format!("Branch: {}", lock.branch),
                        format!("Owner: {}", lock.owner),
                    ];
                }
            }
            Focus::Repository => self.repository_preview(),
            Focus::CommandLog => {}
        }
    }

    async fn preview_file(&mut self) {
        let Some(file) = self.state.files.get(self.file_selected) else {
            self.state.preview = vec!["Working tree clean (or run r to scan for changes).".into()];
            return;
        };
        let file_path = file.path.clone();
        let file_action = file.action.clone();
        let file_size = file.size;
        let mut args = vec!["diff".to_string()];
        match self.diff_mode {
            DiffMode::Working => {}
            DiffMode::Staged if !is_zero_hash(&self.state.repository.staged_revision) => {
                args.extend([
                    "--source".into(),
                    self.state.repository.revision.clone(),
                    "--target".into(),
                    self.state.repository.staged_revision.clone(),
                ]);
            }
            DiffMode::Unstaged if !is_zero_hash(&self.state.repository.staged_revision) => {
                args.extend([
                    "--source".into(),
                    self.state.repository.staged_revision.clone(),
                ]);
            }
            _ => {}
        }
        args.push(file_path.clone());
        if let Ok(output) = self.lore.capture(CommandRequest::read(args)).await {
            self.push_record(output.record.clone());
            let header = format!("{file_action} {file_path} ({file_size} bytes)");
            self.state.preview = if output.timed_out {
                vec![header, "Diff timed out.".into()]
            } else if !output.record.success {
                vec![
                    header,
                    format!(
                        "Diff unavailable: {}",
                        command_error(&output.events, &output.record.stderr)
                    ),
                ]
            } else {
                let patches = diff_preview_lines(&output.events);
                if patches.is_empty() {
                    vec![
                        header,
                        "No text patch available. This may be binary content.".into(),
                    ]
                } else {
                    patches
                }
            };
        }
    }

    fn repository_preview(&mut self) {
        let repo = &self.state.repository;
        self.state.preview = vec![
            format!("Root: {}", repo.root.display()),
            format!("Repository: {}", repo.repository),
            format!("Branch: {}", repo.branch),
            format!("Revision: @{} {}", repo.revision_number, repo.revision),
            format!("Staged: {}", repo.staged_revision),
            format!("Remote available: {}", repo.remote_available),
            format!("Remote authorized: {}", repo.remote_authorized),
            format!("Local ahead: {}", repo.local_ahead),
            format!("Remote ahead: {}", repo.remote_ahead),
        ];
    }

    pub fn handle_stream(&mut self, message: StreamMessage) -> bool {
        match message {
            StreamMessage::Started { display } => {
                self.state.busy = true;
                self.state.progress = Some(display.clone());
                self.state
                    .operation_output
                    .push_back(format!("$ {display}"));
            }
            StreamMessage::Event(event) => {
                if event.tag == "progress" {
                    self.state.progress = Some(event_summary(&event));
                }
                self.state.operation_output.push_back(event_summary(&event));
            }
            StreamMessage::Stderr(stderr) => {
                for line in stderr.lines() {
                    self.state.operation_output.push_back(format!("! {line}"));
                }
            }
            StreamMessage::Finished(record) => {
                self.state.busy = false;
                self.state.progress = None;
                self.state.operation_output.push_back(format!(
                    "{} in {:.2?}",
                    if record.success {
                        "completed"
                    } else {
                        "failed"
                    },
                    record.duration
                ));
                self.push_record(record);
                return true;
            }
        }
        while self.state.operation_output.len() > 1_000 {
            self.state.operation_output.pop_front();
        }
        false
    }

    fn push_record(&mut self, record: crate::model::CommandRecord) {
        self.state.command_history.push_front(record);
        while self.state.command_history.len() > 200 {
            self.state.command_history.pop_back();
        }
    }

    fn start(&mut self, request: CommandRequest) {
        if self.state.busy {
            return;
        }
        self.lore.stream(request, self.stream_tx.clone());
    }

    async fn cycle_focus(&mut self, delta: isize) {
        const PANES: [Focus; 6] = [
            Focus::Repository,
            Focus::Files,
            Focus::Branches,
            Focus::Revisions,
            Focus::Locks,
            Focus::Main,
        ];
        let current = PANES
            .iter()
            .position(|focus| *focus == self.focus)
            .unwrap_or_default();
        let next = (current as isize + delta).rem_euclid(PANES.len() as isize) as usize;
        self.focus = PANES[next];
        self.refresh_preview().await;
    }

    pub fn visible_branches(&self) -> Vec<&Branch> {
        self.state
            .branches
            .iter()
            .filter(|b| self.branch_tab.matches(b))
            .collect()
    }

    /// Classify the sync state of a branch relative to its remote counterpart.
    pub fn branch_sync(&self, branch: &Branch) -> BranchSync {
        if branch.current {
            let repo = &self.state.repository;
            return match (repo.local_ahead, repo.remote_ahead) {
                (true, true) => BranchSync::Diverged,
                (true, false) => BranchSync::Ahead,
                (false, true) => BranchSync::Behind,
                (false, false) => BranchSync::InSync,
            };
        }
        if BranchTab::Local.matches(branch) {
            // Look for a remote branch with the same name to compare latest hashes.
            let remote = self
                .state
                .branches
                .iter()
                .find(|b| BranchTab::Remote.matches(b) && b.name == branch.name);
            match remote {
                Some(r) if r.latest == branch.latest => BranchSync::InSync,
                Some(_) => BranchSync::Differs,
                None => BranchSync::Untracked,
            }
        } else {
            // Remote branch entries: no indicator needed.
            BranchSync::Untracked
        }
    }

    fn selected_branch(&self) -> Option<Branch> {
        self.visible_branches()
            .get(self.branch_selected)
            .map(|b| (*b).clone())
    }

    async fn cycle_branch_tab(&mut self, delta: isize) {
        const TABS: [BranchTab; 2] = [BranchTab::Local, BranchTab::Remote];
        let current = TABS
            .iter()
            .position(|t| *t == self.branch_tab)
            .unwrap_or_default();
        let next = (current as isize + delta).rem_euclid(TABS.len() as isize) as usize;
        self.branch_tab = TABS[next];
        self.branch_selected = 0;
        self.refresh_preview().await;
    }

    fn remap_key(&self, key: KeyEvent) -> KeyEvent {
        let actions = action_keys(self.focus);
        for (action, default_code, default_modifiers) in &actions {
            if let Some(binding) = self.config.keybindings.get(*action)
                && binding_matches(binding, key)
            {
                return KeyEvent::new(*default_code, *default_modifiers);
            }
        }
        for (action, default_code, default_modifiers) in actions {
            if self.config.keybindings.contains_key(action)
                && key_matches(key, default_code, default_modifiers)
            {
                return KeyEvent::new(KeyCode::Null, KeyModifiers::NONE);
            }
        }
        key
    }

    pub async fn on_key(&mut self, key: KeyEvent) {
        match &mut self.mode {
            Mode::Help => {
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?')
                ) {
                    self.mode = Mode::Normal;
                }
                return;
            }
            Mode::Palette { query, selected } => {
                match key.code {
                    KeyCode::Esc => self.mode = Mode::Normal,
                    KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        query.push(ch);
                        *selected = 0;
                    }
                    KeyCode::Backspace => {
                        query.pop();
                        *selected = 0;
                    }
                    KeyCode::Down | KeyCode::Char('j') => *selected = selected.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                    KeyCode::Enter => {
                        let matches = filtered_commands(&self.commands, query);
                        if let Some(spec) = matches.get(*selected).cloned().cloned() {
                            self.mode = Mode::Prompt {
                                title: format!("Arguments — lore {}", spec.path),
                                value: String::new(),
                                secret: spec.safety == Safety::Secret,
                                action: PromptAction::CommandArguments(spec),
                            };
                        }
                    }
                    _ => {}
                }
                return;
            }
            Mode::Prompt { value, action, .. } => {
                match key.code {
                    KeyCode::Esc => self.mode = Mode::Normal,
                    KeyCode::Backspace => {
                        value.pop();
                    }
                    KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        value.push(ch)
                    }
                    KeyCode::Enter => {
                        let action = action.clone();
                        let value = value.clone();
                        self.mode = Mode::Normal;
                        self.submit_prompt(action, value).await;
                    }
                    _ => {}
                }
                return;
            }
            Mode::Confirm {
                required,
                typed,
                action,
                ..
            } => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('n') if required.is_none() => {
                        self.mode = Mode::Normal
                    }
                    KeyCode::Backspace => {
                        typed.pop();
                    }
                    KeyCode::Char(ch) if required.is_some() => typed.push(ch),
                    KeyCode::Char('y') if required.is_none() => {
                        let action = action.clone();
                        self.mode = Mode::Normal;
                        self.execute_confirm(action).await;
                    }
                    KeyCode::Enter
                        if required.as_ref().is_none_or(|required| required == typed) =>
                    {
                        let action = action.clone();
                        self.mode = Mode::Normal;
                        self.execute_confirm(action).await;
                    }
                    _ => {}
                }
                return;
            }
            Mode::Normal => {}
        }
        let key = self.remap_key(key);

        if key.code == KeyCode::Esc && self.state.busy {
            self.lore.cancel_current();
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('p') {
            self.mode = Mode::Palette {
                query: String::new(),
                selected: 0,
            };
            return;
        }
        match key.code {
            KeyCode::Char('q') if !self.state.busy => self.should_quit = true,
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('@') => self.focus = Focus::CommandLog,
            KeyCode::Char(':') => {
                self.mode = Mode::Prompt {
                    title: "Shell command".into(),
                    value: String::new(),
                    secret: false,
                    action: PromptAction::Shell,
                }
            }
            KeyCode::Char('0') => {
                self.focus = Focus::Main;
                self.refresh_preview().await;
            }
            KeyCode::Char('1') => {
                self.focus = Focus::Repository;
                self.refresh_preview().await;
            }
            KeyCode::Char('2') => {
                self.focus = Focus::Files;
                self.refresh_preview().await;
            }
            KeyCode::Char('3') => {
                self.focus = Focus::Branches;
                self.refresh_preview().await;
            }
            KeyCode::Char('4') => {
                self.focus = Focus::Revisions;
                self.refresh_preview().await;
            }
            KeyCode::Char('5') => {
                self.focus = Focus::Locks;
                self.refresh_preview().await;
            }
            KeyCode::Char('R') => self.refresh_all(false).await,
            KeyCode::Char('O') => {
                self.offline_forced = !self.offline_forced;
                if self.offline_forced {
                    self.lore.set_offline(true);
                } else {
                    // Re-probe immediately after disabling forced-offline.
                    self.lore.set_offline(false);
                    self.refresh_status(false).await;
                    if !self.is_offline() {
                        self.refresh_branches().await;
                        self.refresh_locks().await;
                    }
                }
            }
            KeyCode::Char('p') => {
                if self.ensure_online().await {
                    self.start(CommandRequest::mutate(["sync"]));
                } else {
                    self.state.preview =
                        vec!["Offline: sync is unavailable while the server is unreachable. Press O to toggle offline mode.".into()];
                }
            }
            KeyCode::Char('P') => {
                if self.ensure_online().await {
                    self.start(CommandRequest::mutate(["push"]));
                } else {
                    self.state.preview =
                        vec!["Offline: push is unavailable while the server is unreachable. Press O to toggle offline mode.".into()];
                }
            }
            KeyCode::Tab
                if self.focus == Focus::Main && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.diff_mode = match self.diff_mode {
                    DiffMode::Working => DiffMode::Staged,
                    DiffMode::Staged => DiffMode::Unstaged,
                    DiffMode::Unstaged => DiffMode::Working,
                };
                self.refresh_preview().await;
            }
            KeyCode::Right | KeyCode::Tab => self.cycle_focus(1).await,
            KeyCode::Left | KeyCode::BackTab => self.cycle_focus(-1).await,
            KeyCode::Esc if self.focus == Focus::CommandLog => {
                self.focus = Focus::Main;
            }
            KeyCode::Esc if self.focus == Focus::Revisions && self.revision_view.is_some() => {
                self.revision_view = None;
                self.refresh_preview().await;
            }
            KeyCode::Esc if self.focus == Focus::Main => {
                self.focus = Focus::Files;
                self.refresh_preview().await;
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1).await,
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1).await,
            KeyCode::PageDown if self.focus == Focus::Main => {
                self.main_scroll = self.main_scroll.saturating_add(10)
            }
            KeyCode::PageUp if self.focus == Focus::Main => {
                self.main_scroll = self.main_scroll.saturating_sub(10)
            }
            KeyCode::Char('.') if self.focus == Focus::CommandLog => {
                const LOG_PAGE: isize = 10;
                self.command_log_selected = moved(
                    self.command_log_selected,
                    self.state.command_history.len(),
                    LOG_PAGE,
                );
            }
            KeyCode::Char(';') if self.focus == Focus::CommandLog => {
                const LOG_PAGE: usize = 10;
                self.command_log_selected = self.command_log_selected.saturating_sub(LOG_PAGE);
            }
            KeyCode::Enter => self.enter_action().await,
            // In tree mode, Space toggles dir collapse; otherwise fall through to space_action.
            KeyCode::Char(' ')
                if self.focus == Focus::Revisions && self.revision_view.is_some() =>
            {
                let (is_dir, path) = {
                    let view = self.revision_view.as_ref().unwrap();
                    let visible = view.visible();
                    visible
                        .get(view.selected)
                        .map(|e| (e.is_dir, e.path.clone()))
                        .unwrap_or((false, String::new()))
                };
                if is_dir {
                    self.toggle_tree_node(path).await;
                }
            }
            KeyCode::Char(' ') => self.space_action(),
            KeyCode::Char('a') if self.focus == Focus::Files => self.stage_all(),
            KeyCode::Char('c') if self.focus == Focus::Files => {
                self.mode = Mode::Prompt {
                    title: "Commit message".into(),
                    value: String::new(),
                    secret: false,
                    action: PromptAction::Commit,
                }
            }
            KeyCode::Char('A') if self.focus == Focus::Files => {
                self.mode = Mode::Prompt {
                    title: "Amended commit message".into(),
                    value: String::new(),
                    secret: false,
                    action: PromptAction::Amend,
                }
            }
            KeyCode::Char('d') => self.delete_action(),
            KeyCode::Char('r') if self.focus == Focus::Files => self.refresh_all(true).await,
            KeyCode::Char('r') if self.focus == Focus::Locks => self.refresh_locks().await,
            KeyCode::Char('n') if self.focus == Focus::Branches => {
                self.mode = Mode::Prompt {
                    title: "New branch name".into(),
                    value: String::new(),
                    secret: false,
                    action: PromptAction::NewBranch,
                }
            }
            KeyCode::Char('M') if self.focus == Focus::Branches => {
                if let Some(branch) = self.selected_branch() {
                    self.start(CommandRequest::mutate([
                        "branch",
                        "merge",
                        branch.name.as_str(),
                    ]));
                }
            }
            KeyCode::Char('g') if self.focus == Focus::Branches => {
                if let Some(branch) = self.selected_branch() {
                    self.mode = Mode::Prompt {
                        title: format!("Reset {} to revision", branch.name),
                        value: String::new(),
                        secret: false,
                        action: PromptAction::ResetBranch {
                            branch: branch.name.clone(),
                        },
                    };
                }
            }
            KeyCode::Char('[') if self.focus == Focus::Branches => self.cycle_branch_tab(-1).await,
            KeyCode::Char(']') if self.focus == Focus::Branches => self.cycle_branch_tab(1).await,
            KeyCode::Char('C')
                if self.focus == Focus::Revisions && self.revision_view.is_none() =>
            {
                self.copied_revision = self
                    .state
                    .revisions
                    .get(self.revision_selected)
                    .map(|r| r.hash.clone());
            }
            KeyCode::Char('V')
                if self.focus == Focus::Revisions && self.revision_view.is_none() =>
            {
                if let Some(revision) = self.copied_revision.clone() {
                    self.start(CommandRequest::mutate([
                        "revision",
                        "cherry-pick",
                        revision.as_str(),
                    ]));
                }
            }
            KeyCode::Char('t')
                if self.focus == Focus::Revisions && self.revision_view.is_none() =>
            {
                self.confirm_revision(ConfirmKind::Revert)
            }
            KeyCode::Char('g')
                if self.focus == Focus::Revisions && self.revision_view.is_none() =>
            {
                self.confirm_revision(ConfirmKind::Reset)
            }
            KeyCode::Char('y')
                if self.focus == Focus::Revisions && self.revision_view.is_none() =>
            {
                self.copy_revision()
            }
            KeyCode::Char('e') if self.focus == Focus::Files => self.open_selected(true),
            KeyCode::Char('o') if self.focus == Focus::Files => self.open_selected(false),
            _ => {}
        }
    }

    async fn move_selection(&mut self, delta: isize) {
        match self.focus {
            Focus::Files => {
                self.file_selected = moved(self.file_selected, self.state.files.len(), delta)
            }
            Focus::Branches => {
                let visible_count = self.visible_branches().len();
                self.branch_selected = moved(self.branch_selected, visible_count, delta)
            }
            Focus::Revisions => {
                if self.revision_view.is_some() {
                    let new_selected = {
                        let view = self.revision_view.as_ref().unwrap();
                        moved(view.selected, view.visible().len(), delta)
                    };
                    self.revision_view.as_mut().unwrap().selected = new_selected;
                    self.preview_revision_file().await;
                    return;
                }
                self.revision_selected =
                    moved(self.revision_selected, self.state.revisions.len(), delta)
            }
            Focus::Locks => {
                self.lock_selected = moved(self.lock_selected, self.state.locks.len(), delta)
            }
            Focus::Main => {
                self.main_scroll = if delta > 0 {
                    self.main_scroll.saturating_add(1)
                } else {
                    self.main_scroll.saturating_sub(1)
                };
                return;
            }
            Focus::CommandLog => {
                self.command_log_selected = moved(
                    self.command_log_selected,
                    self.state.command_history.len(),
                    delta,
                );
                return;
            }
            Focus::Repository => return,
        }
        self.refresh_preview().await;
    }

    async fn enter_action(&mut self) {
        match self.focus {
            Focus::Files => {
                self.focus = Focus::Main;
                self.refresh_preview().await;
            }
            Focus::Branches => {
                if let Some(branch) = self.selected_branch() {
                    self.refresh_history(Some(branch.name.clone())).await;
                    self.focus = Focus::Revisions;
                    self.refresh_preview().await;
                }
            }
            Focus::Revisions => {
                if self.revision_view.is_some() {
                    // Tree mode: Enter on a dir toggles; Enter on a file goes to Main.
                    let (is_dir, entry_path) = {
                        let view = self.revision_view.as_ref().unwrap();
                        let visible = view.visible();
                        visible
                            .get(view.selected)
                            .map(|e| (e.is_dir, e.path.clone()))
                            .unwrap_or((false, String::new()))
                    };
                    if is_dir {
                        self.toggle_tree_node(entry_path).await;
                    } else {
                        self.focus = Focus::Main;
                    }
                } else {
                    // List mode: Enter opens the revision file tree.
                    self.open_revision_tree().await;
                }
            }
            _ => {}
        }
    }

    fn space_action(&mut self) {
        match self.focus {
            Focus::Files => {
                if let Some(file) = self.state.files.get(self.file_selected) {
                    if file.staged {
                        self.start(CommandRequest::mutate(["unstage", file.path.as_str()]));
                    } else {
                        self.start(CommandRequest::mutate(["stage", file.path.as_str()]));
                    }
                }
            }
            Focus::Branches => {
                if let Some(branch) = self.selected_branch() {
                    self.start(CommandRequest::mutate([
                        "branch",
                        "switch",
                        branch.name.as_str(),
                    ]));
                }
            }
            Focus::Revisions if self.revision_view.is_none() => {
                self.confirm_revision(ConfirmKind::Sync)
            }
            Focus::Locks => {
                if let Some(lock) = self.state.locks.get(self.lock_selected) {
                    if lock.locked {
                        self.start(CommandRequest::mutate([
                            "lock",
                            "release",
                            lock.path.as_str(),
                        ]));
                    } else {
                        self.start(CommandRequest::mutate([
                            "lock",
                            "acquire",
                            lock.path.as_str(),
                        ]));
                    }
                }
            }
            _ => {}
        }
    }

    fn stage_all(&mut self) {
        if self.state.files.iter().any(|file| !file.staged) {
            self.start(CommandRequest::mutate(["stage", "."]));
        } else {
            self.start(CommandRequest::mutate(["unstage", "."]));
        }
    }

    fn delete_action(&mut self) {
        match self.focus {
            Focus::Files => {
                if let Some(file) = self.state.files.get(self.file_selected) {
                    self.mode = confirm(
                        "Discard file changes",
                        format!("Reset {}?", file.path),
                        None,
                        ConfirmAction::ResetFile(file.path.clone()),
                    );
                }
            }
            Focus::Branches => {
                if let Some(branch) = self.selected_branch() {
                    self.mode = confirm(
                        "Archive branch",
                        format!("Archive branch {}?", branch.name),
                        None,
                        ConfirmAction::ArchiveBranch(branch.name.clone()),
                    );
                }
            }
            _ => {}
        }
    }

    fn confirm_revision(&mut self, kind: ConfirmKind) {
        let Some(revision) = self.state.revisions.get(self.revision_selected) else {
            return;
        };
        let (title, message, action) = match kind {
            ConfirmKind::Sync => (
                "Synchronize",
                format!("Synchronize to @{}?", revision.number),
                ConfirmAction::SyncRevision(revision.hash.clone()),
            ),
            ConfirmKind::Revert => (
                "Revert revision",
                format!("Revert @{}?", revision.number),
                ConfirmAction::RevertRevision(revision.hash.clone()),
            ),
            ConfirmKind::Reset => (
                "Reset branch",
                format!("Reset current branch to @{}?", revision.number),
                ConfirmAction::ResetToRevision(revision.hash.clone()),
            ),
        };
        self.mode = confirm(title, message, None, action);
    }

    async fn execute_confirm(&mut self, action: ConfirmAction) {
        match action {
            ConfirmAction::ResetFile(path) => {
                self.start(CommandRequest::mutate(["reset", path.as_str()]))
            }
            ConfirmAction::ArchiveBranch(branch) => self.start(CommandRequest::mutate([
                "branch",
                "archive",
                branch.as_str(),
            ])),
            ConfirmAction::SyncRevision(revision) => {
                if !self.ensure_online().await {
                    self.state.preview = vec![
                        "Offline: sync is unavailable while the server is unreachable. Press O to toggle offline mode.".into(),
                    ];
                    return;
                }
                self.start(CommandRequest::mutate(["sync", revision.as_str()]))
            }
            ConfirmAction::RevertRevision(revision) => self.start(CommandRequest::mutate([
                "revision",
                "revert",
                revision.as_str(),
            ])),
            ConfirmAction::ResetToRevision(revision) => self.start(CommandRequest::mutate([
                "branch",
                "reset",
                revision.as_str(),
            ])),
            ConfirmAction::RunCommand { spec, args } => {
                if spec.requires_network && !self.ensure_online().await {
                    self.state.preview = vec![format!(
                        "Offline: `lore {}` requires a server connection. Press O to toggle offline mode.",
                        spec.path
                    )];
                    return;
                }
                self.run_spec(spec, args)
            }
        }
    }

    async fn submit_prompt(&mut self, action: PromptAction, value: String) {
        match action {
            PromptAction::Commit if !value.trim().is_empty() => {
                self.start(CommandRequest::mutate(["commit", value.as_str()]))
            }
            PromptAction::Amend if !value.trim().is_empty() => {
                self.start(CommandRequest::mutate([
                    "revision",
                    "amend",
                    value.as_str(),
                ]))
            }
            PromptAction::NewBranch if !value.trim().is_empty() => {
                self.start(CommandRequest::mutate(["branch", "create", value.as_str()]))
            }
            PromptAction::ResetBranch { branch } if !value.trim().is_empty() => {
                self.mode = confirm(
                    "Reset branch",
                    format!("Reset {branch} to {value}?"),
                    Some("confirm".into()),
                    ConfirmAction::RunCommand {
                        spec: CommandSpec::new("branch reset", "Reset branch", Safety::Destructive),
                        args: vec![value, "--branch".into(), branch],
                    },
                )
            }
            PromptAction::Shell if !value.trim().is_empty() => self.run_shell(&value).await,
            PromptAction::CommandArguments(spec) => match split_arguments(&value) {
                Ok(args) if spec.safety == Safety::Destructive => {
                    // Network check deferred to execute_confirm → RunCommand.
                    self.mode = confirm(
                        "Destructive Lore command",
                        format!("Run lore {}?", spec.path),
                        Some("confirm".into()),
                        ConfirmAction::RunCommand { spec, args },
                    )
                }
                Ok(args) => {
                    if spec.requires_network && !self.ensure_online().await {
                        self.state.preview = vec![format!(
                            "Offline: `lore {}` requires a server connection. Press O to toggle offline mode.",
                            spec.path
                        )];
                        return;
                    }
                    self.run_spec(spec, args)
                }
                Err(error) => self.state.preview = vec![format!("Invalid arguments: {error}")],
            },
            _ => {}
        }
    }

    fn run_spec(&mut self, spec: CommandSpec, tail: Vec<String>) {
        if !spec.available {
            self.state.preview = vec![format!(
                "`lore {}` is unavailable in Lore {}",
                spec.path, self.lore_version
            )];
            return;
        }
        let mut args: Vec<String> = spec.path.split_whitespace().map(str::to_owned).collect();
        args.extend(tail);
        let class = if spec.safety == Safety::ReadOnly {
            CommandClass::Read
        } else {
            CommandClass::Mutating
        };
        self.start(CommandRequest {
            args,
            class,
            cacheable: false,
        });
    }

    async fn run_shell(&mut self, source: &str) {
        let mut command = if cfg!(windows) {
            let mut c = Command::new("cmd.exe");
            c.args(["/C", source]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", source]);
            c
        };
        command.current_dir(self.lore.repository());
        match command.output().await {
            Ok(output) => {
                let mut lines: Vec<String> = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(str::to_owned)
                    .collect();
                lines.extend(
                    String::from_utf8_lossy(&output.stderr)
                        .lines()
                        .map(|line| format!("! {line}")),
                );
                self.state.preview = lines;
            }
            Err(error) => self.state.preview = vec![error.to_string()],
        }
        self.focus = Focus::Main;
    }

    fn copy_revision(&mut self) {
        let Some(revision) = self.state.revisions.get(self.revision_selected) else {
            return;
        };
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(revision.hash.clone());
        }
        self.copied_revision = Some(revision.hash.clone());
    }

    fn open_selected(&mut self, edit: bool) {
        let Some(file) = self.state.files.get(self.file_selected) else {
            return;
        };
        let path = self.state.repository.root.join(&file.path);
        if edit {
            let editor = self
                .config
                .tools
                .editor
                .clone()
                .or_else(|| std::env::var("VISUAL").ok())
                .or_else(|| std::env::var("EDITOR").ok());
            if let Some(editor) = editor {
                let _ = std::process::Command::new(editor).arg(path).spawn();
                return;
            }
        }
        let _ = open::that(path);
    }

    pub fn filtered_commands(&self, query: &str) -> Vec<&CommandSpec> {
        filtered_commands(&self.commands, query)
    }
}

fn action_keys(focus: Focus) -> Vec<(&'static str, KeyCode, KeyModifiers)> {
    let mut keys = vec![
        ("quit", KeyCode::Char('q'), KeyModifiers::NONE),
        ("help", KeyCode::Char('?'), KeyModifiers::NONE),
        ("command_log", KeyCode::Char('@'), KeyModifiers::NONE),
        ("command_palette", KeyCode::Char('p'), KeyModifiers::CONTROL),
        ("shell", KeyCode::Char(':'), KeyModifiers::NONE),
        ("refresh", KeyCode::Char('R'), KeyModifiers::NONE),
        ("sync", KeyCode::Char('p'), KeyModifiers::NONE),
        ("push", KeyCode::Char('P'), KeyModifiers::NONE),
        ("focus_main", KeyCode::Char('0'), KeyModifiers::NONE),
        ("focus_repository", KeyCode::Char('1'), KeyModifiers::NONE),
        ("focus_files", KeyCode::Char('2'), KeyModifiers::NONE),
        ("focus_branches", KeyCode::Char('3'), KeyModifiers::NONE),
        ("focus_revisions", KeyCode::Char('4'), KeyModifiers::NONE),
        ("focus_locks", KeyCode::Char('5'), KeyModifiers::NONE),
        ("pane_next", KeyCode::Tab, KeyModifiers::NONE),
        ("pane_previous", KeyCode::BackTab, KeyModifiers::SHIFT),
    ];
    match focus {
        Focus::Files => keys.extend([
            ("file_stage", KeyCode::Char(' '), KeyModifiers::NONE),
            ("file_stage_all", KeyCode::Char('a'), KeyModifiers::NONE),
            ("file_commit", KeyCode::Char('c'), KeyModifiers::NONE),
            ("file_amend", KeyCode::Char('A'), KeyModifiers::NONE),
            ("file_discard", KeyCode::Char('d'), KeyModifiers::NONE),
            ("file_scan", KeyCode::Char('r'), KeyModifiers::NONE),
            ("file_edit", KeyCode::Char('e'), KeyModifiers::NONE),
            ("file_open", KeyCode::Char('o'), KeyModifiers::NONE),
        ]),
        Focus::Branches => keys.extend([
            ("branch_switch", KeyCode::Char(' '), KeyModifiers::NONE),
            ("branch_new", KeyCode::Char('n'), KeyModifiers::NONE),
            ("branch_archive", KeyCode::Char('d'), KeyModifiers::NONE),
            ("branch_merge", KeyCode::Char('M'), KeyModifiers::NONE),
            ("branch_reset", KeyCode::Char('g'), KeyModifiers::NONE),
            ("branch_tab_prev", KeyCode::Char('['), KeyModifiers::NONE),
            ("branch_tab_next", KeyCode::Char(']'), KeyModifiers::NONE),
        ]),
        Focus::Revisions => keys.extend([
            ("revision_sync", KeyCode::Char(' '), KeyModifiers::NONE),
            ("revision_copy", KeyCode::Char('C'), KeyModifiers::NONE),
            ("revision_paste", KeyCode::Char('V'), KeyModifiers::NONE),
            ("revision_revert", KeyCode::Char('t'), KeyModifiers::NONE),
            ("revision_reset", KeyCode::Char('g'), KeyModifiers::NONE),
            ("revision_copy_hash", KeyCode::Char('y'), KeyModifiers::NONE),
        ]),
        Focus::Locks => keys.extend([
            ("lock_toggle", KeyCode::Char(' '), KeyModifiers::NONE),
            ("lock_refresh", KeyCode::Char('r'), KeyModifiers::NONE),
        ]),
        Focus::Main => keys.push(("diff_mode", KeyCode::Tab, KeyModifiers::CONTROL)),
        Focus::CommandLog => keys.extend([
            (
                "command_log_page_up",
                KeyCode::Char(';'),
                KeyModifiers::NONE,
            ),
            (
                "command_log_page_down",
                KeyCode::Char('.'),
                KeyModifiers::NONE,
            ),
        ]),
        Focus::Repository => {}
    }
    keys
}

fn binding_matches(binding: &str, key: KeyEvent) -> bool {
    let mut modifiers = KeyModifiers::NONE;
    let parts: Vec<&str> = binding.split('+').collect();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers.insert(KeyModifiers::CONTROL),
            "alt" => modifiers.insert(KeyModifiers::ALT),
            "shift" => modifiers.insert(KeyModifiers::SHIFT),
            _ => return false,
        }
    }
    let name = parts.last().copied().unwrap_or(binding);
    let code = match name.to_ascii_lowercase().as_str() {
        "space" => KeyCode::Char(' '),
        "enter" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "esc" | "escape" => KeyCode::Esc,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        _ if name.chars().count() == 1 => KeyCode::Char(name.chars().next().expect("one char")),
        _ => return false,
    };
    key_matches(key, code, modifiers)
}

fn key_matches(key: KeyEvent, code: KeyCode, modifiers: KeyModifiers) -> bool {
    key.code == code
        && key.modifiers.contains(modifiers)
        && (!key.modifiers.contains(KeyModifiers::CONTROL)
            || modifiers.contains(KeyModifiers::CONTROL))
        && (!key.modifiers.contains(KeyModifiers::ALT) || modifiers.contains(KeyModifiers::ALT))
}

#[derive(Clone, Copy)]
enum ConfirmKind {
    Sync,
    Revert,
    Reset,
}

fn filtered_commands<'a>(commands: &'a [CommandSpec], query: &str) -> Vec<&'a CommandSpec> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(|word| word.to_ascii_lowercase())
        .collect();
    commands
        .iter()
        .filter(|command| {
            let haystack = format!("{} {}", command.path, command.description).to_ascii_lowercase();
            words.iter().all(|word| haystack.contains(word))
        })
        .collect()
}

fn command_error(events: &[crate::model::LoreEvent], stderr: &str) -> String {
    events
        .iter()
        .find_map(event_error)
        .or_else(|| (!stderr.trim().is_empty()).then(|| stderr.trim().to_owned()))
        .unwrap_or_else(|| "Lore command failed".into())
}

fn confirm(
    title: impl Into<String>,
    message: impl Into<String>,
    required: Option<String>,
    action: ConfirmAction,
) -> Mode {
    Mode::Confirm {
        title: title.into(),
        message: message.into(),
        required,
        typed: String::new(),
        action,
    }
}

fn clamp_selection(selected: usize, len: usize) -> usize {
    if len == 0 { 0 } else { selected.min(len - 1) }
}
fn moved(selected: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        0
    } else {
        selected.saturating_add_signed(delta).min(len - 1)
    }
}
fn first_non_empty(values: &[String]) -> String {
    values
        .iter()
        .find(|value| !value.is_empty())
        .cloned()
        .unwrap_or_default()
}
fn is_zero_hash(hash: &str) -> bool {
    hash.is_empty() || hash.chars().all(|ch| ch == '0')
}

/// Extract patch lines from `fileDiff` events (shared by `preview_file` and `preview_revision_file`).
fn diff_preview_lines(events: &[LoreEvent]) -> Vec<String> {
    events
        .iter()
        .filter(|e| e.tag == "fileDiff")
        .flat_map(|e| {
            field_string(&e.data, "patch")
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Return the changed files from `revisionInfoDelta` events as `(path, marker)` pairs.
/// Marker is `'A'` (add), `'M'` (modified keep), or `'D'` (delete/remove).
/// Directory entries and truly-unchanged `keep` files are excluded.
fn revision_changed_files(events: &[LoreEvent]) -> Vec<(String, char)> {
    let mut files: Vec<(String, char)> = events
        .iter()
        .filter(|e| e.tag == "revisionInfoDelta")
        .filter(|e| field_bool(&e.data, "flagFile"))
        .filter_map(|e| {
            let marker = match field_string(&e.data, "action")
                .to_ascii_lowercase()
                .as_str()
            {
                "add" => Some('A'),
                "delete" | "remove" => Some('D'),
                "keep" if field_bool(&e.data, "flagModify") => Some('M'),
                _ => None,
            };
            marker.map(|m| (field_string(&e.data, "path"), m))
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

/// Build a lazygit-style readout for a single revision, returned as lines for `state.preview`.
///
/// Expects the events from `lore revision info <hash> --delta --json`.
/// Emits a header block (revision number, hash, commit message, author, date, parents)
/// followed by a sorted list of changed files with A / M / D markers.
fn build_revision_readout(events: &[LoreEvent]) -> Vec<String> {
    let mut rev_number: u64 = 0;
    let mut rev_hash = String::new();
    let mut parents: Vec<String> = Vec::new();
    let mut message = String::new();
    let mut timestamp_ms: Option<u64> = None;
    let mut created_by = String::new();
    let mut committed_by = String::new();

    for event in events {
        match event.tag.as_str() {
            "revisionInfo" => {
                rev_number = field_u64(&event.data, "revisionNumber");
                rev_hash = first_non_empty(&[
                    field_string(&event.data, "revision"),
                    field_string(&event.data, "signature"),
                ]);
                parents = event
                    .data
                    .get("parent")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .map(|v| match v {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
            }
            "metadata" => {
                let key = field_string(&event.data, "key").to_ascii_lowercase();
                match key.as_str() {
                    "message" => {
                        message = event
                            .data
                            .get("value")
                            .and_then(|v| v.get("data"))
                            .and_then(|v| v.as_str())
                            .map(str::to_owned)
                            .unwrap_or_else(|| field_string(&event.data, "value"));
                    }
                    "created-by" => {
                        created_by = event
                            .data
                            .get("value")
                            .and_then(|v| v.get("data"))
                            .and_then(|v| v.as_str())
                            .map(str::to_owned)
                            .unwrap_or_default();
                    }
                    "committed-by" => {
                        committed_by = event
                            .data
                            .get("value")
                            .and_then(|v| v.get("data"))
                            .and_then(|v| v.as_str())
                            .map(str::to_owned)
                            .unwrap_or_default();
                    }
                    "timestamp" => {
                        timestamp_ms = event
                            .data
                            .get("value")
                            .and_then(|v| v.get("data"))
                            .and_then(|v| v.as_u64());
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // --- Header block ---
    let short = rev_hash.get(..rev_hash.len().min(8)).unwrap_or(&rev_hash);
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("@{}  {}", rev_number, short));
    if !message.is_empty() {
        lines.push(message);
    }
    lines.push(String::new());
    if !created_by.is_empty() {
        lines.push(format!("Author    {}", created_by));
    }
    if !committed_by.is_empty() && committed_by != created_by {
        lines.push(format!("Commit    {}", committed_by));
    }
    if let Some(ms) = timestamp_ms {
        lines.push(format!("Date      {}", format_timestamp(ms)));
    }
    let real_parents: Vec<String> = parents
        .iter()
        .filter(|p| !is_zero_hash(p))
        .map(|p| p.get(..p.len().min(8)).unwrap_or(p).to_owned())
        .collect();
    if !real_parents.is_empty() {
        lines.push(format!("Parents   {}", real_parents.join(", ")));
    }

    // --- File list (reuses revision_changed_files for consistent filtering) ---
    let changed = revision_changed_files(events);
    let total = changed.len();
    lines.push(String::new());
    lines.push(format!(
        "{} file{} changed",
        total,
        if total == 1 { "" } else { "s" }
    ));

    const MAX_FILES: usize = 500;
    let shown = changed.len().min(MAX_FILES);
    for (path, marker) in &changed[..shown] {
        lines.push(format!("{}   {}", marker, path));
    }
    if changed.len() > MAX_FILES {
        lines.push(format!("… and {} more", changed.len() - MAX_FILES));
    }

    lines
}

/// Format a Unix timestamp in milliseconds as `YYYY-MM-DD HH:MM` (UTC).
/// Uses the Neri/Schneider days-from-civil algorithm; no external crate required.
fn format_timestamp(ms: u64) -> String {
    let secs = ms / 1000;
    let days = secs / 86400;
    let rem = secs % 86400;
    let hh = rem / 3600;
    let mm = (rem % 3600) / 60;

    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{:04}-{:02}-{:02} {:02}:{:02}", y, m, d, hh, mm)
}

impl App {
    /// Open the revision file-tree drill-down for the currently selected revision.
    async fn open_revision_tree(&mut self) {
        let Some(revision) = self.state.revisions.get(self.revision_selected) else {
            return;
        };
        let hash = revision.hash.clone();
        let number = revision.number;
        let parent = revision.parents.iter().find(|p| !is_zero_hash(p)).cloned();

        let args = vec![
            "revision".to_string(),
            "info".into(),
            hash.clone(),
            "--delta".into(),
        ];
        // Keyed on an immutable revision hash: safe to memoize.
        let Ok(output) = self.lore.capture(CommandRequest::cached_read(args)).await else {
            return;
        };
        self.push_record(output.record);
        let files = revision_changed_files(&output.events);
        if files.is_empty() {
            return; // nothing to browse; stay in list mode
        }
        let entries = crate::tree::build_tree(&files);
        self.revision_view = Some(RevisionView {
            number,
            hash,
            parent,
            entries,
            collapsed: HashSet::new(),
            selected: 0,
        });
        self.preview_revision_file().await;
    }

    /// Update `state.preview` for whichever tree entry is currently selected.
    async fn preview_revision_file(&mut self) {
        self.main_scroll = 0;
        // Extract what we need before any async calls to avoid borrow conflicts.
        let info = {
            let view = match &self.revision_view {
                Some(v) => v,
                None => return,
            };
            let visible = view.visible();
            let entry = match visible.get(view.selected) {
                Some(e) => e,
                None => return,
            };
            (
                view.number,
                view.hash.clone(),
                view.parent.clone(),
                entry.path.clone(),
                entry.is_dir,
                entry.label.clone(),
                view.collapsed.contains(&entry.path),
            )
        };
        let (number, hash, parent, path, is_dir, label, is_collapsed) = info;

        if is_dir {
            let arrow = if is_collapsed { "▶" } else { "▼" };
            self.state.preview = vec![format!(
                "{} {}  (Space or Enter to expand/collapse)",
                arrow, label
            )];
            return;
        }

        let Some(parent_hash) = parent else {
            let short = hash.get(..hash.len().min(8)).unwrap_or(&hash);
            self.state.preview = vec![
                format!("@{}  {}", number, short),
                String::new(),
                format!("{} — added in initial revision", path),
                "No parent revision to diff against.".into(),
            ];
            return;
        };

        let args = vec![
            "diff".to_string(),
            "--source".into(),
            parent_hash,
            "--target".into(),
            hash,
            path,
        ];
        // Both endpoints are immutable revision hashes: safe to memoize.
        if let Ok(output) = self.lore.capture(CommandRequest::cached_read(args)).await {
            self.push_record(output.record.clone());
            self.state.preview = if output.timed_out {
                vec!["Diff timed out.".into()]
            } else if !output.record.success {
                vec![format!(
                    "Diff unavailable: {}",
                    command_error(&output.events, &output.record.stderr)
                )]
            } else {
                let patches = diff_preview_lines(&output.events);
                if patches.is_empty() {
                    vec!["No text patch available. This may be binary content.".into()]
                } else {
                    patches
                }
            };
        }
    }

    /// Toggle expand/collapse for the given directory path and refresh the preview.
    async fn toggle_tree_node(&mut self, dir_path: String) {
        if let Some(view) = &mut self.revision_view {
            if view.collapsed.contains(&dir_path) {
                view.collapsed.remove(&dir_path);
            } else {
                view.collapsed.insert(dir_path);
            }
            let new_len = view.visible().len();
            view.selected = clamp_selection(view.selected, new_len);
        }
        self.preview_revision_file().await;
    }
}

pub fn resolve_repository(path: &Path) -> Result<PathBuf> {
    dunce::canonicalize(path)
        .with_context(|| format!("invalid repository path {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_palette_by_all_words() {
        let commands = baseline_commands();
        let matches = filtered_commands(&commands, "branch reset");
        assert!(matches.iter().any(|command| command.path == "branch reset"));
        assert!(matches.iter().all(|command| command.path.contains("branch")
            || command.description.to_lowercase().contains("branch")));
    }

    #[test]
    fn selection_never_exceeds_list() {
        assert_eq!(moved(4, 2, 1), 1);
        assert_eq!(moved(0, 2, -1), 0);
    }

    #[tokio::test]
    async fn pane_focus_cycles_and_wraps() {
        let mut app = App::test_fixture();
        app.focus = Focus::Main;
        app.cycle_focus(1).await;
        assert_eq!(app.focus, Focus::Repository);
        app.cycle_focus(-1).await;
        assert_eq!(app.focus, Focus::Main);
    }

    #[test]
    fn format_timestamp_known_value() {
        // 1782758179015 ms → 2026-06-29 18:36 UTC (verified against civil algorithm)
        assert_eq!(format_timestamp(1_782_758_179_015), "2026-06-29 18:36");
        // Unix epoch itself
        assert_eq!(format_timestamp(0), "1970-01-01 00:00");
    }

    #[test]
    fn build_revision_readout_header_and_markers() {
        use crate::model::LoreEvent;
        use serde_json::json;

        let events = vec![
            LoreEvent {
                tag: "revisionInfo".into(),
                data: json!({
                    "revision": "abcdef1234567890",
                    "revisionNumber": 7,
                    "parent": ["0000000000000000000000000000000000000000000000000000000000000000"]
                }),
            },
            LoreEvent {
                tag: "metadata".into(),
                data: json!({"key": "message", "value": {"tagName": "string", "data": "fix: something"}}),
            },
            LoreEvent {
                tag: "metadata".into(),
                data: json!({"key": "created-by", "value": {"tagName": "string", "data": "alice@example.com"}}),
            },
            LoreEvent {
                tag: "metadata".into(),
                data: json!({"key": "timestamp", "value": {"tagName": "numeric", "data": 0_u64}}),
            },
            // directory entry — should be skipped
            LoreEvent {
                tag: "revisionInfoDelta".into(),
                data: json!({"path": "src", "action": "add", "flagModify": false, "flagFile": false}),
            },
            // unchanged keep — should be skipped
            LoreEvent {
                tag: "revisionInfoDelta".into(),
                data: json!({"path": "README.md", "action": "keep", "flagModify": false, "flagFile": true}),
            },
            LoreEvent {
                tag: "revisionInfoDelta".into(),
                data: json!({"path": "src/lib.rs", "action": "add", "flagModify": false, "flagFile": true}),
            },
            LoreEvent {
                tag: "revisionInfoDelta".into(),
                data: json!({"path": "src/main.rs", "action": "keep", "flagModify": true, "flagFile": true}),
            },
            LoreEvent {
                tag: "revisionInfoDelta".into(),
                data: json!({"path": "old.rs", "action": "delete", "flagModify": false, "flagFile": true}),
            },
        ];

        let lines = build_revision_readout(&events);

        assert!(
            lines[0].contains("@7") && lines[0].contains("abcdef12"),
            "header line: {}",
            lines[0]
        );
        assert!(
            lines.iter().any(|l| l.contains("fix: something")),
            "message missing"
        );
        assert!(
            lines.iter().any(|l| l.contains("alice@example.com")),
            "author missing"
        );
        assert!(
            lines.iter().any(|l| l.contains("1970-01-01")),
            "date missing"
        );
        // null parent should be filtered out
        assert!(
            !lines.iter().any(|l| l.contains("Parents")),
            "null parent should be hidden"
        );
        assert!(
            lines.iter().any(|l| l == "A   src/lib.rs"),
            "A marker expected"
        );
        assert!(
            lines.iter().any(|l| l == "M   src/main.rs"),
            "M marker expected"
        );
        assert!(lines.iter().any(|l| l == "D   old.rs"), "D marker expected");
        // dir entry and unchanged keep must not appear
        assert!(
            !lines
                .iter()
                .any(|l| l.ends_with("src") && l.starts_with("A")),
            "dir entry leaked"
        );
        assert!(
            !lines.iter().any(|l| l.ends_with("README.md")),
            "unchanged keep leaked"
        );
        // summary line
        assert!(
            lines.iter().any(|l| l.contains("3 files changed")),
            "summary line missing"
        );
    }

    #[test]
    fn parses_configured_key_combinations() {
        let ctrl_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert!(binding_matches("ctrl+x", ctrl_x));
        assert!(binding_matches(
            "space",
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)
        ));
        assert!(!binding_matches("alt+x", ctrl_x));
    }

    #[test]
    fn branch_sync_current_branch() {
        let mut app = App::test_fixture();
        let current = app
            .state
            .branches
            .iter()
            .find(|b| b.current)
            .unwrap()
            .clone();

        // Both flags false → InSync.
        app.state.repository.local_ahead = false;
        app.state.repository.remote_ahead = false;
        assert_eq!(app.branch_sync(&current), BranchSync::InSync);

        // Local ahead only → Ahead.
        app.state.repository.local_ahead = true;
        app.state.repository.remote_ahead = false;
        assert_eq!(app.branch_sync(&current), BranchSync::Ahead);

        // Remote ahead only → Behind.
        app.state.repository.local_ahead = false;
        app.state.repository.remote_ahead = true;
        assert_eq!(app.branch_sync(&current), BranchSync::Behind);

        // Both ahead → Diverged.
        app.state.repository.local_ahead = true;
        app.state.repository.remote_ahead = true;
        assert_eq!(app.branch_sync(&current), BranchSync::Diverged);
    }

    #[test]
    fn branch_sync_non_current_local() {
        let mut app = App::test_fixture();

        // Add a non-current local branch and a matching remote with same hash → InSync.
        let hash = "aabbccdd";
        app.state.branches.push(Branch {
            name: "feature".into(),
            location: "local".into(),
            latest: hash.into(),
            current: false,
            ..Branch::default()
        });
        app.state.branches.push(Branch {
            name: "feature".into(),
            location: "remote".into(),
            latest: hash.into(),
            current: false,
            ..Branch::default()
        });
        let local = app
            .state
            .branches
            .iter()
            .find(|b| b.name == "feature" && b.location == "local")
            .unwrap()
            .clone();
        assert_eq!(app.branch_sync(&local), BranchSync::InSync);

        // Diverged hashes → Differs.
        let idx = app
            .state
            .branches
            .iter()
            .position(|b| b.name == "feature" && b.location == "remote")
            .unwrap();
        app.state.branches[idx].latest = "11223344".into();
        assert_eq!(app.branch_sync(&local), BranchSync::Differs);

        // No remote counterpart → Untracked.
        app.state
            .branches
            .retain(|b| !(b.name == "feature" && b.location == "remote"));
        assert_eq!(app.branch_sync(&local), BranchSync::Untracked);
    }

    // --- Offline mode tests ---

    #[test]
    fn is_offline_when_forced() {
        let mut app = App::test_fixture();
        app.state.repository.remote_available = true;
        app.offline_forced = true;
        assert!(
            app.is_offline(),
            "forced offline should report offline even when remote_available"
        );
    }

    #[test]
    fn is_online_when_remote_available_and_not_forced() {
        let mut app = App::test_fixture();
        app.state.repository.remote_available = true;
        app.offline_forced = false;
        assert!(!app.is_offline());
    }

    #[test]
    fn is_offline_when_remote_unavailable() {
        let mut app = App::test_fixture();
        app.state.repository.remote_available = false;
        app.offline_forced = false;
        assert!(app.is_offline());
    }

    #[test]
    fn offline_toggle_switches_state() {
        let mut app = App::test_fixture();
        assert!(!app.offline_forced);
        app.offline_forced = true;
        assert!(app.offline_forced);
        app.offline_forced = false;
        assert!(!app.offline_forced);
    }

    #[test]
    fn config_defaults_are_sane() {
        let config = Config::default();
        // command_timeout: min 500 ms floor, default 3000
        assert_eq!(config.general.command_timeout_ms, 3_000);
        assert_eq!(
            config.command_timeout(),
            std::time::Duration::from_millis(3_000)
        );
        // reconnect_interval: min 5 s floor, default 30 s
        assert_eq!(config.general.reconnect_interval_ms, 30_000);
        assert_eq!(
            config.reconnect_interval(),
            std::time::Duration::from_millis(30_000)
        );
        // offline defaults to false
        assert!(!config.general.offline);
    }

    #[test]
    fn config_floors_are_enforced() {
        let mut config = Config::default();
        config.general.command_timeout_ms = 100; // below 500 ms floor
        assert_eq!(
            config.command_timeout(),
            std::time::Duration::from_millis(500)
        );
        config.general.reconnect_interval_ms = 1_000; // below 5 s floor
        assert_eq!(
            config.reconnect_interval(),
            std::time::Duration::from_millis(5_000)
        );
    }

    #[test]
    fn network_commands_are_tagged_in_baseline() {
        use crate::manifest::baseline_commands;
        let commands = baseline_commands();
        let requires_network = |path: &str| {
            commands
                .iter()
                .find(|c| c.path == path)
                .map(|c| c.requires_network)
                .unwrap_or(false)
        };
        assert!(requires_network("sync"), "sync must require network");
        assert!(requires_network("push"), "push must require network");
        assert!(
            requires_network("lock acquire"),
            "lock acquire must require network"
        );
        assert!(
            requires_network("lock release"),
            "lock release must require network"
        );
        assert!(
            requires_network("branch push"),
            "branch push must require network"
        );
        // local-only commands must not be marked
        assert!(
            !requires_network("commit"),
            "commit must not require network"
        );
        assert!(!requires_network("stage"), "stage must not require network");
        assert!(
            !requires_network("history"),
            "history must not require network"
        );
    }
}
