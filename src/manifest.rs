use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Safety {
    ReadOnly,
    Mutating,
    Destructive,
    Secret,
    LongRunning,
}

#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub path: String,
    pub description: String,
    pub usage: String,
    pub safety: Safety,
    pub available: bool,
    /// Whether this command requires an active server connection. When `true`
    /// and the app is offline, LazyLore will re-probe before running it and
    /// refuse if the server remains unreachable.
    pub requires_network: bool,
}

impl CommandSpec {
    pub fn new(path: &str, description: &str, safety: Safety) -> Self {
        Self {
            path: path.into(),
            description: description.into(),
            usage: format!("lore {path}"),
            safety,
            available: true,
            requires_network: false,
        }
    }

    /// Mark this command as requiring a live server connection.
    pub fn network(mut self) -> Self {
        self.requires_network = true;
        self
    }
}

pub fn baseline_commands() -> Vec<CommandSpec> {
    use Safety::*;
    let groups: &[(&str, Safety)] = &[
        ("repository status", ReadOnly),
        ("repository info", ReadOnly),
        ("repository list", ReadOnly),
        ("repository create", Mutating),
        ("repository clone", Mutating),
        ("repository delete", Destructive),
        ("repository verify", ReadOnly),
        ("repository verify state", ReadOnly),
        ("repository verify fragment", ReadOnly),
        ("repository dump", ReadOnly),
        ("repository gc", Mutating),
        ("repository store immutable query", ReadOnly),
        ("repository metadata get", ReadOnly),
        ("repository metadata set", Mutating),
        ("repository metadata clear", Destructive),
        ("repository instance list", ReadOnly),
        ("repository instance prune", Destructive),
        ("repository config get", ReadOnly),
        ("repository update-path", Mutating),
        ("branch list", ReadOnly),
        ("branch info", ReadOnly),
        ("branch create", Mutating),
        ("branch switch", Mutating),
        ("branch push", Mutating),
        ("branch merge", Mutating),
        ("branch merge unresolve", Mutating),
        ("branch merge into", Mutating),
        ("branch merge start", Mutating),
        ("branch merge restart", Destructive),
        ("branch merge resolve", Mutating),
        ("branch merge resolve mine", Mutating),
        ("branch merge resolve theirs", Mutating),
        ("branch merge abort", Destructive),
        ("branch diff", ReadOnly),
        ("branch archive", Destructive),
        ("branch reset", Destructive),
        ("branch protect", Mutating),
        ("branch unprotect", Mutating),
        ("branch latest list", ReadOnly),
        ("branch metadata get", ReadOnly),
        ("branch metadata set", Mutating),
        ("branch metadata clear", Destructive),
        ("revision history", ReadOnly),
        ("revision info", ReadOnly),
        ("revision commit", Mutating),
        ("revision amend", Mutating),
        ("revision sync", Mutating),
        ("revision bisect", Mutating),
        ("revision diff", ReadOnly),
        ("revision find metadata", ReadOnly),
        ("revision find number", ReadOnly),
        ("revision restore", Mutating),
        ("revision cherry-pick", Mutating),
        ("revision cherry-pick unresolve", Mutating),
        ("revision cherry-pick restart", Destructive),
        ("revision cherry-pick resolve", Mutating),
        ("revision cherry-pick resolve mine", Mutating),
        ("revision cherry-pick resolve theirs", Mutating),
        ("revision cherry-pick abort", Destructive),
        ("revision revert", Mutating),
        ("revision revert unresolve", Mutating),
        ("revision revert restart", Destructive),
        ("revision revert resolve", Mutating),
        ("revision revert resolve mine", Mutating),
        ("revision revert resolve theirs", Mutating),
        ("revision revert abort", Destructive),
        ("revision metadata get", ReadOnly),
        ("revision metadata set", Mutating),
        ("revision metadata clear", Destructive),
        ("file info", ReadOnly),
        ("file metadata get", ReadOnly),
        ("file metadata set", Mutating),
        ("file metadata clear", Destructive),
        ("file dependency add", Mutating),
        ("file dependency remove", Mutating),
        ("file dependency list", ReadOnly),
        ("file stage", Mutating),
        ("file stage move", Mutating),
        ("file stage merge", Mutating),
        ("file dirty", Mutating),
        ("file dirty move", Mutating),
        ("file dirty copy", Mutating),
        ("file unstage", Mutating),
        ("file reset", Destructive),
        ("file obliterate", Destructive),
        ("file history", ReadOnly),
        ("file diff", ReadOnly),
        ("file write", Mutating),
        ("file hash", ReadOnly),
        ("auth login", Secret),
        ("auth info", ReadOnly),
        ("auth list", ReadOnly),
        ("auth logout", Destructive),
        ("auth clear", Destructive),
        ("layer add", Mutating),
        ("layer remove", Destructive),
        ("layer list", ReadOnly),
        ("logfile info", ReadOnly),
        ("login", Secret),
        ("link add", Mutating),
        ("link remove", Destructive),
        ("link update", Mutating),
        ("link list", ReadOnly),
        ("status", ReadOnly),
        ("clone", Mutating),
        ("stage", Mutating),
        ("stage move", Mutating),
        ("stage merge", Mutating),
        ("dirty", Mutating),
        ("dirty move", Mutating),
        ("dirty copy", Mutating),
        ("unstage", Mutating),
        ("reset", Destructive),
        ("diff", ReadOnly),
        ("history", ReadOnly),
        ("commit", Mutating),
        ("sync", Mutating),
        ("push", Mutating),
        ("lock acquire", Mutating),
        ("lock status", ReadOnly),
        ("lock query", ReadOnly),
        ("lock release", Mutating),
        ("service run", LongRunning),
        ("service start", Mutating),
        ("service stop", Mutating),
        ("notification subscribe", LongRunning),
        ("completions", Mutating),
        ("shared-store create", Mutating),
        ("shared-store info", ReadOnly),
        ("shared-store set-use-automatically", Mutating),
    ];
    /// Commands that always require a live server connection.
    const NETWORK_COMMANDS: &[&str] = &[
        "clone",
        "sync",
        "push",
        "branch push",
        "branch merge",
        "branch merge into",
        "branch merge start",
        "branch merge restart",
        "branch merge abort",
        "branch merge resolve",
        "branch merge resolve mine",
        "branch merge resolve theirs",
        "branch merge unresolve",
        "branch protect",
        "branch unprotect",
        "revision sync",
        "revision cherry-pick",
        "revision cherry-pick abort",
        "revision cherry-pick restart",
        "revision cherry-pick resolve",
        "revision cherry-pick resolve mine",
        "revision cherry-pick resolve theirs",
        "revision cherry-pick unresolve",
        "lock acquire",
        "lock query",
        "lock release",
        "repository create",
        "repository clone",
        "auth login",
        "auth logout",
        "auth clear",
        "login",
        "shared-store create",
        "notification subscribe",
    ];

    groups
        .iter()
        .map(|(path, safety)| {
            let mut spec = CommandSpec::new(path, "Lore command", *safety);
            if NETWORK_COMMANDS.contains(path) {
                spec.requires_network = true;
            }
            spec
        })
        .collect()
}

