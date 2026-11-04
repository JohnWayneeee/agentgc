# agentgc

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

English | [Русский](README.ru.md)

Run many AI coding agents in parallel without the mess: isolated task environments, a process supervisor, and a safe cleanup for Claude Code, Codex and other CLI agents.

## For you if

- You run several coding agents at once, each in its own git worktree.
- Ports and database names collide between your tasks.
- Your disk fills up with agent and dev tool leftovers.
- Your machine freezes when five agents build at once.

## The problem

- Two tasks want the same port or database name.
- Worktree folders outlive their git entry. `git status` fails in them.
- Parallel tests and builds start at the same time and load the CPU.
- Idle dev servers still hold RAM. `ps` shows which ones are idle.

## What it does

| Command | What it does |
|---|---|
| `agentgc scan` | Lists junk: package caches, downloaded browser builds, agent sessions, temp files. Read-only. |
| `agentgc worktrees` | Finds orphan worktrees and duplicate dependency folders. Read-only. |
| `agentgc clean` | Deletes listed junk one item at a time, with a confirmation for each item. |
| `agentgc ps` | Shows which processes belong to which task, with their RAM. Read-only. |
| `agentgc ps --stop` | Stops idle dev servers, language servers, tests and builds. Dry run by default. |
| `agentgc task new` / `list` / `done` | Creates, lists and removes a task environment: worktree, ports, env file, database name. |
| `[compose]` `[routes]` `[services]` | Config sections: a compose file per task, route URLs, and plain processes per task. |
| `agentgc up` / `down` / `status` / `logs` / `restart` | Starts, stops, restarts and checks a task's services, and shows their logs. |
| `agentgc run` | Queues heavy commands so only some run at once. Guards named exclusive resources. |

Full reference: [docs/reference.md](docs/reference.md). It lists every flag and rule.

## Quick start

### Build from source

agentgc is a Rust project.

```
cargo install --path .
```

or `cargo build --release` (binary: `target/release/agentgc`).

### Example

Put this in `agentgc.task.toml` in the repo root. The services section comes from the [full reference](docs/reference.md).

```toml
[ports]
names = ["web", "api"]
[db]
create = "createdb {db}"
[services.web]
cmd = "npm run dev -- --port {{port.web}}"
port = "web"
[services.api]
cmd = "cargo run -- --port {{port.api}} --db {{db}}"
cwd = "apps/api"
port = "api"
```

Run these in the repo root:

```
cd my-repo
agentgc task new feat-login --yes    # worktree, ports, env file, services
agentgc up --task feat-login         # start the services
agentgc status --task feat-login     # pid, state and URL per service
agentgc down --task feat-login       # stop the services
agentgc task done feat-login --yes   # remove the task (refuses unsaved work)
```

`task new` and `task done` print a plan first. `--yes` runs it.

## Safety by design

- Dry run is the default for `task new`, `task done` and `ps --stop`.
- `clean` asks before each item. Only `y` deletes a `safe` item. An `ask` item needs its id typed.
- `ask` items go to the Recycle Bin or Trash. They are never deleted for good.
- Browser profiles and user data are not in the built-in rules. A deny-list refuses paths with names such as `User Data`, `Profile` or `Documents`.
- A path that holds a `.git` entry is refused. It is a project, not a cache.
- Without a terminal, `clean` deletes only the items named with `--yes-item`.
- `ps --stop` and `task done` never stop agents, databases or other processes. `clean` never kills a process.
- agentgc opens no network port, installs no system service and does not start at boot or login.

### Known limits

- Windows is the tested platform. Linux and macOS code paths exist, but they are not tested yet.
- Windows has no graceful stop. `ps --stop` needs `--force` there.
- The supervisor is opt-in and per task. Services start only when you run `up`.

## How it compares

- **A shell script:** deletes whatever its paths say. `clean` checks each path against guards and asks per item.
- **Worktree helpers alone:** create checkouts. agentgc also picks free ports, writes a task env file, and refuses `task done` while work is unsaved.
- **A queue tool alone:** limits concurrent jobs. `agentgc run` adds named exclusive resources, such as one phone or one browser profile, and Windows support.

Idea credit: [heavy](https://github.com/mokevnin/heavy) by mokevnin, a counting semaphore for macOS and Linux. `agentgc run` is separate code.

## FAQ

### Does it delete my files without asking?

No. `clean` asks before each item, and `ask` items go to the Recycle Bin. `task done` runs only with `--yes`, and it refuses when work is unsaved.

### Does it run a daemon?

No. Nothing runs in the background unless you start it. `run` has no daemon. `up` starts one supervisor for one task, and `down` stops it.

### Which agents does it work with?

The built-in rules cover Claude Code, Codex CLI and Gemini CLI. The process rules also match OpenCode. For other agents, write your own rules: `--rules` for `scan` and `clean`, `--proc-rules` for `ps`.

### Does it need Docker?

No. agentgc never runs docker or starts containers. The compose file is optional, and `[services]` runs plain processes.

### Is it safe on Windows?

It is tested on Windows, and the limits above apply. Run `clean --dry-run` first, and use `--force` for stops.

## Support

If agentgc saves you time, a star on GitHub helps others find it.

Updates and notes: Telegram channel TELEGRAM_INVITE_LINK_PLACEHOLDER

## License

Released under the [MIT License](LICENSE).
