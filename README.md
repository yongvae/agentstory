# agentstory

English · [中文](README.zh-CN.md)

**A lightweight agent-memory search tool written in Rust.** One 5 MB binary indexes the conversation history of every coding agent on your machine (Claude Code, Codex, Cursor, Gemini, Qwen, OpenCode, Hermes, Pi, DeepSeek Harness), answers searches in ~30 ms using ~15 MB of memory, and hands sessions from one agent to another. It also keeps a shared memory, wiki and mistakes log that every agent reads. Short command: `ags`.

| Measured on 1.45 GB of real agent history (339 sessions, 6,595 turns, Apple M3 Max) | |
|---|---|
| Binary | **5.1 MB**, single file, SQLite bundled, no runtime |
| Search (process start to results, incl. CJK) | **~32 ms** avg, **15 MB** peak memory, at any level |
| Handoff card | **~20 ms**, 21 MB |
| Refresh when nothing changed | **0.11 s**, 10 MB |
| Full index build | 13.3 s, 112 MB peak |
| Index on disk | 145 MB (10% of the raw history); 112 MB with `store_output = false` |
| MCP server idle | 8.6 MB |

Reproduce with `scripts/bench.sh` (uses a throwaway index).

Every coding agent writes its history somewhere different: Claude Code in JSONL under `~/.claude`, Codex in rollout files, Cursor in agent transcripts, OpenCode and Hermes in SQLite. When you run several agents (or several bots on one machine), none of them can see what the others did, and none of them can find what *they* did last week.

`agentstory` reads all of them in place, keeps a redacted full-text index, and gives you four things:

| | |
|---|---|
| `agentstory search` | Full-text search across every agent's sessions, including tool output (hits there are labelled `[output]` and rank below what was said). Works for Chinese/Japanese/Korean with no setup (trigram index), ranks by relevance and recency. |
| `agentstory show` | Read one session in order: whole, `--last 3` turns, or `--around <word>`. |
| `agentstory handoff` | A handoff card for continuing someone else's session: the goal, everything the user said since, where it stopped, files written, side-effect actions (commits, pushes, code reviews, chat messages, doc and task writes, deploys, schedules), errors. |
| `agentstory wiki` | Export sessions as Markdown pages + daily digests + index, so your history becomes part of a knowledge base. |

Plus `agentstory mcp`, an MCP server exposing the same as tools, and `--json` on every command, so agents can call it directly.

### Search levels

Most of the time an agent only needs the conversation: what the person asked and what was agreed. Sometimes it needs the details.

| `--level` | Searches | Use it for |
|---|---|---|
| `chat` (default) | What the person and the agent said to each other | Decisions, preferences, "what did we agree on" |
| `work` | + commands run and files written | "Which session touched `nav.css`", "who ran the migration" |
| `full` | + what tools printed | Error messages, stack traces, log lines |

Without `--level`, a `chat` search that finds nothing in the conversation retries at `full` and says so. Every hit reports where it matched (`"matched": "chat" \| "work" \| "output"`). The MCP `search_sessions` tool takes the same `level`.

### Agent space: memory, wiki, mistakes

Search tells you what happened. An **agent space** is where the lessons go, as plain Markdown every agent can read:

| | |
|---|---|
| `ags init [dir] --lang en\|zh` | Creates `AGENTS.md` (with `CLAUDE.md`/`GEMINI.md`/`QWEN.md` linked to it), `memory/` (`USER.md`, `NOW.md`, generated `MEMORY.md` index, one-fact-per-file entries), `wiki/`, `mistakes/`, `journal/`, a Cursor rule and a Claude Code hook. Never overwrites existing files; safe to re-run. |
| `ags mem add feedback small-batches -d "…"` | A memory entry with frontmatter (type, source: owner/agent/untrusted, status, private). `ags mem supersede old new` retires a fact without deleting it; the index is regenerated. |
| `ags mistake add "Pushed to main" -l "Open a branch" -t git` | A dated mistake page from the template, listed newest-first in `mistakes/index.md`. |
| `ags wiki add "Release checklist" -s "…"` | A wiki page from the template, listed in `wiki/index.md`. |
| `ags doctor` | Size caps on the always-loaded files, frontmatter, stale index, broken `[[links]]`, leaked keys or personal numbers, pending proposals. Exit code 2 on problems, so it can gate CI or a nightly job. |
| `ags corrections --since 7d` | Finds turns where the user corrected an agent ("don't…", "wrong", "next time…", "不要…", "以后…"), with the reply they reacted to: raw material for the mistakes log. |
| `ags digest 2026-11-04` | Everything said that day across all agents, redacted: input for a nightly distillation job. |
| `ags hook nudge` | A `UserPromptSubmit` hook: every N human prompts, reminds the agent to write down what it learned (long chat sessions never reach a natural end). |

