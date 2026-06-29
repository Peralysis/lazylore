use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, Result, anyhow, bail};
use semver::Version;
use serde::Deserialize;
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
    sync::{Mutex, broadcast, mpsc},
};

use crate::{
    manifest::redact,
    model::{CommandRecord, LoreEvent},
};

const MINIMUM_LORE_VERSION: Version = Version::new(0, 8, 4);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandClass {
    Read,
    Mutating,
}

#[derive(Debug, Clone)]
pub struct CommandRequest {
    pub args: Vec<String>,
    pub class: CommandClass,
}

impl CommandRequest {
    pub fn read(args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            class: CommandClass::Read,
        }
    }

    pub fn mutate(args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            class: CommandClass::Mutating,
        }
    }
}

#[derive(Debug)]
pub struct CommandOutput {
    pub events: Vec<LoreEvent>,
    pub record: CommandRecord,
}

#[derive(Debug)]
pub enum StreamMessage {
    Started { display: String },
    Event(LoreEvent),
    Stderr(String),
    Finished(CommandRecord),
}

#[derive(Clone)]
pub struct LoreClient {
    binary: PathBuf,
    repository: PathBuf,
    mutation_lock: Arc<Mutex<()>>,
    cancel_tx: broadcast::Sender<()>,
}

#[derive(Deserialize)]
struct RawEvent {
    #[serde(rename = "tagName")]
    tag_name: String,
    #[serde(default)]
    data: Value,
}

impl LoreClient {
    pub fn new(binary: impl Into<PathBuf>, repository: impl Into<PathBuf>) -> Self {
        let (cancel_tx, _) = broadcast::channel(1);
        Self {
            binary: binary.into(),
            repository: repository.into(),
            mutation_lock: Arc::new(Mutex::new(())),
            cancel_tx,
        }
    }

    pub fn repository(&self) -> &Path {
        &self.repository
    }

    pub fn cancel_current(&self) {
        let _ = self.cancel_tx.send(());
    }

