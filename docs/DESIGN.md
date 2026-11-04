# Design

## Goal

Give people and agents one way to find, read and continue past conversations, whatever agent wrote them. Built for a machine where several agents (Claude Code, Cursor, Codex, Hermes, chat bots running them) share work and need to hand it to each other.

Non-goals for now: semantic search, a TUI, syncing between machines, writing back into agents' stores.

## Pipeline

```
connectors ──► Session (agent-neutral) ──► sanitize (redact, truncate) ──► SQLite
  claude-code                                                               ├─ sessions: metadata + JSON body
  cursor, codex                                                             └─ turns: FTS5 trigram, one row per turn
  gemini, qwen
  opencode, hermes
                                         search / sessions / show / handoff / wiki / mcp read from SQLite
```

### Session model (`src/model.rs`)

A `Session` is a list of `Message`s with role `user | assistant | tool | system`.

- `system` is input that did not come from a person: scheduled prompts, hook feedback, task notifications, compaction summaries (`is_summary`). Keeping these apart is what makes "what did the user actually ask" answerable.
- `author` keeps the speaker name when the transcript carries one (chat bridges put `<sender name="…">` in the prompt; another bot relaying a request is recorded under that bot's name).
- Tool calls are normalized into `ToolCall { kind: shell|edit|read|search|agent|other, command, files }` from each agent's own naming (`Bash`/`Shell`/`exec_command`/`run_shell_command`/`terminal`, `Edit`/`StrReplace`/`apply_patch`/`write_file`, …).
- A `Turn` is one input and everything the agent did until the next input. Turns are the unit of search and display.

### Cleaning (`src/clean.rs`, `src/config.rs`)

Agents wrap context around the user's words. Built in: `<system-reminder>`, `<environment_context>`, `<user_instructions>`, IDE selections, Cursor's `<user_query>`/`<timestamp>`, and harness messages (task notifications, hook feedback, compaction summaries) are recognised.

Chat bridges and other harnesses add their own markup. Rather than hard-coding any of them, the user describes theirs in `~/.config/agentstory/config.toml` (`$AGENTSTORY_CONFIG`): tags to drop, the tag holding the person's words, the tag or pattern naming the speaker, notes to strip, relay commands whose text is the agent's reply to a chat, "nothing to say" markers, and extra side-effect patterns for handoff cards. See the doc comment in `src/config.rs` for the format.

In a session where some user messages name their speaker, unsigned user messages are treated as harness input (scheduled prompts), so they are not mistaken for the person.

### Connectors (`src/connectors/`)

```rust
trait Connector {
    fn name(&self) -> &'static str;
    fn locations(&self) -> &'static [&'static str];
    fn discover(&self, home: &Path) -> Vec<PathBuf>;     // files or databases
    fn parse(&self, path: &Path) -> Result<Vec<Session>>; // one file may hold many sessions
}
```

Rules every connector follows:

- Open sources read-only; tolerate malformed lines and unknown record types.
- Skip side chains and child sessions (sub-agents) so they don't duplicate the parent; skip rewound/inactive rows.
- Skip context the harness injects as user messages (Codex `AGENTS.md` instructions, environment context).
- Mark tool errors from the agent's own signal (`is_error`, exit codes, `status: error`).
- Fill missing timestamps forward (Cursor only stamps user turns).

To add an agent: add a file, register it in `connectors::all()`, add a fixture under `tests/fixtures/<agent>/` mirroring the real directory layout, and extend `tests/cli.rs`.

### Index (`src/index.rs`)

- One SQLite file, WAL mode, `0600`.
- `files(path, agent, mtime, size)` drives incremental updates: unchanged files are skipped, changed files are re-parsed and their sessions replaced, vanished files are removed. For SQLite stores the `-wal` file's mtime counts.
- `sessions` stores the sanitized session as JSON, so `show`/`handoff` don't depend on the source still existing.
- `turns` is FTS5 with the `trigram` tokenizer (substring and CJK search, no segmenter; terms under 3 characters fall back to `LIKE`). Three searchable columns, one per search level: `text` (what was said; input and reply up to 20,000 chars each), `act` (files written, commands run), `out` (tool output: the first 1,500 and last 500 characters of each call, up to 16,000 per turn; skipped when `[index] store_output = false`). BM25 weights 1.0 / 0.5 / 0.3. A hit's level is the shallowest column set that holds every term.
- Ranking: BM25 × recency (half-life 30 days, floor 0.3) × 0.6 for harness messages; at most two hits per session.
- `user_version` is the schema version; a mismatch rebuilds.

### Handoff (`src/handoff.rs`)

Deterministic extraction, no model call:

- goal = first human turn; then every later human turn (last 15)
- latest compaction summary, if any
- last input, last non-empty reply ("never replied" is called out)
- files written with counts
- side-effect actions from shell commands: commit, push, code review (`gh pr` / `glab mr` create, merge, edit, comment, review, close), publish, deploy, `crontab` edits, `rm -r`, plus any patterns the user adds under `[handoff.actions]` in their config
- errors, paired with the call that failed
- the agent's native resume command when known

The card ends with rules for the receiver: check real state, don't repeat side effects, ask when unclear.

### Wiki (`src/wiki.rs`)

`sessions/<date>-<agent>-<id>.md` (front matter, handoff card, transcript), `days/<date>.md`, `index.md` (by day, by project). Uses `[[wikilinks]]`. Pages are rewritten only when content changes, so the export can live in git.

### MCP (`src/mcp.rs`)

Line-delimited JSON-RPC 2.0 on stdio. `initialize`, `ping`, `tools/list`, `tools/call`; notifications get no reply. The index is refreshed once, on the first tool call.

### Agent space (`src/space.rs`, `src/mine.rs`, `src/hook.rs`)

The layout follows what worked in daily use with several bots sharing one person's work:

- **Always-loaded layer, capped.** `USER.md` (3000 chars), `NOW.md` (2000), and the generated `MEMORY.md` index; 8000 total. Caps force merging instead of silent growth (idea from Hermes Agent's hard memory limits).
- **One fact per file** with frontmatter: `source` (owner / agent / untrusted, after OpenClaw's provenance labels), `status` + `superseded_by` (facts are retired, not deleted, after Graphiti's validity windows), `private`.
- **Corrections go where they are used.** A correction about how to do a task belongs in that task's skill or wiki page; only general preferences go to `memory/feedback/` (from Hermes' review prompt).
- **Mistakes log** newest first; a recurring kind becomes a wiki checklist.
- **Mining is deterministic.** `corrections` uses phrase patterns in English and Chinese and drops texts repeated 3+ times (templates). `digest` gives a day's redacted transcript to whatever does the distilling (a scheduled agent, a person).
- **Notes are indexed** with the sessions (separate FTS table, 1.5× boost) for every space the index has seen (`init`, or any command run inside a space).
- In chat-bridge sessions (some user messages carry a sender), unsigned user messages are treated as harness input, so scheduled prompts are not mistaken for the user.

## Testing

- Unit tests next to the code (cleaning, redaction, tool classification, time parsing).
- `tests/cli.rs` builds a fake home with one session per agent (JSON fixtures + SQLite stores created in the test) and checks: every agent indexed, incremental no-op, CJK and short-term search, agent filter, file/command search, exclusion of side chains/child sessions/rewinds/injected context, redaction in output and index, `show` slicing, handoff fields for every connector, idempotent wiki, MCP protocol; `init` idempotence and settings merge, memory add/supersede/index, doctor catching planted problems, mistakes and wiki pages, notes in search, corrections, digest, and the nudge hook counting only human prompts.

## Roadmap

1. More connectors: Copilot CLI, Aider, Goose, Kimi CLI, Crush, Droid, Pi, Grok Build, Cline/Roo (VS Code storage), OpenCode 1.x JSON storage.
2. `agentstory watch`: keep the index fresh with file events.
3. Optional local embeddings for semantic recall, behind a feature flag.
4. Memory distillation hooks: emit candidate facts/decisions from new turns for a memory system to accept or reject.
5. Prebuilt binaries (GitHub Releases, Homebrew).
