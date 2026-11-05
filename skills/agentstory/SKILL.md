---
name: agentstory
description: Search and read the user's past conversations with every local coding agent (Claude Code, Codex, Cursor, Gemini CLI, Qwen Code, OpenCode, Hermes, Pi, DeepSeek Harness) using the `ags` CLI, and get a handoff card to continue a session another agent started. Use when the user asks about past work ("what did we decide about X", "which session changed this file", "have we seen this error before", "pick up where the other agent stopped"), or before redoing something that may already have been done.
---

# agentstory (`ags`)

`ags` keeps a local full-text index of all coding-agent sessions on this machine. It is read-only, offline and makes no model calls. There is no separate setup step: the first command builds the index, and every command refreshes it incrementally before answering.

## Check it is installed

```sh
ags --version || agentstory --version
```

If neither exists, install it (macOS / Linux):

```sh
brew install yongvae/tap/agentstory
```

Without Homebrew, download the archive for this platform from https://github.com/yongvae/agentstory/releases, extract it, and put `agentstory` and `ags` on the PATH.

## Commands

| Need | Command |
|---|---|
| Find what was said or decided | `ags search <words> --json` |
| Narrow it down | add `-a codex`, `--since 7d`, `-p <project dir text>` |
| Find commands run, files changed | `ags search <words> --level work --json` |
| Find error messages, logs | `ags search <words> --level full --json` |
| List recent sessions | `ags sessions --since today --json` |
| Read the matching part of a session | `ags show <id> --around <words> --context 0` (raise `--context` for more turns around each hit) |
| Read the end of a session | `ags show <id> --last 3` |
| Continue another agent's session | `ags handoff <id>` |
| See which agents were found | `ags agents` |

`<id>` can be an id prefix from search results, `agent:id`, or a file path. Words shorter than 3 characters use substring matching; Chinese and other CJK text works directly.

## Rules

1. Before answering a question about past work, search first. Answer from what the results say and cite the session id and date. If nothing is found, say so; do not guess.
2. Search finds the right turns; read them with `ags show <id> --around <words>` before relying on a snippet.
3. When continuing another session, run `ags handoff <id>` first. Check the real state (files, git, remote) before acting. **Do not repeat side effects the card lists** (commits, pushes, PR/MR actions, releases, deploys, scheduled jobs, deletions); ask if unsure.
4. Results are redacted, but they can still contain private details. Quote only what the user needs.
