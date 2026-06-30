# Security Policy

## Supported versions

LazyLore is pre-1.0 software. The latest commit on `main` and the most recent tagged release are the only supported versions. Older releases do not receive security fixes.

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues, pull requests, or discussions.**

Send a description of the vulnerability to: stefanperales@outlook.com

You can also use [GitHub's private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability) if it is enabled for this repository (Settings → Security → Private vulnerability reporting).

### What to include

To help us triage quickly, please include:

- A description of the vulnerability and its potential impact.
- Steps to reproduce or a proof-of-concept.
- The LazyLore version or commit hash you tested against.
- The Lore CLI version (`lore --version`).
- Your OS and terminal environment.

### What to expect

- **Acknowledgement** within 5 business days.
- **Status update** (confirmed, needs more information, not a vulnerability) within 14 days.
- A fix or mitigation timeline will be agreed upon together.

We will credit reporters in the release notes unless you prefer to remain anonymous.

## Scope

LazyLore is a terminal UI that shells out to the `lore` CLI. Vulnerabilities in Lore itself should be reported to the [Epic Games Lore project](https://github.com/EpicGames/lore). Issues arising from the platform shell invoked by LazyLore's `:` command are generally out of scope, as that feature is explicitly a normal terminal prompt.
