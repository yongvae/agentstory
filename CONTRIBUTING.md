# Contributing

Thanks for helping. Before you open a pull request:

1. Read [ROADMAP.md](ROADMAP.md), especially the principles. The two that matter most for patches:
   - **Agent-neutral code.** Built-in rules may only name agents' own public formats and widely used public tools (git, gh, glab, package managers, kubectl…). Anything specific to a chat bridge, a harness or a company/internal platform goes in the user's config file (`~/.config/agentstory/config.toml`), not in code, tests, fixtures or docs.
   - **Light.** Run `scripts/bench.sh` before and after a change that touches indexing or search, and put both numbers in the PR. If README numbers change, update them in the same PR (both languages).
2. Fixtures are synthetic. Never copy real transcripts, ids, names, paths or tokens into `tests/fixtures/`.
3. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` must pass.
