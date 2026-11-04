# agentgc (paste into AGENTS.md, CLAUDE.md or your agent's rules file)

`agentgc` is installed on this machine. It gives each parallel task its own worktree, ports and database name,
queues heavy commands, runs task services, and cleans up. Run `agentgc --help` or `agentgc <command> --help`
when unsure. Full reference: https://github.com/JohnWayneeee/agentgc/blob/main/docs/reference.md

## Safe to run any time (read-only)

- `agentgc scan` junk on disk, `agentgc worktrees` orphan worktrees, `agentgc ps` processes grouped by task
- `agentgc task list`, `agentgc status`, `agentgc logs NAME`, `agentgc run --status`

## Starting a task

1. `agentgc task new NAME` is a dry run. Read the plan, then run it again with `--yes`.
2. Work inside the new worktree. Never hard-code ports or database names.
   Read them from `<worktree>/.agentgc/task.env` (`PORT_<NAME>`, `DB_NAME`, `TASK_ID`, `AGENTGC_ROUTE_<NAME>`).
3. Run tests and builds as `agentgc run -- <command>` so many agents do not start them at once.
4. `agentgc up` starts the task's services, `agentgc status` and `agentgc logs NAME` inspect them,
   `agentgc down` stops them.

## Finishing a task

- `agentgc task done NAME` is a dry run. Show the plan and the checks to the user.
- Run `agentgc task done NAME --yes` only after the user agrees. It refuses while the folder has uncommitted
  work or unmerged commits. Report a refusal to the user. Do not bypass it with `--force`.

## Never do these without the user's explicit request

- `agentgc clean`: it is interactive and deletes files. Do not script its answers. Tell the user to run it.
- `agentgc ps --stop --yes` and `agentgc down --force`: they stop processes.
- `--force` on any command.
- Do not delete worktree folders by hand. Use `agentgc task done`.
