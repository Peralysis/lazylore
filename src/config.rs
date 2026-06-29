use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub general: GeneralConfig,
    pub ui: UiConfig,
    pub tools: ToolConfig,
    pub keybindings: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub lore_binary: PathBuf,
    pub refresh_interval_ms: u64,
    pub watch_files: bool,
    pub scan_on_start: bool,
    pub history_page_size: usize,
    pub confirm_destructive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub mouse: bool,
    pub file_tree: bool,
    pub theme: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolConfig {
    pub editor: Option<String>,
    pub opener: Option<String>,
    pub diff_tool: Option<String>,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            lore_binary: PathBuf::from("lore"),
            refresh_interval_ms: 2_000,
            watch_files: true,
            scan_on_start: false,
            history_page_size: 100,
            confirm_destructive: true,
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            mouse: true,
            file_tree: true,
            theme: "default".into(),
        }
    }
}

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        ProjectDirs::from("dev", "lazylore", "lazylore")
            .map(|dirs| dirs.config_dir().join("config.toml"))
    }

    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = path.map(Path::to_path_buf).or_else(Self::default_path);
        let Some(path) = path else {
            return Ok(Self::default());
        };
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("failed to parse config {}", path.display()))
    }

    pub fn refresh_interval(&self) -> Duration {
        Duration::from_millis(self.general.refresh_interval_ms.max(250))
    }
}
