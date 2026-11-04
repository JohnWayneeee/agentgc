# Contributing to agentgc

Thanks for helping. The tool deletes files and stops processes, so changes are judged by safety first.

## Build and test

```
cargo build
cargo test
```

`cargo build` must finish without warnings. All tests must pass.

## Rules for changes

- **Tests use temp folders only.** A test never touches the real home folder, real browser or app data, or real processes.
  Use a fake `Remover`, a fake `Killer` and an injected home folder, as the existing tests do.
- **No recursive delete in program code.** `remove_dir_all` is allowed in tests only. Delete files you created, one by one.
- **Dry run is the default** for every command that changes something. Writing needs `--yes` or a confirmation.
- **User data is never cache.** A rule that can hold accounts, sessions, profiles or projects must be `safety = "ask"`.
  Never add a rule for a browser profile.
- **Never kill an agent, a database or an unknown process.** Check pid, start time and name again right before a signal.
- **Keep output free of secrets.** Command lines are redacted. Keep it so.
- No new dependency without a reason in the pull request. Pin exact versions.

## Pull requests

1. Open an issue first for a large change.
2. One topic per pull request. Add or update tests and the README or `docs/reference.md`.
3. Commit messages in English, imperative mood, subject line under 72 characters.
4. Fill in the pull request template.

## Platforms

Windows is the tested platform. Linux and macOS code paths exist but have not been built or run. Help is welcome:
build on those systems, run `cargo test`, and open an issue with the result.

## Privacy

Do not put personal paths, usernames, emails or real project names in code, tests, docs, issues or logs you paste.
Use neutral examples such as `C:/work/project` or `~/project`.
