# agentgc

Garbage collector for the disk junk that CLI coding agents and dev tools leave behind:
package caches, browser downloads, agent sessions, temp files.

**Status:** `scan`, `worktrees`, `ps`, `task list` and `run --status` are read-only (`ps --stop --yes` stops idle processes, see below). Only `task done --yes` and `clean` delete: `task done` a finished task folder (see below), `clean` the junk that `scan` lists, one item at a time (see `clean`). `run` writes only its own slot and info files.

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

## Delete junk one item at a time (`clean`)

```
agentgc clean [--rules FILE] [--only ID]... [--yes-item ID]... [--allow-ask ID]... [--dry-run] [--log FILE]
```

**Never run it against paths you did not read.** Run `scan` and read every path first. Never put a browser profile or other app data into a bulk cleanup. `clean` asks about every item and sends "ask" items to the Recycle Bin.

`clean` loads the same rules as `scan`, measures them, skips missing and 0 B items and sorts by size. An item is one
rule with all its existing paths. For each item it shows: name, id, safety, every path with its size, the note, the 5
largest direct children, the age of the newest file in days, and a warning for each running process whose cwd is inside
the path (pid and name; nothing is stopped). Then it asks.

### Confirmation model

- `safe` item: `Delete this? [y/N/q]`. Only `y` deletes. Anything else (also an empty line) skips. `q` stops the run.
  End of input stops the run.
- `ask` item (may hold accounts, sessions, projects): you must TYPE THE ITEM ID exactly. Anything else skips
  (`q` stops the run). It is moved to the Recycle Bin / Trash, never deleted for good. Space is freed only after you
  empty the bin.
- stdin is not a terminal (non-interactive): nothing is asked and nothing is deleted, except items named with
  `--yes-item ID`. An `ask` item also needs `--allow-ask ID` for the same id, otherwise it is refused.
  In a terminal these two flags are ignored: every item is asked.
- `--only ID` limits the run to those ids (repeatable). An unknown id is an error.
- `--dry-run` shows the same screens and the plan. It never asks to delete and never deletes.
- `--log FILE` appends one JSON line per item: item id, action (`deleted`, `trashed`, `skipped`, `refused`),
  bytes, skipped files, unix time. Dry runs write nothing.

### Deletion

`safe` items are deleted for good, path by path. Symlinks and junctions are never followed: the link itself is removed.
A file that is locked or cannot be removed is skipped and counted. There are no retries and no force tricks.
`clean` never kills a process and never runs `clean_cmd` (it stays a hint). The end of the run shows, per item, the
bytes freed (size before minus size after), the locked files skipped, a total, and the free space of the system
drive before and after when it can be read.

### Hard guards

Checked when the rules are loaded and again right before every deletion. The error names the rule and the path.
No flag skips them.

- The path must be absolute, with no `.` or `..` parts, with at least 4 components (drive or root counts, so a drive
  root or `C:\Users` is impossible), and must not be the home dir or a parent of it. If the home dir cannot be found,
  everything is refused.
- Deny-list: a path with a component (any case) equal to `.ssh`, `.gnupg`, `.git`, `Documents`, `Desktop`, `Pictures`,
  `Videos`, `Downloads`, `OneDrive`, `User Data`, `Profile`, `Profiles`, `Login Data`, `Cookies`, `Local Storage`,
  `IndexedDB`, `Wallets`, `wallet`, `keystore`, `Application Support`, or starting with `Profile `, is refused.
- A path that holds a `.git` entry inside is refused: it is a project, not a cache. The same deny-list names are
  refused anywhere inside the path. A folder that cannot be read is refused too (it may hide such an entry).
- `ask` items only go to the trash, never to permanent delete, whatever the flags.

If one rule breaks a guard, the run stops before anything is touched; use `--only` to leave that rule out.
The real removal sits behind a small `Remover` trait, so tests use a fake and never touch the real bin.

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
words are hidden. Without `--stop`, `ps` never stops, signals or changes anything; the hint line is text only.

### Stop idle processes (`ps --stop`)

```
agentgc ps --stop [--yes] [--pid N]... [--task NAME] [--force] [--proc-rules FILE]
```

`--stop` conflicts with `--json` and ignores `--min-mb`, `--all` and `--root`. `--yes`, `--pid`, `--task`, `--force`
work only with `--stop`.

- **Candidates:** processes marked `idle` (under 1% CPU) of kind `dev-server`, `lsp`, `test` or `build`, with a
  readable cwd outside system folders. Never `agent`, `db` or `other`. Never `agentgc` or any process in its parent chain.
  `--task NAME` keeps only processes whose task root folder (nearest `.git`, else the cwd) has this name, any case.
