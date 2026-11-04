First public release. agentgc gives each AI coding agent task its own environment: a git worktree, free ports, a database name and an env file. It can run the services of a task, queue heavy commands, and clean up afterwards, always with a plan and a question first.

agentgc does not start AI agents. It prepares and cleans the environment around them.

## What is in 0.1.0

| Area | Commands |
|---|---|
| Isolate | `task new`, `task list`, `task done` (dry run by default, refuses unsaved work, resumable) |
| Supervise | `up`, `down`, `status`, `logs`, `restart` (opt-in, per task, no system service, no network port) |
| Queue | `run` (heavy commands take turns, named exclusive resources) |
| Clean | `scan`, `worktrees`, `clean` (one question per item, Recycle Bin for risky items), `ps`, `ps --stop` |
| Teach agents | `init` (writes marked rules into `AGENTS.md`, `CLAUDE.md` and a Claude Code skill, after you confirm each file) |

Full command list: [README](https://github.com/JohnWayneeee/agentgc#commands) and [docs/reference.md](https://github.com/JohnWayneeee/agentgc/blob/main/docs/reference.md).

## Download

| System | File |
|---|---|
| Windows (x64) | `agentgc-v0.1.0-x86_64-pc-windows-msvc.zip` |
| Linux (x64) | `agentgc-v0.1.0-x86_64-unknown-linux-gnu.tar.gz` |
| macOS (Apple Silicon) | `agentgc-v0.1.0-aarch64-apple-darwin.tar.gz` |

Each archive holds the binary, the README and the license. Unpack it and put `agentgc` on your `PATH`. Other systems, such as Intel Macs: build from source with `cargo install --path .`.

Verify a download: compare the output of `sha256sum <file>` (macOS: `shasum -a 256 <file>`, Windows: `certutil -hashfile <file> SHA256`) with the `.sha256` file next to it.

The binaries are not code signed. Windows SmartScreen or macOS Gatekeeper can warn on first start. If you do not trust a binary, build it from source.

## Platform status

- Tests pass on Windows, Linux and macOS in GitHub Actions.
- The full flow was run end to end by hand on Windows only. Linux and macOS are untested by hand. Please report problems in an [issue](https://github.com/JohnWayneeee/agentgc/issues).
- On Windows, `ps --stop` needs `--force`, because agentgc has no graceful stop there.

## Before the first clean

Run `agentgc clean --dry-run`. Nothing is deleted without a question for each item.

Found a safety bug, such as a wrong deletion? Use the Security tab, not a public issue. See [SECURITY.md](https://github.com/JohnWayneeee/agentgc/blob/main/SECURITY.md).
