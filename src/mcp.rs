//! Minimal MCP server over stdio (JSON-RPC 2.0, one message per line), so any MCP-capable agent
//! can search and read other agents' sessions without shelling out.

use crate::handoff;
use crate::index::{Filter, Index, Level};
use crate::render::{self, View};
use crate::time::{fmt_local, parse_when};
use anyhow::Result;
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::PathBuf;

const PROTOCOL: &str = "2025-06-18";

fn tools() -> Value {
    json!([
        {
            "name": "search_sessions",
            "description": "Full-text search over past conversations of every local coding agent (Claude Code, Codex, Cursor, Gemini, Qwen, OpenCode, Hermes). Returns matching turns with session ids. Works with Chinese and partial words.",
            "inputSchema": {"type": "object", "properties": {
                "query": {"type": "string", "description": "Words to find; all must match. Quote a phrase to keep it together."},
                "level": {"type": "string", "enum": ["chat", "work", "full"], "description": "chat (default): only what the person and the agent said; work: plus commands run and files written; full: plus tool output. If chat finds nothing, full is tried automatically."},
                "agent": {"type": "string", "description": "Only this agent, e.g. claude-code, codex, cursor"},
                "since": {"type": "string", "description": "2026-11-01, 7d, 24h, today"},
                "project": {"type": "string", "description": "Substring of the working directory"},
                "limit": {"type": "integer", "default": 8}
            }, "required": ["query"]}
        },
        {
            "name": "list_sessions",
            "description": "List recent sessions across agents, newest first, with id, agent, project, title and turn counts.",
            "inputSchema": {"type": "object", "properties": {
                "agent": {"type": "string"}, "since": {"type": "string"}, "project": {"type": "string"},
                "contains": {"type": "string", "description": "Only sessions mentioning this text"},
                "limit": {"type": "integer", "default": 20}
            }}
        },
        {
            "name": "read_session",
            "description": "Read a session in order. Use `last` for the latest turns or `around` to jump to where a word appears.",
            "inputSchema": {"type": "object", "properties": {
                "id": {"type": "string", "description": "Session id, unique prefix, or agent:id"},
                "last": {"type": "integer"}, "around": {"type": "string"},
                "context": {"type": "integer", "default": 2}
            }, "required": ["id"]}
        },
        {
            "name": "handoff",
            "description": "Handoff card for continuing another session: goal, everything the user said since, where it stopped, files written, side-effect actions, errors.",
            "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}
        }
    ])
}

fn s(args: &Value, k: &str) -> Option<String> {
    args.get(k)
        .and_then(|v| v.as_str())
        .map(String::from)
        .filter(|x| !x.is_empty())
}
fn n(args: &Value, k: &str) -> Option<usize> {
    args.get(k).and_then(|v| v.as_u64()).map(|x| x as usize)
}

fn filter(args: &Value) -> Result<Filter> {
    Ok(Filter {
        agents: s(args, "agent").into_iter().collect(),
        since: s(args, "since").map(|x| parse_when(&x)).transpose()?,
        until: None,
        project: s(args, "project"),
        source: None,
        level: s(args, "level")
            .map(|l| Level::parse(&l))
            .transpose()?
            .unwrap_or_default(),
    })
}

fn call(idx: &Index, name: &str, args: &Value) -> Result<String> {
    match name {
        "search_sessions" => {
            let q = s(args, "query").unwrap_or_default();
            let explicit = s(args, "level").is_some();
            let (hits, used) = idx.search_auto(
                &q,
                &filter(args)?,
                n(args, "limit").unwrap_or(8),
                false,
                explicit,
            )?;
            if hits.is_empty() {
                return Ok("No matches.".into());
            }
            let note = if !explicit && used == Level::Full {
                "(nothing in the conversation itself; these come from commands and tool output)\n\n"
            } else {
                ""
            };
            Ok(note.to_string()
                + &hits
                    .iter()
                    .map(|h| {
                        format!(
                            "[{}] {} turn {} · {} · {}\n{}",
                            h.key,
                            fmt_local(h.ts),
                            h.turn,
                            h.speaker,
                            h.project.clone().unwrap_or_default(),
                            h.snippet
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n"))
        }
        "list_sessions" => {
            let rows = idx.sessions(
                &filter(args)?,
                s(args, "contains").as_deref(),
                n(args, "limit").unwrap_or(20),
            )?;
            Ok(rows
                .iter()
                .map(|r| {
                    format!(
                        "{} · {} → {} · {} human turns · {}\n  {}",
                        r.key,
                        fmt_local(r.started),
                        fmt_local(r.updated),
                        r.user_turns,
                        r.project.clone().unwrap_or_default(),
                        r.title.clone().unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "read_session" => {
            let row = idx.resolve(&s(args, "id").unwrap_or_default())?;
            let sess = idx.load(&row.key)?;
            let v = View {
                last: n(args, "last"),
                around: s(args, "around"),
                context: n(args, "context").unwrap_or(2),
                tools: false,
            };
            Ok(render::markdown(&sess, &v))
        }
        "handoff" => {
            let row = idx.resolve(&s(args, "id").unwrap_or_default())?;
            Ok(handoff::markdown(&handoff::build(&idx.load(&row.key)?)))
        }
        other => anyhow::bail!("unknown tool {other}"),
    }
}

pub fn serve(db: PathBuf, home: PathBuf) -> Result<()> {
    let mut idx = Index::open(&db)?;
    let mut fresh = false;
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            writeln!(
                out,
                "{}",
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}})
            )?;
            out.flush()?;
            continue;
        };
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let result: Result<Value, (i64, String)> = match method {
            "initialize" => Ok(json!({
                "protocolVersion": req.pointer("/params/protocolVersion").and_then(|v| v.as_str()).unwrap_or(PROTOCOL),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "agentstory", "version": env!("CARGO_PKG_VERSION")}
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => {
                if !fresh {
                    let _ = idx.update(&home, &[], false);
                    fresh = true;
                }
                let name = req
                    .pointer("/params/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let args = req
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or(json!({}));
                match call(&idx, name, &args) {
                    Ok(text) => {
                        Ok(json!({"content": [{"type": "text", "text": text}], "isError": false}))
                    }
                    Err(e) => Ok(
                        json!({"content": [{"type": "text", "text": format!("{e:#}")}], "isError": true}),
                    ),
                }
            }
            m if m.starts_with("notifications/") => continue,
            m => Err((-32601, format!("method not found: {m}"))),
        };
        if id.is_none() {
            continue;
        }
        let msg = match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err((code, message)) => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
            }
        };
        writeln!(out, "{msg}")?;
        out.flush()?;
    }
    Ok(())
}
