# Roadmap

agentstory is a small, fast tool that does one job well: **find and extract what you need from coding-agent sessions**, so people and other agents can pick up past work. Everything else (memory files, wiki, handoff) exists to put that extraction to use.

## Principles

1. **Light.** One binary, no daemon, no model calls, no network. Every feature is measured with `scripts/bench.sh`, and the README numbers are kept current.
2. **Agent-neutral.** Built-in support is limited to agents' own public formats and widely used public tools (git, gh, glab, package managers). Anything specific to a harness, a chat bridge or a company platform belongs in the user's config, never in the code.
3. **Read-only and private.** Sources are never modified; secrets are redacted before anything is stored or printed; the index stays local.
4. **Deterministic.** Extraction is rules, not summaries, so results can be trusted and tested.
5. **Tested against real formats.** Each connector ships with a fixture that mirrors the agent's on-disk layout; "tested on real data" is claimed only after a run on real sessions.

## Now (0.5)

- [x] Search levels: `chat` (default) / `work` / `full`, with automatic escalation and a per-hit `matched` label
- [x] Configurable transcript rules for chat bridges and harnesses (`~/.config/agentstory/config.toml`); no harness or platform names in the code
- [x] Optional lean index (`[index] store_output = false`)
- [x] 9 connectors: Claude Code, Cursor, Codex, Hermes, Pi, DeepSeek Harness, Gemini CLI, Qwen Code, OpenCode
- [x] Homebrew tap with prebuilt binaries for macOS and Linux (x86_64, arm64)

## Next

- [ ] **crates.io release** (`cargo install agentstory`), then trusted publishing from CI
- [ ] **Extraction commands** that answer common questions directly, without reading whole sessions:
  - `ags decisions` — what the person asked for, changed their mind about, or approved
  - `ags files <path>` — every session that touched a file, with what was asked at the time
  - `ags errors` — recurring errors across sessions, with the fix that worked
- [ ] **Lighter index**: store compact session bodies (drop text already in the FTS columns), target < 6% of raw history at `full`
- [ ] **Faster startup**: skip the refresh stat walk when the last refresh was under N seconds ago
- [ ] More connectors: GitHub Copilot CLI, Aider, Goose, Kimi CLI, Crush, Factory Droid, Grok Build, Cline/Roo Code (VS Code storage), OpenCode 1.x JSON storage

## Later

- [ ] `ags watch`: keep the index fresh from file events
- [ ] Optional local embeddings behind a feature flag, for "find sessions like this one"
- [ ] Windows support and a Windows build
- [ ] Hand off between agents of different kinds by writing the target agent's native resume format

## Not planned

- Cloud sync or hosted search
- Built-in LLM summarisation (agents can summarise the extracted text themselves)
- Support for proprietary or internal platforms in the codebase (use the config file instead)
