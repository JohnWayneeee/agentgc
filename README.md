# agentgc

Garbage collector for the disk junk that CLI coding agents and dev tools leave behind:
package caches, browser downloads, agent sessions, temp files.

**Status: read-only.** `scan` only measures. Nothing is deleted yet.

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
`build`, `db`, `other`) come from substring rules in `rules/processes.toml`; `--proc-rules` replaces them.
A dev server, language server, test or build with under 1% CPU over a 1 second sample is marked `idle`.
Default `--min-mb` is 50. `--all` adds processes whose cwd cannot be read ("unknown cwd"). System folders and
`agentgc` itself are skipped. Text after `--token`, `--key`, `--password`, `--secret` and values of `NAME=value`
words are hidden. `ps` never stops, signals or changes anything; the hint line is text only.

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
