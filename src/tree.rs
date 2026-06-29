//! Collapsible directory tree for revision file lists.
use std::collections::{BTreeMap, HashSet};

/// A single row in the flattened, DFS-ordered tree.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeEntry {
    /// Nesting depth (0 = top level).
    pub depth: usize,
    /// Full path used for collapse-set membership tests.
    /// For directories this may be a compressed path like `"Build/Windows"`.
    /// For files this is always the complete file path.
    pub path: String,
    /// Display label. For single-child directory chains this is the compressed
    /// multi-segment name, e.g. `"Build/Windows"`.
    pub label: String,
    pub is_dir: bool,
    /// `'A'` / `'M'` / `'D'` for file entries; `None` for directory rows.
    pub marker: Option<char>,
}

/// Return every entry that should be visible given the current collapsed set.
/// An entry is hidden iff some collapsed directory path `P` is a strict
/// ancestor of the entry: `entry.path.starts_with("{P}/")`.
pub fn visible_entries<'a>(
    entries: &'a [TreeEntry],
    collapsed: &HashSet<String>,
) -> Vec<&'a TreeEntry> {
    entries
        .iter()
        .filter(|e| {
            !collapsed
                .iter()
                .any(|dir| e.path.starts_with(&format!("{}/", dir)))
        })
        .collect()
}

// ── internal construction types ──────────────────────────────────────────────

enum Node {
    File { marker: char },
    Dir { children: BTreeMap<String, Node> },
}

/// Build a flat, DFS-ordered `Vec<TreeEntry>` from `(path, marker)` pairs.
///
/// Paths use `/` as the separator. Directories with exactly one child that is
/// also a directory are compressed into a single entry whose label shows the
/// merged segments, e.g. `"Build/Windows"`. Within each directory, child
/// directories are listed before files; both groups are alphabetical.
pub fn build_tree(files: &[(String, char)]) -> Vec<TreeEntry> {
    let mut root: BTreeMap<String, Node> = BTreeMap::new();
    for (path, marker) in files {
        insert_path(&mut root, &path.split('/').collect::<Vec<_>>(), *marker);
    }
    let mut entries = Vec::new();
    flatten(&root, "", 0, &mut entries);
    entries
}

fn insert_path(map: &mut BTreeMap<String, Node>, parts: &[&str], marker: char) {
    if parts.is_empty() {
        return;
    }
    if parts.len() == 1 {
        map.insert(parts[0].to_owned(), Node::File { marker });
        return;
    }
    let child = map.entry(parts[0].to_owned()).or_insert_with(|| Node::Dir {
        children: BTreeMap::new(),
    });
    if let Node::Dir { children } = child {
        insert_path(children, &parts[1..], marker);
    }
}

/// DFS-flatten `map`, emitting directories before files (both alphabetical).
fn flatten(map: &BTreeMap<String, Node>, prefix: &str, depth: usize, out: &mut Vec<TreeEntry>) {
    for (name, node) in map {
        if let Node::Dir { children } = node {
            let full_path = join_path(prefix, name);
            let (label, final_path, leaf_children) = compress(&full_path, name, children);
            out.push(TreeEntry {
                depth,
                path: final_path.clone(),
                label,
                is_dir: true,
                marker: None,
            });
            flatten(leaf_children, &final_path, depth + 1, out);
        }
    }
    for (name, node) in map {
        if let Node::File { marker } = node {
            out.push(TreeEntry {
                depth,
                path: join_path(prefix, name),
                label: name.clone(),
                is_dir: false,
                marker: Some(*marker),
            });
        }
    }
}

/// Compress a single-child directory chain by merging segment names until the
/// chain branches or terminates at a file.
fn compress<'a>(
    base_path: &str,
    base_label: &str,
    children: &'a BTreeMap<String, Node>,
) -> (String, String, &'a BTreeMap<String, Node>) {
    if children.len() == 1 {
        let (child_name, child_node) = children.iter().next().unwrap();
        if let Node::Dir {
            children: grandchildren,
        } = child_node
        {
            return compress(
                &format!("{}/{}", base_path, child_name),
                &format!("{}/{}", base_label, child_name),
                grandchildren,
            );
        }
    }
    (base_label.to_owned(), base_path.to_owned(), children)
}

fn join_path(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{}/{}", prefix, name)
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn f(pairs: &[(&str, char)]) -> Vec<(String, char)> {
        pairs.iter().map(|(p, m)| ((*p).to_string(), *m)).collect()
    }

    #[test]
    fn flat_files_at_root() {
        let tree = build_tree(&f(&[(".gitignore", 'A'), ("README.md", 'M')]));
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].path, ".gitignore");
        assert_eq!(tree[0].marker, Some('A'));
        assert!(!tree[0].is_dir);
        assert_eq!(tree[1].path, "README.md");
        assert_eq!(tree[1].marker, Some('M'));
    }

    #[test]
    fn dirs_before_files_alphabetical() {
        let tree = build_tree(&f(&[("z_file.rs", 'A'), ("a_dir/child.rs", 'A')]));
        let dir_pos = tree.iter().position(|e| e.is_dir).unwrap();
        let file_pos = tree.iter().position(|e| e.label == "z_file.rs").unwrap();
        assert!(dir_pos < file_pos, "directory must precede root-level file");
    }

    #[test]
    fn single_child_chain_compressed() {
        // Build/Windows/AcheronStation all have one child each → should compress.
        let tree = build_tree(&f(&[("Build/Windows/AcheronStation/file.txt", 'A')]));
        let dir = tree.iter().find(|e| e.is_dir).unwrap();
        assert!(
            dir.label.contains('/'),
            "expected compressed label, got: {}",
            dir.label
        );
        // The file should appear at depth 1 (one level under the compressed dir).
        let file = tree.iter().find(|e| !e.is_dir).unwrap();
        assert_eq!(file.depth, 1);
        // The file's full path must be reachable through the compressed dir path.
        assert!(file.path.starts_with(&format!("{}/", dir.path)));
    }

    #[test]
    fn branching_dir_not_compressed() {
        // src has two children → must not be compressed.
        let tree = build_tree(&f(&[("src/main.rs", 'A'), ("src/lib.rs", 'M')]));
        let dir = tree.iter().find(|e| e.is_dir).unwrap();
        assert_eq!(dir.label, "src");
        assert_eq!(dir.depth, 0);
        assert_eq!(tree.iter().filter(|e| !e.is_dir).count(), 2);
    }

    #[test]
    fn collapse_hides_descendants_only() {
        let tree = build_tree(&f(&[
            ("src/main.rs", 'A'),
            ("src/lib.rs", 'M'),
            ("Cargo.toml", 'A'),
        ]));
        let mut collapsed = HashSet::new();
        collapsed.insert("src".to_owned());

        let visible = visible_entries(&tree, &collapsed);
        // "src" dir itself must still be visible.
        assert!(visible.iter().any(|e| e.path == "src" && e.is_dir));
        // Children must be hidden.
        assert!(visible.iter().all(|e| !e.path.starts_with("src/")));
        // Sibling at root must still be visible.
        assert!(visible.iter().any(|e| e.path == "Cargo.toml"));
    }

    #[test]
    fn mixed_actions_preserved() {
        let tree = build_tree(&f(&[("add.rs", 'A'), ("mod.rs", 'M'), ("del.rs", 'D')]));
        let markers: Vec<_> = tree.iter().map(|e| e.marker.unwrap()).collect();
        // BTreeMap sorts alphabetically: add.rs, del.rs, mod.rs
        assert_eq!(markers, vec!['A', 'D', 'M']);
    }
}
