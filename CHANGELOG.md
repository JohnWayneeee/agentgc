# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Published on crates.io as `agentgc-worktree`. `cargo install agentgc-worktree` installs the `agentgc` command.

## [0.3.1] - 2026-11-05

### Changed

- Agent rules that `agentgc init` writes (the `AGENTS.md` block, the `CLAUDE.md` block and the Claude Code skill) now cover `task review`, `doctor`, the Claude Code hooks (ask by default, `--allow` opt-in) and `run --min-interval`. 0.3.0 shipped the older rules text.

## [0.3.0] - 2026-11-04

### Added

- `agentgc task review NAME` shows what a task changed before you finish it. It only reads.
- `agentgc hook install` adds Claude Code hooks. A `PreToolUse` hook queues heavy commands through `agentgc run --`. A `SessionStart` hook prints the task status and env. Running it again replaces the old agentgc hook entry.
- The `PreToolUse` hook asks for approval by default. `--allow` opts in to skip the question.
- `agentgc doctor` lists hard-coded ports and database names in the files git tracks. It only reads. It works inside a linked git worktree.
- `agentgc run --min-interval SECONDS` waits at least that many seconds between the starts of two queued commands.
- `db.disconnect` in `agentgc.task.toml` runs your command before the database drop in `task done`.
- `agentgc worktrees --deps` estimates how much a shared dependency store would save.

### Changed

- `task done` stops when `git branch -d` refuses an unmerged branch. The worktree is gone and the registry entry stays; merge the branch and run it again, or use `--force`. Before 0.3.0 it printed a note and went on.

## [0.2.0] - 2026-11-04

### Added

- Services can wait for each other. In `[services.NAME]`, set `depends_on = ["db"]` and `ready_timeout = 30`. A service with a `port` is ready when the port accepts a connection.
- `up` starts services in dependency order. `status` has a READY column. A cycle or an unknown service name is refused at load time.

### Changed

- `agentgc task list` shows the state of every task: changed files, commits ahead and behind the base branch, and how many services run. `--json` has the same fields. The command only reads.

### Fixed

- `status` works on macOS. The service identity is recorded after exec.

## [0.1.0] - 2026-11-04

### Added

- `scan` command with TOML rules to list junk. MIT license.
- `worktrees` command to find orphaned git worktrees. Reports prunable worktrees and duplicate dependency folders per repo, including dependency folders in monorepo subfolders.
- `ps` command groups processes by task folder. `ps --stop` stops idle dev servers and language servers.
- `task new` and `task list` with a declarative task config (`agentgc.task.toml`).
- `task done` with safety checks. It is resumable after a partial failure.
- `run` command: a machine-wide queue for heavy commands and named exclusive resources.
- `clean` command with a confirmation for each item, hard path guards, and trash for `ask` items.
- Compose templating and routes per task.
- `[services]` section in the task config, with `up`, `down`, `status`, `logs` and `restart`.
- `init` command. It writes agent rules into `AGENTS.md`, `CLAUDE.md` and a Claude Code skill, after confirmation for each file.
- CI and a tag-based release workflow that builds archives with `.sha256` files.
- Security policy, contributing guide, and PR and issue templates.
- English README and Russian README.

[Unreleased]: https://github.com/JohnWayneeee/agentgc/compare/v0.3.1...HEAD
[0.3.1]: https://github.com/JohnWayneeee/agentgc/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/JohnWayneeee/agentgc/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/JohnWayneeee/agentgc/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/JohnWayneeee/agentgc/releases/tag/v0.1.0