Inside a space, `ags search` returns curated notes (memory, wiki, mistakes, journal) alongside conversation turns; `--only notes` or `--only sessions` narrows it.

## Supported agents

| Agent | Where it reads | Status |
|---|---|---|
| Claude Code | `~/.claude/projects/*/*.jsonl` (`$CLAUDE_CONFIG_DIR`) | tested on real data |
| Cursor agent (CLI + IDE) | `~/.cursor/projects/*/agent-transcripts/*/*.jsonl` | tested on real data |
| OpenAI Codex | `~/.codex/sessions/**/rollout-*.jsonl`, `archived_sessions/` (`$CODEX_HOME`) | tested on real data |
| Hermes Agent | `~/.hermes/state.db`, `profiles/*/state.db` (`$HERMES_HOME`) | tested on real data |
| Pi | `~/.pi/agent/sessions/*/*.jsonl` (`$PI_CODING_AGENT_DIR`) | tested on real data |
| DeepSeek Harness (dsh) | `~/.dsh/sessions/--*--/<id>/session.vN.jsonl[.zstd]` (`$DSH_HOME`) | fixture-tested (format v4, zstd frames) |
| Gemini CLI | `~/.gemini/tmp/*/chats/session-*.json` | fixture-tested |
| Qwen Code | `~/.qwen/projects/*/chats/*.jsonl` (`$QWEN_HOME`) | fixture-tested |
| OpenCode 2.x | `~/.local/share/opencode/opencode*.db` (`$XDG_DATA_HOME`) | fixture-tested |

`agentstory agents` shows what it finds on your machine. Adding an agent is one file in `src/connectors/` (see [docs/DESIGN.md](docs/DESIGN.md)).

## Install

```sh
brew install yongvae/tap/agentstory
```

Verified on macOS (Apple silicon) with Homebrew 7: installs in seconds from a prebuilt archive, `brew test` passes.