pub fn merge_runtime_manifest(baseline: Vec<CommandSpec>, markdown: &str) -> Vec<CommandSpec> {
    let mut commands: BTreeMap<String, CommandSpec> = baseline
        .into_iter()
        .map(|mut command| {
            command.available = false;
            (command.path.clone(), command)
        })
        .collect();
    let mut current: Option<String> = None;
    for line in markdown.lines() {
        if let Some(rest) = line.strip_prefix("## `lore ")
            && let Some(path) = rest.strip_suffix('`')
        {
            let path = path.trim().to_owned();
            current = Some(path.clone());
            commands
                .entry(path.clone())
                .and_modify(|spec| spec.available = true)
                .or_insert_with(|| CommandSpec {
                    path,
                    description: "Command discovered from this Lore installation".into(),
                    usage: String::new(),
                    safety: Safety::Destructive,
                    available: true,
                    requires_network: false,
                });
        } else if line.starts_with("**Usage:**") {
            if let Some(path) = current.as_ref() {
                if let Some(spec) = commands.get_mut(path) {
                    spec.usage = line
                        .replace("**Usage:**", "")
                        .replace('`', "")
                        .trim()
                        .into();
                }
            }
        } else if !line.trim().is_empty() && current.is_some() && !line.starts_with('#') {
            let path = current.as_ref().expect("checked");
            if let Some(spec) = commands.get_mut(path) {
                if spec.description == "Lore command"
                    || spec.description.starts_with("Command discovered")
                {
                    spec.description = line.trim().into();
                }
            }
            current = None;
        }
    }
    commands.into_values().collect()
}

pub fn redact(args: &[String]) -> Vec<String> {
    let mut result = Vec::with_capacity(args.len());
    let mut hide_next = false;
    for arg in args {
        if hide_next {
            result.push("<redacted>".into());
            hide_next = false;
            continue;
        }
        result.push(arg.clone());
        if arg == "--token" {
            hide_next = true;
        }
    }
    result
}

pub fn split_arguments(input: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            } else {
                current.push(ch);
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                args.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if quote.is_some() {
        return Err("unterminated quote".into());
    }
    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        args.push(current);
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_quoted_arguments_without_a_shell() {
        assert_eq!(
            split_arguments("commit \"hello world\" --stats").unwrap(),
            ["commit", "hello world", "--stats"]
        );
    }

    #[test]
    fn redacts_token_values() {
        assert_eq!(
            redact(&[
                "auth".into(),
                "login".into(),
                "--token".into(),
                "secret".into()
            ]),
            ["auth", "login", "--token", "<redacted>"]
        );
    }

    #[test]
    fn baseline_covers_all_major_groups() {
        let commands = baseline_commands();
        for group in [
            "repository",
            "branch",
            "revision",
            "file",
            "auth",
            "layer",
            "link",
            "lock",
            "service",
            "notification",
            "shared-store",
        ] {
            assert!(
                commands
                    .iter()
                    .any(|command| command.path.starts_with(group))
            );
        }
    }
}
