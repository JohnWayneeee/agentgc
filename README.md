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