- **Dry run is the default.** It lists pid, kind, name, cwd, CPU % and memory, then prints
  `dry-run: nothing stopped`. Command lines are not printed, so secrets stay hidden.
- **`--yes` in a terminal:** asks `Stop pid N name? [y/N/q]` for each candidate. Only `y` stops it; `q` or end of
  input quits. `--pid` limits which processes are asked.
- **`--yes` without a terminal:** only pids named with `--pid` are stopped. An unknown or non-candidate pid is an
  error and nothing is stopped. Without `--pid` it is an error too.
- **Pid reuse guard:** right before the signal the process is read again. If pid, start time, name or cwd differ from
  what was shown, it is refused and not signaled.
- **Graceful first:** a terminate signal, then up to 3 s of waiting. On Windows a graceful terminate does not exist:
  `ps --stop --yes` prints `use --force` and stops nothing. `--force` is the hard kill (on Linux and macOS it skips
  the graceful step too). Processes still running after the wait are reported.
- **Summary:** stopped, still running, refused, memory freed (the RSS shown before the stop).

All signals go through the `Killer` trait in `src/proc_kill.rs`; `task done --kill-procs` uses the same code.
Tests use a fake and never signal a real process.

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

### Compose file per task (`[compose]`, `[routes]`)

Parallel tasks must not clash on ports, container names, volumes or hostnames. `task new` can render one
docker-compose file per task from a template. Both sections are optional.

```toml
[ports]
names = ["web", "api"]
[compose]
template = "docker-compose.task.yml.tpl"   # path relative to the repo root; must exist
output = "docker-compose.task.yml"         # path relative to the worktree; no '..'
[routes]                                   # route name -> port name from [ports]
web = "web"                                # http://web.<task>.localhost:<port>
```

Template (`docker-compose.task.yml.tpl`):

```yaml
services:
  web:
    container_name: web-{{task}}
    ports: ["{{port.web}}:3000"]
    environment:
      DATABASE_URL: postgres://localhost/{{db}}
```

Placeholders (plain text replace): `{{task}}`, `{{port.NAME}}`, `{{db}}`, `{{env.NAME}}` (any key of the task env, for
example `{{env.PREFIX}}`), `{{worktree}}`. An unknown placeholder is an error that names it; nothing is written then.

- `task new` renders the file after ports and db are allocated and writes it with `create_new`. An existing file is
  never overwritten: `new` stops with an error and the file stays.
- It prints the routes (`web.<task>.localhost:<port>`) and writes them to `<worktree>/.agentgc/task.env` as
  `AGENTGC_ROUTE_<NAME>=http://...`. It also writes `COMPOSE_PROJECT_NAME=agentgc-<task>`, so `docker compose` keeps
  containers and volumes of each task apart.
- agentgc never starts or stops containers and never runs docker.
- The registry entry records `compose_file` and `routes` (old entries without them still load). `task list` shows the routes.
- `task done --yes` removes only that one recorded file: it must be a plain file, untracked, and inside the
  worktree (`remove_file`, never a recursive delete). It prints the hint `docker compose -p agentgc-<task> down -v`
  and never runs it. The rendered file does not count as uncommitted work.
- Checks at load time: `output` and `template` are relative paths without `..`; the template file exists; every route
  points to a name in `ports.names`.

### Plain processes per task (`[services]`)

Docker-free setup: each task runs plain processes plus one shared database server. `[services.NAME]` is optional.
`[db]` already gives every task its own database name inside one server, so `[db]` + `[services]` replace a
per-task compose stack. agentgc only describes and prints the commands. It never starts, supervises or stops them.
No daemon.

```toml
[ports]
names = ["web", "api"]
[db]
create = "createdb {db}"                   # one database per task in the shared server (run with --run-setup)
[services.web]
cmd = "npm run dev -- --port {{port.web}}"  # placeholders as in compose: {{task}} {{port.NAME}} {{db}} {{env.NAME}} {{worktree}}
cwd = "."                                   # relative to the worktree, no '..'; default "."
port = "web"                                # optional: a name from [ports]; adds the route web.<task>.localhost:<port>
[services.api]
cmd = "cargo run -- --port {{port.api}} --db {{db}}"
cwd = "apps/api"
port = "api"
```

- Rendering happens at plan time with the compose renderer. An unknown placeholder is an error that names it, and
  nothing is written. The rendered command must be one non-empty line.
- `task new` prints, per service, the ready command and its cwd (also in the dry run). With `--yes` it writes, with
  `create_new` and never over an existing file: `<worktree>/.agentgc/services.json` (array of `name`, `cmd`, `cwd`,
  `port` as the allocated number or `null`), `services.sh` (`cd <cwd> && <cmd>`) and `services.ps1`
  (`Set-Location <cwd>; <cmd>`), one plain line per service, no comments. Run the lines yourself.
