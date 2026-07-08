use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, value};

pub fn config_path(repo: &Path) -> PathBuf {
    repo.join(".lore").join("config.toml")
}

/// Rewrites the repository's `.lore/config.toml` `identity` key to `id` if it
/// doesn't already match, preserving every other key, section, and comment in
/// the file. Returns the previous value when a rewrite happened; `Ok(None)`
/// when the file was already correct or doesn't exist (nothing to reconcile).
pub fn set_identity(repo: &Path, id: &str) -> Result<Option<String>> {
    let path = config_path(repo);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let mut doc: DocumentMut = text
        .parse()
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let current = doc
        .get("identity")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    if current.as_deref() == Some(id) {
        return Ok(None);
    }
    doc["identity"] = value(id);
    fs::write(&path, doc.to_string())
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(dir: &Path, body: &str) {
        fs::create_dir_all(dir.join(".lore")).unwrap();
        fs::write(config_path(dir), body).unwrap();
    }

    #[test]
    fn rewrites_mismatched_identity_and_preserves_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            r#"remote_url = "lore://127.0.0.1:41337"
identity = "user@example.com"

[store]
max_capacity = 2000000

[file]
direct_write = false
"#,
        );

        let previous = set_identity(dir.path(), "3a1c0fad-530c-44ef-b25a-bfa6aad45bc6")
            .unwrap()
            .expect("identity should have been rewritten");
        assert_eq!(previous, "user@example.com");

        let text = fs::read_to_string(config_path(dir.path())).unwrap();
        assert!(text.contains(r#"identity = "3a1c0fad-530c-44ef-b25a-bfa6aad45bc6""#));
        assert!(text.contains(r#"remote_url = "lore://127.0.0.1:41337""#));
        assert!(text.contains("[store]"));
        assert!(text.contains("max_capacity = 2000000"));
        assert!(text.contains("[file]"));
        assert!(text.contains("direct_write = false"));
    }

    #[test]
    fn leaves_already_correct_identity_untouched() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            r#"identity = "3a1c0fad-530c-44ef-b25a-bfa6aad45bc6"
"#,
        );

        let result = set_identity(dir.path(), "3a1c0fad-530c-44ef-b25a-bfa6aad45bc6").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn missing_config_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let result = set_identity(dir.path(), "3a1c0fad-530c-44ef-b25a-bfa6aad45bc6").unwrap();
        assert!(result.is_none());
    }
}
