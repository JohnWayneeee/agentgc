# agentgc

Garbage collector for the disk junk that CLI coding agents and dev tools leave behind:
package caches, browser downloads, agent sessions, temp files.

**Status:** `scan`, `worktrees`, `ps` and `task list` are read-only. Only `task done --yes` deletes, and only a finished task folder (see below).

## Build

```
cargo install --path .
```

or `cargo build --release` (binary: `target/release/agentgc`).

## Usage

```
agentgc scan                    # table sorted by size
agentgc scan --json             # machine-readable output
agentgc scan --rules my.toml    # use your own rules instead of the built-in ones
```

Missing paths and unreadable files are skipped. Env vars are expanded:
`%USERPROFILE%`, `%LOCALAPPDATA%`, `%APPDATA%`, `%TEMP%` (Windows), `$HOME`, `$XDG_CACHE_HOME` (Unix), and `~`.
A path with an unset variable is skipped.

## Orphan worktrees

```
agentgc worktrees [--root PATH]... [--depth N] [--json]
```

Finds directories whose `.git` is a file with `gitdir: <path>` and `<path>` no longer exists.
Git does not track them, so `git status` fails there. Default root: current directory. Default depth: 6.
Skips `node_modules`, `.git`, `target`, `.next`, `.venv`. Does not follow symlinks.
Per orphan: total size, `node_modules` size, file count (without node_modules), age of the newest file, missing gitdir.
`worktrees` never deletes anything.

### Prunable worktrees

Always printed in a separate section (and under `prunable` in `--json`). A directory with a `.git` folder is a main repo.
`agentgc` reads each `<repo>/.git/worktrees/*/gitdir`. If the parent folder of the path inside is gone, git still
knows the worktree but its folder does not exist. Per entry: name, repo, missing path, hint `git worktree prune`.
The hint is text only. `agentgc` never runs it.

### Duplicate dependencies (`--deps`)

```
agentgc worktrees --root PATH --deps [--dep-depth N] [--json]
```

Groups worktrees by repo (main checkout included, orphans marked `[orphan]`). For each repo with 2 or more members it
prints, per member, the total and the dependency folders found (`node_modules`, `.venv`, `vendor/bundle`, `target`,
`.next`) with relative path and size, largest first (6 lines, then "+N more"). Folders are searched at the member root
and in subfolders up to `--dep-depth` (default 3), so monorepos such as `web/node_modules` are found. The search
does not enter `.git`, found dependency folders or other member roots (nested worktrees count under their own
member), and does not follow symlinks. When 2 or more members hold dependencies, a note says: N members hold
dependencies, X in total; a shared store (for example pnpm) can cut this. With `--json` the same data (`path` and
`bytes` per folder) is under `deps`. Read-only.

## Running processes (`ps`)

```
agentgc ps [--json] [--min-mb N] [--all] [--root PATH] [--proc-rules FILE]
```

Shows which running processes belong to which task folder (nearest parent with a `.git` file or folder, so a
worktree and its main checkout are separate groups) and how much RAM each group uses. Groups are sorted by RAM.
Per process: pid, kind, RAM, `idle` mark, command cut to 80 chars. Kinds (`agent`, `dev-server`, `lsp`, `test`,
`build`, `db`, `other`) come from rules in `rules/processes.toml`; `--proc-rules` replaces them. A rule matches the executable name or
whole command-line words (a path counts by its file name only, so folder names never match), plus an optional
`path_contains` substring test for install paths such as `@anthropic-ai/claude-code`.
A dev server, language server, test or build with under 1% CPU over a 1 second sample is marked `idle`.
Default `--min-mb` is 50. `--all` adds processes whose cwd cannot be read ("unknown cwd"). System folders and
`agentgc` itself are skipped. Text after `--token`, `--key`, `--password`, `--secret` and values of `NAME=value`
words are hidden. `ps` never stops, signals or changes anything; the hint line is text only.

## Task environments (`task`)

```
agentgc task new NAME [--repo PATH] [--config FILE] [--run-setup] [--yes]
agentgc task list [--repo PATH] [--json]
agentgc task done NAME [--repo PATH] [--config FILE] [--yes] [--kill-procs] [--force]
```

Parallel agents each need an isolated environment: own worktree, ports, database name, queue/cache prefix.
`task new` builds it from a declarative file `agentgc.task.toml` in the repo root (`--config FILE` overrides).
Every field is optional.

```toml
[worktree]
dir = "worktrees"          # repo-local folder for task worktrees
branch = "{name}"          # branch template
base = "main"              # branch to start from
[copy]
files = [".env", ".env.local"]   # copied from the main checkout when present
[ports]
names = ["web", "api"]
range = [3100, 3999]
[db]
name = "{repo}_{name}"     # a NAME only; agentgc never talks to a database server
create = ""                # optional command template, for example "createdb {db}"
drop = ""                  # command template run by `done --yes` ({db} {name}); never run by `new`
[env]
TASK_ID = "{name}"
PREFIX = "{name}_"         # queue names, cache keys, test storage paths
[env.dynamic]
# values may use {name} {repo} {db} {port.web} {port.api}
DATABASE_URL = "postgres://localhost/{db}"
[setup]
commands = []              # for example ["pnpm install"]
```

