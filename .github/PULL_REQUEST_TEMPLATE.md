## What changes and why

## Checklist

- [ ] `cargo build` has no warnings and `cargo test` passes
- [ ] Tests use temp folders and fakes only, never real user paths or processes
- [ ] No `remove_dir_all` outside tests; dry run stays the default for new write commands
- [ ] README or `docs/reference.md` updated if behavior changed
- [ ] No personal paths, usernames, emails or real project names in code, tests or docs
- [ ] Tested on: Windows / Linux / macOS (say which)
