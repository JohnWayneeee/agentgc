# Security policy

agentgc deletes files and stops processes, so a safety bug can cost a user real data. We treat these as security issues.

## What to report

- A way to make `clean`, `task done` or `init` delete, overwrite or touch a path outside what they show and confirm.
- A bypass of a hard guard: the protected folder names, the minimum path depth, the home-folder check, the link and junction handling.
- `ps --stop`, `down` or `task done --kill-procs` signalling a process that was not shown or not recorded.
- A secret (token, key, password) printed by `ps`, `status`, `logs` or any other command.
- A path or config value that escapes the worktree (for example with `..` or a link).

Ordinary bugs go to the issue tracker.

## How to report

Use GitHub's private reporting: open the **Security** tab of the repository and choose **Report a vulnerability**.
Do not open a public issue for a security problem. Do not include personal paths, usernames or real secrets in the report.
Describe the command, the config or rules file, the operating system and what happened.

## What to expect

There is one maintainer. We aim to answer within 7 days and to fix confirmed safety bugs before any other work.
There are no tagged releases yet. Fixes land on `main`.

## Safe use

- Run `scan` and read every path before `clean`. Use `--dry-run` first.
- Do not point `clean` at a rules file you did not read.
- Linux and macOS code paths exist but are not tested yet. Test on a throwaway folder first.