- A service with `port` adds the route `<name>.<task>.localhost:<port>` and `AGENTGC_ROUTE_<NAME>` to
  `.agentgc/task.env`, like `[routes]`. If `[routes]` already has that name, the `[routes]` entry wins.
- The registry records the generated files (`generated_files`) and the service names (`services`); old entries
  without them still load. `task list` shows the service names.
- `task done --yes` removes only the recorded files, with `remove_file`: each must be a plain file (no symlink),
  untracked, and inside the worktree. Same checks as for the compose file. `.agentgc/` stays out of the
  uncommitted-work check.
- Checks at load time: the service name follows the task name rules, `cmd` is not empty, `cwd` is relative without
  `..`, `port` is a name in `ports.names`.

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

## Queue for heavy commands (`run`)

```
agentgc run [--resource NAME]... [--slots N] [--pool NAME] [--dir DIR] [--timeout SECONDS]
            [--on-timeout fail|run] [--log FILE] [--no-skip-ci] -- COMMAND [ARGS...]
agentgc run --status [--dir DIR] [--pool NAME] [--json]
```

```
agentgc run -- pnpm test                                    # at most a quarter of the cores run at once
agentgc run --resource phone --slots 1 -- npm run e2e       # one run at a time on a shared device
agentgc run --status                                        # who holds which slot
```

Many agents on one machine start heavy commands (tests, builds) at the same time. `run` makes them take turns.
It also guards an exclusive resource (a phone, a simulator, one browser profile).

- **No daemon.** A slot is a file `<dir>/<pool>/<resource>.<i>.lock` with an advisory exclusive lock
  (`LockFileEx` on Windows, `flock` on Linux and macOS). The OS releases the lock when the holder dies, even after a crash or kill.
- **Not fair.** Waiters poll every 200 ms. Whoever finds a free slot first gets it. There is no order and no priority.
- **Defaults.** Dir `<temp dir>/agentgc-run`, pool `default`, resource `default`, slots = a quarter of the logical cores
  (at least 1). Env fallbacks: `AGENTGC_DIR`, `AGENTGC_POOL`, `AGENTGC_SLOTS`, `AGENTGC_TIMEOUT`, `AGENTGC_ON_TIMEOUT`,
  `AGENTGC_LOG`. Names: lowercase letters, digits, `-`, `_`.
- **Several resources.** Taken in sorted name order (no deadlock), each with `--slots` slots. All are held while the
  command runs. A named exclusive resource uses `--slots 1`.
- **Waiting.** When no slot is free, `run` prints once to stderr who holds each slot (pid, how long, command, from the
  holder's `.info` file) and keeps waiting. A free slot prints nothing. When it gets the slot: `got <resource> slot i after X s`.
- **Holder info.** `<resource>.<i>.info` (pid, command, cwd, start) is for display only. The lock is the truth. A stale
  info file of a dead holder blocks nothing. A holder removes only its own `.info`. `run` never deletes a lock or info
  file of another process.
- **Timeout.** `--timeout` stops the waiting. `--on-timeout fail` (default): "gave up waiting", exit 75.
  `--on-timeout run`: runs the command anyway, without slots.
- **The command.** argv after `--`, no shell, stdio and cwd inherited. Exit code of `agentgc` = exit code of the command;
  127 when the program cannot start; 75 on timeout. On Windows, bare names also find `.cmd` and `.bat` (`npm`, `pnpm`).
- **Nesting.** The child gets `AGENTGC_HELD=<pool>:<resource>[,<resource>...]` (several pools joined with `;`).
  A nested `agentgc run` whose resources are all in that list runs at once. Resources not in the list are still queued.
- **CI.** With env `CI` set, `run` runs the command at once, without a queue. `--no-skip-ci` turns this off.
- **Log.** `--log FILE` appends one JSON line per queued run: `start`, `pool`, `resources`, `command`, `cwd`, `slots`,
  `waited_ms`, `ran_ms`, `timed_out`, `exit`. Runs that skipped the queue are not logged.
- **Status.** `--status` lists every pool and resource under the dir with each slot held or free (a non-blocking lock
  attempt, released at once) and the holder's info. `--json` for machines. Read-only: it never stops or kills anything.

`run` queues CPU-heavy work and exclusive devices. Ports and database names are separate: `task` handles them.

Idea credit: [heavy](https://github.com/mokevnin/heavy), a counting semaphore for macOS and Linux. `agentgc run` is
separate code; it adds Windows and named resources.

## SECURITY

`clean` runs no command from the rules, but it deletes. Read every rule path first and use `--dry-run`.

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
