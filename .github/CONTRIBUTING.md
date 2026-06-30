# Contributing to LazyLore

Thank you for taking the time to contribute! This guide covers everything you need to know to get from zero to a merged pull request.

Please read our [Code of Conduct](.github/CODE_OF_CONDUCT.md) before contributing. If you believe you have found a security vulnerability, see [SECURITY.md](.github/SECURITY.md) instead of opening a public issue.

## Prerequisites

| Requirement | Minimum version | Notes |
|---|---|---|
| Rust toolchain | 1.87 | `rustup update stable` |
| Lore CLI | 0.8.4 | Required to run; see [install guide](https://epicgames.github.io/lore/how-to/install-lore-cli/) |

The `lore` executable must be on `PATH` (or supplied with `--lore-binary` at runtime).

## Build and run

```console
cargo build --release
target/release/lazylore
```

Run from a Lore working copy or pass a path:

```console
lazylore /path/to/repository
lazylore --scan
lazylore --lore-binary /custom/path/to/lore
```

## Required checks

All three checks must pass before you open a PR — they are exactly what CI runs:

```console
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

Run `cargo fmt --all` to auto-format before checking. Clippy warnings are treated as errors, so address them rather than suppressing them.

## Testing

Core behavior is covered by captured NDJSON fixture tests and does not require a live Lore server. Write or update fixture tests for new behaviour wherever possible, and regression tests for bug fixes.

A future opt-in end-to-end suite may point at Lore's demo server; those tests live in `tests/` and are not run by default.

## Commit and PR conventions

- Use [Conventional Commits](https://www.conventionalcommits.org/) prefixes: `feat:`, `fix:`, `chore:`, `docs:`, `refactor:`, `test:`.
- Keep each PR focused on one logical change; split unrelated fixes into separate PRs.
- Link the relevant issue in the PR description (`Closes #123`).
- Update the keyboard map table in `README.md` if you add or change a keybinding.

## Submitting a pull request

1. Fork the repository and create a branch from `main`.
2. Make your changes and confirm all required checks pass.
3. Open a PR — the template will guide you through the rest.

We aim to review PRs within a week. If you haven't heard back in two weeks, feel free to leave a comment to bump the PR.
