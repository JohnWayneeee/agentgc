# AGENTS.md

agentgc is a Rust CLI that runs many AI coding agents in parallel. It gives each task its own git worktree,
free ports, a database name and an env file, and it supervises services, queues heavy commands and cleans up.

This file is for coding agents and contributors who work on agentgc itself. Rules for agents that use agentgc
in other projects are in `integrations/AGENTS.md`.

## Build and test

```sh
cargo build            # must finish without warnings
cargo test             # all tests must pass
cargo build --release  # binary: target/release/agentgc
```

CI runs `cargo build --locked --all-targets` and `cargo test --locked` on Windows, Linux and macOS.
Add `--locked` locally too, so `Cargo.lock` does not change.

Tests use temp folders only. They never touch the real home folder, a browser, app data or real processes.
Use a fake `Remover`, a fake `Killer` and an injected home folder, as the existing tests do.
Tests and the program use no network. The crate has no HTTP dependency.

## Code conventions

- Dry run is the default for every command that changes something. It acts only with `--yes`, `--apply`,
  or a confirmation per item.
- Read-only commands never write: `scan`, `worktrees`, `doctor`, `ps`, `task list`, `task review`, `status`,
  `logs`, `run --status`. `run` writes only its own files next to the slots: lock, info, gate and last-start files.
- Errors are plain messages: `error: <message>` on stderr, exit code 1 (see `src/main.rs`).
- `ps` hides the values of secret flags (`--token`, `--key`, `--password`, `--secret`) and of `NAME=value` words in
  command lines. Keep it so, and never print a secret in any other output.
- No recursive delete in program code. `remove_dir_all` is for tests only. Delete files one by one.
- A new dependency needs a reason in the pull request. Crates that touch the system (`sysinfo`, `trash`, `fs4`,
  `windows-sys`, `libc`) are pinned to an exact version with `=`. Keep that.

## Safety rules

- Never delete or overwrite a user file without a plan and a confirmation.
- `clean` deletes only the items it lists, one at a time. `ask` items go to the Recycle Bin or Trash.
  They are never deleted for good.
- User data is never cache. A rule that can hold accounts, sessions, profiles or projects must use
  `safety = "ask"`. Never add a rule for a browser profile.
- A path that holds a `.git` entry is refused. It is a project, not a cache.
- Never kill an agent, a database or an unknown process. Check pid, start time and name again right before
  a signal.
- The Claude Code `PreToolUse` hook asks by default (`permissionDecision: ask`). Never make `allow` the default.
  `--allow` is an explicit opt-in.
- `init` never overwrites an existing file. It edits only its marked block in `AGENTS.md` and `CLAUDE.md`,
  keeps a `.agentgc.bak` copy, and never replaces an existing `SKILL.md`.

## Embedded agent rules

- `integrations/AGENTS.md` and `integrations/claude-code/SKILL.md` are compiled into the binary with
  `include_str!` (see `src/init.rs`). `agentgc init` writes them into user projects.
- Keep both files in sync with each other and with the commands in `src/main.rs`. Check each command
  named in them against `agentgc --help`.
- A change reaches `agentgc init` users only with the next release.

## Docs rules

- `README.md` and `README.ru.md` say the same things. Update both in the same change.
- `CHANGELOG.md` follows Keep a Changelog. Add entries under `## [Unreleased]`, in `### Added`, `### Changed`
  or `### Fixed`.
- A new feature goes under `### Added`: in `[Unreleased]` until the release, then in the version it ships in.
  Never list under `### Fixed` a change to code that no release has shipped.
- `docs/reference.md` is the full reference: every command, flag and rule.
- No em dashes in docs. Use a period, a comma or a colon.
- Use neutral examples such as `C:/work/project` or `~/project`. No personal paths, usernames, emails or
  real project names.

## Commits and pull requests

- Commit messages in English, imperative mood, one subject line under 72 characters.
  Example: `Add min-interval to run`.
- No AI or Claude attribution lines and no `Co-authored-by` lines in commits, pull requests or comments.
- One topic per pull request. Add or update tests and the README or `docs/reference.md`.