Variables: `{name}` (lowercase letters, digits, `-`, `_`; anything else is an error), `{repo}` (repo folder name,
sanitized), `{db}`, `{port.<name>}`.

**Dry run is the default.** `task new NAME` prints the plan (worktree path and branch, files to copy, ports, db name,
env file, commands) and changes nothing. `--yes` executes it:

1. `git worktree add -b <branch> <path> <base>` through the `git` CLI (no shell).
2. Copies the listed files that exist. Never overwrites.
3. Picks for each port the first free one in the range: a TCP bind on 127.0.0.1 works and no other registered task holds it.
4. Writes `<worktree>/.agentgc/task.env` (`TASK_ID`, `PREFIX`, `DB_NAME`, `PORT_<NAME>`, `[env]`, `[env.dynamic]`).

The registry entry `<repo>/.agentgc/tasks/<name>.json` is written FIRST with `"state": "creating"`, before step 1.
At the end it is rewritten (temp file in the same folder, then rename) with `"state": "ready"`. A failed `new` leaves
the entry as `creating`, so `done` can clean up. Entries without `state` count as ready. `new` refuses when the branch
already exists. `base` and `branch` from the config must not start with `-` or hold whitespace or control characters.
In the db name, `{repo}` uses `_` instead of `-`; elsewhere `{repo}` is unchanged.

`create` and `setup.commands` run only with `--run-setup`: one by one, split by whitespace (no shell, no quotes),
cwd = the worktree, stop at the first failure. Otherwise they are printed as "not run". The plan hides `user:pass@`
in URLs and values of keys that contain SECRET, TOKEN, PASSWORD or KEY. A hint appears when the worktree folder or
`.agentgc/` is not git-ignored; `agentgc` never edits `.gitignore`.

`task list` is read-only. Per task: name, path, branch, ports, db, age in days, status `ok`, `missing` (folder gone),
`dirty` (`git status --porcelain` prints something), `unknown` (git failed) or `incomplete` (state `creating`).
`task new` and `task list` never delete anything. Printing never panics on a closed stdout (`| head`).

### `task done`

```
agentgc task done NAME [--repo PATH] [--config FILE] [--yes] [--kill-procs] [--force]
```

Removes a finished task. This is the only command that deletes. **Dry run is the default:** it prints the plan and
the checks and changes nothing. `--yes` executes. When in doubt, it refuses and says why.

Checks (read-only, run first):

1. NAME is sanitized as in `new`. The task must be in the registry (an `incomplete` entry is allowed).
2. **Path guard.** The registry path, resolved, must be a direct child of `<repo>/<worktree.dir>`, named like the task,
   not a symlink, with a `.git` file (a linked worktree). Otherwise a hard error. `--force` never skips it.
3. Uncommitted work: `git status --porcelain`, ignoring `.agentgc/`. Any entry is a BLOCKER (10 files, then "+N more").
4. Unsaved commits: `git rev-list <branch> --not <base> --remotes`. More than 0 is a BLOCKER ("N commits are neither
   merged into <base> nor pushed").
5. Processes whose cwd is inside the worktree (same kinds as `ps`). All are listed. Only `dev-server`, `lsp`, `test`,
   `build` can be stopped. `agent`, `db` and `other` are never stopped, only listed as "left running"; the folder may
   stay locked on Windows. Processes whose cwd cannot be read are not seen.

Blockers abort before any change. With `--force` a loud line "FORCE: this will discard: ..." lists what is lost.

Steps with `--yes` (the first failure stops the run and the already done steps are printed):

- a. With `--kill-procs`: stop the stoppable processes (never `agentgc` itself), wait up to 3 s, report each pid.
  Without it: "not stopped (use --kill-procs)".
- b. A non-empty `drop` template from the registry entry runs with `{db}` and `{name}` filled (whitespace split, no
  shell, cwd = repo root). A failure aborts, unless `--force`.
- c. Deletes `<worktree>/.agentgc/task.env`, then `.agentgc` only if it is empty.
- d. `git worktree remove <path>`; `--force` is passed to git only when you passed `--force`. If git refuses, the
  message is printed and `done` stops. The folder is never deleted by any other means.
- e. `git branch -d <branch>` (`-D` with `--force`). If `-d` refuses (unmerged), a note is printed and the run continues.
- f. Deletes the registry file.
- g. Prints "freed about X" (size measured before step a).

Never touched: anything outside the task folder, the main checkout, other tasks, remote branches. No recursive delete
(`remove_dir_all`) exists in the command. If stdout closes during `--yes`, output goes silent but the steps still finish.

## SECURITY

`agentgc.task.toml` can name commands: `db.create` and `setup.commands` run with `new --run-setup`, and `db.drop`
(stored in the registry) runs with `done --yes`. They run as you, without a shell. Review the file like a Makefile.
Never run `new --run-setup` or `done --yes` from a repo you do not trust.

## Rules format

Built-in rules live in `rules/default.toml`.

```toml
[[rule]]
id = "npm-cache"
name = "npm cache"
paths = ["%LOCALAPPDATA%\\npm-cache", "~/.npm"]
safety = "safe"              # "safe" = regrows by itself, "ask" = needs your decision
note = "Package tarballs. Regrows on next install."
clean_cmd = "npm cache clean --force"   # optional hint, never run
```

## Related

- [heavy](https://github.com/mokevnin/heavy): machine-wide semaphore for heavy commands.
