use std::{collections::VecDeque, path::PathBuf, time::Duration};

use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct RepositoryState {
    pub root: PathBuf,
    pub repository: String,
    pub branch: String,
    pub branch_id: String,
    pub revision: String,
    pub revision_number: u64,
    pub staged_revision: String,
    pub remote_revision: String,
    pub local_ahead: bool,
    pub remote_ahead: bool,
    pub remote_available: bool,
    pub remote_authorized: bool,
    pub stale: bool,
}

#[derive(Debug, Clone, Default)]
pub struct FileStatus {
    pub path: String,
    pub from_path: String,
    pub size: u64,
    pub action: String,
    pub node_type: String,
    pub staged: bool,
    pub dirty: bool,
    pub conflict: bool,
    pub unresolved: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Branch {
    pub name: String,
    pub id: String,
    pub location: String,
    pub latest: String,
    pub creator: String,
    pub current: bool,
    pub archived: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Revision {
    pub hash: String,
    pub number: u64,
    pub message: String,
    pub parents: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct FileLock {
    pub path: String,
    pub branch: String,
    pub owner: String,
    pub locked: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Conflict {
    pub path: String,
    pub operation: String,
    pub resolved: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LinkInfo {
    pub path: String,
    pub repository: String,
    pub revision: String,
}

#[derive(Debug, Clone, Default)]
pub struct LayerInfo {
    pub path: String,
    pub repository: String,
}

#[derive(Debug, Clone)]
pub struct LoreEvent {
    pub tag: String,
    pub data: Value,
}

#[derive(Debug, Clone)]
pub struct CommandRecord {
    pub argv: Vec<String>,
    pub display: String,
    pub success: bool,
    pub status: Option<i32>,
    pub duration: Duration,
    pub stderr: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    Repository,
    #[default]
    Files,
    Branches,
    Revisions,
    Locks,
    Main,
    CommandLog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffMode {
    #[default]
    Working,
    Staged,
    Unstaged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BranchTab {
    #[default]
    Local,
    Remote,
}

/// Sync relationship between a local branch and its remote counterpart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchSync {
    /// Local and remote are in sync.
    InSync,
    /// Local is ahead of remote (current branch only, from Lore booleans).
    Ahead,
    /// Remote is ahead of local (current branch only, from Lore booleans).
    Behind,
    /// Both local and remote have diverged (current branch only).
    Diverged,
    /// Non-current local branch whose `latest` hash differs from the remote counterpart.
    Differs,
    /// No same-named remote branch found, or comparison not applicable.
    Untracked,
}

impl BranchTab {
    pub fn matches(self, branch: &Branch) -> bool {
        let is_remote = branch.location.to_ascii_lowercase().contains("remote");
        match self {
            BranchTab::Local => !is_remote,
            BranchTab::Remote => is_remote,
        }
    }
}

#[derive(Debug, Default)]
pub struct AppState {
    pub repository: RepositoryState,
    pub files: Vec<FileStatus>,
    pub branches: Vec<Branch>,
    pub revisions: Vec<Revision>,
    pub locks: Vec<FileLock>,
    pub conflicts: Vec<Conflict>,
    pub links: Vec<LinkInfo>,
    pub layers: Vec<LayerInfo>,
    pub preview: Vec<String>,
    pub operation_output: VecDeque<String>,
    pub command_history: VecDeque<CommandRecord>,
    pub repository_error: Option<String>,
    pub busy: bool,
    pub progress: Option<String>,
}

pub fn field_string(value: &Value, name: &str) -> String {
    let Some(value) = value.get(name) else {
        return String::new();
    };
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Array(values) if values.iter().all(Value::is_number) => values
            .iter()
            .filter_map(Value::as_u64)
            .map(|n| format!("{n:02x}"))
            .collect(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub fn field_bool(value: &Value, name: &str) -> bool {
    value
        .get(name)
        .and_then(|v| match v {
            Value::Bool(flag) => Some(*flag),
            Value::Number(number) => number.as_u64().map(|n| n != 0),
            _ => None,
        })
        .unwrap_or(false)
}

pub fn field_u64(value: &Value, name: &str) -> u64 {
    value.get(name).and_then(Value::as_u64).unwrap_or_default()
}
