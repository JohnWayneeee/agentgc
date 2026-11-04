---
name: agentgc
description: Use when running several coding agents in parallel in git worktrees, when ports or database names collide between tasks, when heavy builds or tests should run one at a time, or when cleaning up finished task folders and agent leftovers. Wraps the agentgc CLI.
---

# agentgc

Check the tool exists: run `agentgc --help`. If the command is missing, tell the user to install it from
https://github.com/JohnWayneeee/agentgc and stop.

Follow `integrations/AGENTS.md` from that repository. The short version:

- Read-only commands are always safe: `scan`, `worktrees`, `ps`, `task list`, `task review NAME` (summary of a task
  for the human reviewer), `status`, `logs`, `run --status`, `doctor`.
- `doctor` finds hard-coded ports and database names in tracked files. Run it, then fix each hit so the app reads
  `PORT_*` and `DB_NAME` from `.agentgc/task.env`.
- New task: `agentgc task new NAME` (dry run), then `--yes`. Read ports and the database name from
  `<worktree>/.agentgc/task.env`.
- Heavy commands: `agentgc run -- <command>`. For a rate-limited shared service, use
  `agentgc run --resource NAME --slots 1 --min-interval SECONDS -- <command>`.
  If a `PreToolUse` hook rewrote a command to `agentgc run -- ...`, that is expected. Do not undo it.
- Finish: run `agentgc task review NAME` and show it to the user before `task done NAME`. Then
  `agentgc task done NAME` (dry run), show it to the user, `--yes` only after approval.
- Never run `clean`, `ps --stop --yes`, `down --force` or any `--force` without an explicit user request.
  `clean` is interactive: the user answers its questions, not the agent.
- Never run `agentgc hook install --apply` (it edits the Claude Code settings file) or `--allow` on the hook
  (it skips the approval prompt) without an explicit user request.