    pub async fn validate(&self) -> Result<Version> {
        let output = Command::new(&self.binary)
            .arg("--version")
            .output()
            .await
            .with_context(|| format!("could not execute Lore CLI at {}", self.binary.display()))?;
        if !output.status.success() {
            bail!("`lore --version` failed")
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let version = text
            .split_whitespace()
            .find_map(|token| {
                let clean = token.trim_start_matches('v').split(['+', '-']).next()?;
                Version::parse(clean).ok()
            })
            .ok_or_else(|| anyhow!("could not parse Lore version from `{}`", text.trim()))?;
        if version < MINIMUM_LORE_VERSION {
            bail!(
                "Lore {version} is unsupported; LazyLore requires Lore {MINIMUM_LORE_VERSION} or newer"
            )
        }
        Ok(version)
    }

    pub async fn markdown_help(&self) -> Result<String> {
        let output = Command::new(&self.binary)
            .arg("--markdown-help")
            .output()
            .await
            .context("failed to inspect Lore capabilities")?;
        if !output.status.success() {
            bail!("Lore did not provide --markdown-help")
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn command(&self, args: &[String]) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .args(["--json", "--no-pager", "--non-interactive", "--repository"])
            .arg(&self.repository)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    pub async fn capture(&self, request: CommandRequest) -> Result<CommandOutput> {
        let _guard = if request.class == CommandClass::Mutating {
            Some(self.mutation_lock.lock().await)
        } else {
            None
        };
        let started = Instant::now();
        let output = self
            .command(&request.args)
            .output()
            .await
            .with_context(|| format!("failed to execute {}", display_command(&request.args)))?;
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let events = parse_events(&output.stdout, &stderr)?;
        let status = output.status.code();
        let event_failure = completion_failed(&events);
        let success = output.status.success() && !event_failure;
        Ok(CommandOutput {
            events,
            record: CommandRecord {
                argv: redact(&request.args),
                display: display_command(&redact(&request.args)),
                success,
                status,
                duration: started.elapsed(),
                stderr,
            },
        })
    }

    pub fn stream(&self, request: CommandRequest, tx: mpsc::UnboundedSender<StreamMessage>) {
        let client = self.clone();
        tokio::spawn(async move {
            let mut cancel_rx = client.cancel_tx.subscribe();
            let _guard = if request.class == CommandClass::Mutating {
                Some(client.mutation_lock.clone().lock_owned().await)
            } else {
                None
            };
            let redacted = redact(&request.args);
            let display = display_command(&redacted);
            let _ = tx.send(StreamMessage::Started {
                display: display.clone(),
            });
            let started = Instant::now();
            let mut child = match client.command(&request.args).spawn() {
                Ok(child) => child,
                Err(error) => {
                    let _ = tx.send(StreamMessage::Stderr(error.to_string()));
                    let _ = tx.send(StreamMessage::Finished(CommandRecord {
                        argv: redacted,
                        display,
                        success: false,
                        status: None,
                        duration: started.elapsed(),
                        stderr: error.to_string(),
                    }));
                    return;
                }
            };
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let stderr_task = tokio::spawn(async move {
                let mut text = String::new();
                if let Some(mut stderr) = stderr {
                    let _ = stderr.read_to_string(&mut text).await;
                }
                text
            });
            let mut event_failure = false;
            let mut primary_complete_seen = false;
            if let Some(stdout) = stdout {
                let mut lines = BufReader::new(stdout).lines();
                loop {
                    let line = tokio::select! {
                        line = lines.next_line() => line,
                        _ = cancel_rx.recv() => {
                            let _ = tx.send(StreamMessage::Stderr("Operation cancelled".into()));
                            let _ = child.start_kill();
                            break;
                        }
                    };
                    let Ok(Some(line)) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    match parse_event_line(&line) {
                        Ok(event) => {
                            if !primary_complete_seen && event.tag == "complete" {
                                primary_complete_seen = true;
                                event_failure = event_completion_failed(&event);
                            }
                            let _ = tx.send(StreamMessage::Event(event));
                        }
                        Err(error) => {
                            let _ = tx.send(StreamMessage::Stderr(format!(
                                "Malformed Lore JSON event: {error}: {line}"
                            )));
                        }
                    }
                }
            }
            let status = child.wait().await.ok();
            let stderr = stderr_task.await.unwrap_or_default();
            if !stderr.trim().is_empty() {
                let _ = tx.send(StreamMessage::Stderr(stderr.clone()));
            }
            let code = status.as_ref().and_then(std::process::ExitStatus::code);
            let success = status.is_some_and(|status| status.success()) && !event_failure;
            let _ = tx.send(StreamMessage::Finished(CommandRecord {
                argv: redacted,
                display,
                success,
                status: code,
                duration: started.elapsed(),
                stderr,
            }));
        });
    }
}

fn parse_event_line(line: &str) -> Result<LoreEvent> {
    let raw: RawEvent = serde_json::from_str(line)?;
    Ok(LoreEvent {
        tag: raw.tag_name,
        data: raw.data,
    })
}

fn parse_events(stdout: &[u8], stderr: &str) -> Result<Vec<LoreEvent>> {
    let text = String::from_utf8_lossy(stdout);
    let mut events = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        events.push(parse_event_line(line).with_context(|| format!("invalid Lore JSON: {line}"))?);
    }
    if events.is_empty() && !stderr.trim().is_empty() {
        bail!("Lore failed: {}", stderr.trim())
    }
    Ok(events)
}

fn completion_failed(events: &[LoreEvent]) -> bool {
    events
        .iter()
        .find(|event| event.tag == "complete")
        .is_some_and(event_completion_failed)
}

fn event_completion_failed(event: &LoreEvent) -> bool {
    event.tag == "complete"
        && event
            .data
            .get("status")
            .and_then(Value::as_i64)
            .unwrap_or_default()
            != 0
}

fn display_command(args: &[String]) -> String {
    let mut result = String::from("lore");
    for arg in args {
        result.push(' ');
        if arg.contains(char::is_whitespace) {
            result.push('"');
            result.push_str(&arg.replace('"', "\\\""));
            result.push('"');
        } else {
            result.push_str(arg);
        }
    }
    result
}

pub fn event_error(event: &LoreEvent) -> Option<String> {
    if event.tag != "error" && event.tag != "complete" {
        return None;
    }
    let error = event.data.get("error")?;
    for key in ["message", "reason", "description"] {
        if let Some(text) = error.get(key).and_then(Value::as_str) {
            if !text.is_empty() {
                return Some(text.into());
            }
        }
    }
    if error.is_null() || error == &Value::Object(Default::default()) {
        None
    } else {
        Some(error.to_string())
    }
}

pub fn event_summary(event: &LoreEvent) -> String {
    if let Some(error) = event_error(event) {
        return format!("error: {error}");
    }
    if event.tag == "progress" {
        let message = event
            .data
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("working");
        return message.into();
    }
    format!("{} {}", event.tag, compact_json(&event.data))
}

fn compact_json(value: &Value) -> String {
    let text = value.to_string();
    if text.len() > 240 {
        format!("{}…", &text[..240])
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_unknown_events_without_rejecting_them() {
        let event =
            parse_event_line(r#"{"tagName":"futureEvent","data":{"answer":42,"newField":true}}"#)
                .unwrap();
        assert_eq!(event.tag, "futureEvent");
        assert_eq!(event.data["answer"], 42);
    }

    #[test]
    fn sees_failed_completion() {
        let event = parse_event_line(
            r#"{"tagName":"complete","data":{"status":5,"error":{"message":"nope"}}}"#,
        )
        .unwrap();
        assert!(event_completion_failed(&event));
        assert_eq!(event_error(&event).as_deref(), Some("nope"));
    }

    #[test]
    fn parses_captured_lore_fixtures() {
        for fixture in [
            include_str!("../tests/fixtures/status.ndjson"),
            include_str!("../tests/fixtures/branches.ndjson"),
            include_str!("../tests/fixtures/diff.ndjson"),
        ] {
            let events: Vec<LoreEvent> = fixture
                .lines()
                .filter(|line| !line.is_empty())
                .map(|line| parse_event_line(line).unwrap())
                .collect();
            assert!(events.iter().any(|event| event.tag == "complete"));
        }
    }
}
