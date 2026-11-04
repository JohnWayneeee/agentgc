---
name: agentgc
description: Use when running several coding agents in parallel in git worktrees, when ports or database names collide between tasks, when heavy builds or tests should run one at a time, or when cleaning up finished task folders and agent leftovers. Wraps the agentgc CLI.
---

# agentgc

Check the tool exists: run `agentgc --help`. If the command is missing, tell the user to install it from
https://github.com/JohnWayneeee/agentgc and stop.

Follow `integrations/AGENTS.md` from that repository. The short version:

- Read-only commands are always safe: `scan`, `worktrees`, `ps`, `task list`, `status`, `logs`, `run --status`.
- New task: `agentgc task new NAME` (dry run), then `--yes`. Read ports and the database name from
  `<worktree>/.agentgc/task.env`.
- Heavy commands: `agentgc run -- <command>`.
- Finish: `agentgc task done NAME` (dry run), show it to the user, `--yes` only after approval.
- Never run `clean`, `ps --stop --yes`, `down --force` or any `--force` without an explicit user request.
  `clean` is interactive: the user answers its questions, not the agent.
