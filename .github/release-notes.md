agentgc gives each AI coding agent task its own environment: a git worktree, free ports, a database name and an env file. It can run the services of a task, queue heavy commands, and clean up afterwards, always with a plan and a question first.

agentgc does not start AI agents. It prepares and cleans the environment around them.

## What is new in 0.3.1

- Agent rules that `agentgc init` writes (the `AGENTS.md` block, the `CLAUDE.md` block and the Claude Code skill) now cover `task review`, `doctor`, the Claude Code hooks (ask by default, `--allow` opt-in) and `run --min-interval`. 0.3.0 shipped the older rules text.

## What is new in 0.3.0

- `agentgc task review NAME` shows what a task changed before you finish it. It only reads.
- Claude Code hooks: `agentgc hook install` adds a `PreToolUse` hook that queues heavy commands through `agentgc run --`, and a `SessionStart` hook that prints the task status and env. The hook asks for approval by default; `--allow` is the opt-in to skip the question. Running `hook install` again replaces the old agentgc entry.
- `task done` resumes after a refused branch delete: the worktree is gone, the registry entry stays, and the next run retries the branch delete.
- `agentgc doctor` lists hard-coded ports and database names in the files git tracks. It only reads.
- `agentgc run --min-interval` waits at least that many seconds between the starts of two queued commands.
- `db.disconnect` in `agentgc.task.toml` runs your command before the database drop in `task done`.
- `agentgc worktrees --deps` estimates how much a shared dependency store would save.

## What is new in 0.2.0

- `agentgc task list` shows the state of every task: changed files, commits ahead and behind the base branch, and how many services run. `--json` has the same fields. The command only reads.
- Services can wait for each other. In `[services.NAME]` set `depends_on = ["db"]` and `ready_timeout = 30`. A service with a `port` is ready when the port accepts a connection. `up` starts the services in order, and `status` has a READY column. A cycle or an unknown name is refused at load time.

## What is in the tool

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
| Windows (x64) | `agentgc-VERSION-x86_64-pc-windows-msvc.zip` |
| Linux (x64) | `agentgc-VERSION-x86_64-unknown-linux-gnu.tar.gz` |
| macOS (Apple Silicon) | `agentgc-VERSION-aarch64-apple-darwin.tar.gz` |

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
