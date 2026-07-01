# LazyLore

[![CI](https://github.com/Peralysis/lazylore/actions/workflows/ci.yml/badge.svg)](https://github.com/Peralysis/lazylore/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Peralysis/lazylore)](https://github.com/Peralysis/lazylore/releases)
[![License](https://img.shields.io/github/license/Peralysis/lazylore)](LICENSE)

LazyLore is a cross-platform terminal UI for [Epic's Lore version control system](https://github.com/EpicGames/lore). It follows the pane layout and keyboard conventions of lazygit where Lore has an equivalent operation, while exposing Lore-native features such as file locks, links, layers, repository verification, and shared stores.

> [!IMPORTANT]
> Lore is pre-1.0. LazyLore currently requires Lore 0.8.4 or newer and capability-checks the installed CLI at startup.

## Features

- Files, staged state, branches, revision history, locks, conflicts, and unified diffs in one responsive interface.
- `Space`, `c`, `A`, `d`, `p`, `P`, `C`/`V`, and other familiar lazygit-style controls.
- Searchable `Ctrl+P` browser covering the complete Lore CLI command surface.
- Structured integration through Lore's newline-delimited `--json` events; core state never depends on decorative CLI output.
- Non-blocking operations, progress events, command history, redacted secrets, and confirmation gates for destructive commands.
- Scale-first change discovery: instant tracked status, debounced filesystem notifications, and an explicit full scan.
- Windows, macOS, and Linux terminal support.

## Requirements

- [Lore CLI 0.8.4+](https://epicgames.github.io/lore/how-to/install-lore-cli/)
- A terminal with color and alternate-screen support

The `lore` executable must be on `PATH`, or supplied with `--lore-binary`.

## Build and run

```console
cargo build --release
target/release/lazylore
```

Run it from a Lore working copy or pass a path:

```console
lazylore
lazylore /path/to/repository
lazylore --scan
lazylore --lore-binary /custom/path/to/lore
```

Outside a usable repository, LazyLore opens an onboarding screen. Press `Ctrl+P` to find `repository create`, `repository clone`, authentication, and shared-store commands.

## Keyboard map

| Scope | Keys |
|---|---|
| Global | `Tab`/`Right` next pane, `Left`/`Shift+Tab` previous pane, `1–5`/`0` direct focus |
| Global | `j/k` or `Up/Down` move within a pane, `q` quit |
| Global | `p` sync, `P` push, `R` tracked-state refresh, `?` help, `@` command log (`;`/`.` page, `Esc` back) |
| Global | `Ctrl+P` Lore command browser, `:` shell command |
| Files | `Space` stage/unstage, `a` stage all, `c` commit, `A` amend, `d` discard, `r` full scan, `e` edit, `o` open, `Enter` view diff |
| Branches | `Space` switch, `n` create, `d` archive, `M` merge, `g` reset, `Enter` history, `[`/`]` Local/Remote tabs |
| Revisions | `Space` sync, `C`/`V` copy/cherry-pick, `t` revert, `g` reset, `y` copy hash, `Enter` file tree |
| Revision tree | `Space`/`Enter` expand/collapse dir, `Enter` on file view diff, `Esc` back to list |
| Locks | `Space` acquire/release, `r` refresh |
| Main | `Ctrl+Tab` cycles working/staged/unstaged diff; PgUp/PgDn scroll, `Esc` back to Files |

Lore does not currently expose Git-style stash, rebase, tags, worktrees, or line-level staging, so those lazygit controls are intentionally absent.

## Change discovery

Lore normally reports only files already marked dirty; it does not walk a potentially enormous working tree for every status request. LazyLore follows that model:

1. Startup loads tracked status immediately.
2. Files changed while LazyLore is open are debounced and passed to `lore dirty`.
3. `[unscanned]` remains visible until `r` performs `lore status --scan`.

Use `--scan` or `general.scan_on_start = true` when correctness at startup is more important than scan cost.

## Configuration

LazyLore reads `config.toml` from the platform-native application configuration directory. Override it with `--config`; run with `--help` for the remaining startup options.

```toml
[general]
lore_binary = "lore"
refresh_interval_ms = 2000
watch_files = true
scan_on_start = false
history_page_size = 100
confirm_destructive = true

[ui]
mouse = true
file_tree = true
theme = "default"

[tools]
editor = "code"
opener = ""
diff_tool = ""

[cache]
enabled = true
ttl_secs = 604800
max_disk_mb = 128
max_memory_entries = 256
```

Revision deltas and revision-to-revision diffs are keyed on immutable revision
hashes, so LazyLore memoizes them in memory and under the platform cache
directory to avoid re-invoking `lore` when you revisit a revision. Working-tree
state, status, history, branches, and locks always come from a live `lore`
call. Entries expire after `ttl_secs` and the on-disk cache is trimmed,
oldest-first, once it exceeds `max_disk_mb`. Set `cache.enabled = false` to
disable caching entirely.

Bindings are action-based and accept single characters or names such as `space`, `enter`, `tab`, `escape`, `pageup`, and combinations such as `ctrl+p` or `alt+x`:

```toml
[keybindings]
quit = "ctrl+q"
file_commit = "c"
```

`VISUAL` and `EDITOR` are used when `tools.editor` is unset.

## Safety and compatibility

- LazyLore passes argument arrays directly to `lore`; it does not invoke a shell for Lore operations.
- `--token` values are masked in command history.
- Repository deletion, obliteration, destructive resets, and equivalent commands require typing `confirm`.
- Unknown JSON events and fields are preserved for display rather than causing a protocol failure.
- The installed command tree is compared with a Lore 0.8.4 baseline. Missing commands are disabled; newly discovered commands appear with conservative safety defaults.
- `:` intentionally invokes the platform shell and should be treated like a normal terminal prompt.

## Development

```console
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

Core behavior is tested with captured NDJSON fixtures and without requiring a live Lore server. A future opt-in end-to-end suite can point at Lore's demo server.

## License

MIT. See [LICENSE](LICENSE).