Installs two commands: `agentstory` and the short alias `ags`. Prebuilt archives for macOS (Apple silicon, Intel) and Linux (x86_64, arm64) are on the [releases page](https://github.com/yongvae/agentstory/releases).

## Use

```sh
agentstory index                          # first run builds the index; later runs re-read only changed files
agentstory search rate limiter            # all words must match
agentstory search "两个词" --agent codex --since 7d
agentstory sessions --since today         # newest first
agentstory sessions --grep deploy -p myrepo
agentstory show 3947cf05 --last 3         # id prefix, agent:id, or file path
agentstory show 3947cf05 --around 交接
agentstory handoff 3947cf05               # Markdown card; add --json for agents
agentstory wiki ~/notes/agent-sessions --since 30d --min-turns 2
```

Every query refreshes the index incrementally first (a stat per file; ~0.2 s on 330 sessions). Pass `--no-update` to skip it.

### Let your agents use it

**Easiest: ask your agent.** Send it this message:

> Install https://github.com/yongvae/agentstory and use it to search my past conversations with all my coding agents.

The agent installs the binary and copies [`skills/agentstory/`](skills/agentstory/SKILL.md) into its skills directory. To do it by hand:

```sh
brew install yongvae/tap/agentstory
git clone --depth 1 https://github.com/yongvae/agentstory /tmp/agentstory
cp -r /tmp/agentstory/skills/agentstory ~/.claude/skills/    # Claude Code
# or, for another agent that reads Agent Skills, its skills directory, e.g. ~/.agents/skills/
```

No separate indexing step is needed: the first command builds the index. The skill tells the agent when to search, how to read a hit, and to check a handoff card before continuing someone else's work. Agents without skill support can use the MCP server or an `AGENTS.md` line instead.

**MCP** (Claude Code, Codex, Cursor, Gemini CLI, OpenCode, Hermes, …):

```sh
claude mcp add agentstory -- agentstory mcp
codex mcp add agentstory -- agentstory mcp
```

or in any MCP config: `{"command": "agentstory", "args": ["mcp"]}`. Tools: `search_sessions`, `list_sessions`, `read_session`, `handoff`.

**Plain CLI** works for anything that can run shell commands. A line in `AGENTS.md` / `CLAUDE.md` is enough:

> Before answering questions about past work, run `agentstory search <words> --json`. To continue another agent's session, run `agentstory handoff <id>` first and do not repeat side effects listed there.

## What it does not do

- It never writes to agent data. Sources are opened read-only.
- No network, no telemetry, no model calls. Handoff cards are extracted mechanically, so they are fast and never invent progress.
- Not a semantic/vector search (yet). Trigram full-text covers exact words, partial words and CJK.

## Configuration (optional)

Plain agent sessions need no configuration. If your agents run behind a chat bridge or another harness that wraps messages in its own markup, describe it in `~/.config/agentstory/config.toml` so search and handoff cards see the person's words, the speaker's name and the replies the agent sent through the bridge:

```toml
[transcript]
drop_tags = ["router_*"]            # wrapper elements to ignore
message_tags = ["chat_message"]     # <chat_message>…</chat_message> holds what the person said
speaker_tag = "from"                # <from name="Alice"/>
relay_commands = ["chatctl send"]   # shell commands whose text is the agent's chat reply
silent_replies = ["NO_REPLY"]

[index]
store_output = true                 # false: skip tool output for a smaller index (levels chat/work only)

[handoff.actions]                   # extra side effects to list on handoff cards
message = '(\bchatctl\s+send\b[^\n;&|]{0,120})'
```

## Why it is light

- **No daemon, no model, no embeddings.** Each command is a short-lived process; nothing runs in the background.
- **SQLite FTS5 with the trigram tokenizer.** Substring and CJK search with no segmenter or model download; terms under 3 characters fall back to `LIKE`.
- **Incremental.** A refresh stats each source file and re-reads only changed ones; SQLite stores count their `-wal` file.
- **Stores the gist, not the raw log.** Tool outputs keep their first 1,500 and last 500 characters (errors are usually at the end), secrets are redacted, sessions are kept as compact normalized JSON, so the index is ~10% of the raw history.

## Privacy

The index lives at `<data dir>/agentstory/agentstory.db` (macOS: `~/Library/Application Support/agentstory/`, Linux: `~/.local/share/agentstory/`), created `0600` in a `0700` directory. Before anything is stored or printed, `agentstory` redacts private keys, API keys (`sk-…`, `ghp_…`, `AKIA…`, `AIza…`), JWTs, bearer tokens, `token=`/`password=`-style assignments, long opaque strings, and phone/ID numbers. Redaction is pattern-based: treat the index and wiki exports as sensitive anyway.

## Related projects

- [coding_agent_session_search (cass)](https://github.com/Dicklesworthstone/coding_agent_session_search): a much larger Rust TUI/CLI with 30+ connectors and semantic search. Note its license adds restrictions beyond MIT. agentstory was written independently and shares no code with it.
- [SpecStory CLI](https://github.com/specstoryai/getspecstory) (Apache-2.0): saves sessions as Markdown in your repo. Its format notes were the reference for the Gemini, Qwen and OpenCode connectors.
- [Agent Sessions](https://github.com/jazzyalex/agent-sessions): a macOS app for browsing sessions.

agentstory's focus is the multi-agent case: one agent picking up where another stopped, and session history feeding a shared memory, wiki and mistakes log that every agent reads.

## License

MIT
