//! DeepSeek Harness (dsh): `$DSH_HOME/sessions/--<project-key>--/<session-id>/session[.vN].jsonl[.zstd]`
//! (`DSH_HOME` defaults to `~/.dsh`). Each session folder may hold several format generations
//! (older ones are kept when a session is upgraded); only the highest is read. Files are usually
//! a series of zstd frames. Line 1 is a header; every other line is an event `{type, seq, time, data}`.
//! Format read from deepseek-ai/deepseek-harness (MIT), packages/session/*, format v4, with the
//! v2/v3 tool-result shape tolerated.

use super::{Connector, mtime_ms, text_of, tool_call};
use crate::clean::{relayed_reply, user_text};
use crate::model::{Message, Role, Session};
use anyhow::{Result, bail};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub struct Dsh;

static LOG_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^session(?:\.v([1-9][0-9]*))?\.jsonl(\.zstd)?$").unwrap());
static EXIT_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[exit code: (-?\d+)\]\s*$").unwrap());

fn version_of(p: &Path) -> Option<u32> {
    let name = p.file_name()?.to_string_lossy().into_owned();
    let c = LOG_NAME.captures(&name)?;
    Some(
        c.get(1)
            .map(|m| m.as_str().parse().unwrap_or(0))
            .unwrap_or(0),
    )
}

/// Concatenated zstd frames → bytes. A cut-off last frame keeps what decoded cleanly.
fn read_zstd(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cursor = bytes;
    while !cursor.is_empty() {
        let Ok(mut dec) = ruzstd::decoding::StreamingDecoder::new(&mut cursor) else {
            break;
        };
        let before = out.len();
        if dec.read_to_end(&mut out).is_err() {
            // Drop a partial final line from a torn frame.
            if let Some(nl) = out[before..].iter().rposition(|b| *b == b'\n') {
                out.truncate(before + nl + 1);
            } else {
                out.truncate(before);
            }
            break;
        }
    }
    out
}

fn read_log(p: &Path) -> Result<String> {
    let bytes = std::fs::read(p)?;
    let raw = if p.extension().is_some_and(|e| e == "zstd") {
        read_zstd(&bytes)
    } else {
        bytes
    };
    Ok(String::from_utf8_lossy(&raw).into_owned())
}

impl Connector for Dsh {
    fn name(&self) -> &'static str {
        "dsh"
    }
    fn locations(&self) -> &'static [&'static str] {
        &[
            "~/.dsh/sessions/--*--/<id>/session.vN.jsonl[.zstd]",
            "$DSH_HOME/sessions/…",
        ]
    }
    fn discover(&self, home: &Path) -> Vec<PathBuf> {
        let base = std::env::var("DSH_HOME")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".dsh"));
        let root = base.join("sessions");
        if !root.exists() {
            return vec![];
        }
        // Highest generation per session folder.
        let mut best: HashMap<PathBuf, (u32, PathBuf)> = HashMap::new();
        for e in walkdir::WalkDir::new(&root)
            .max_depth(3)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !e.file_type().is_file() {
                continue;
            }
            let Some(v) = version_of(e.path()) else {
                continue;
            };
            let dir = e.path().parent().unwrap().to_path_buf();
            match best.get(&dir) {
                Some((bv, _)) if *bv >= v => {}
                _ => {
                    best.insert(dir, (v, e.path().to_path_buf()));
                }
            }
        }
        best.into_values().map(|(_, p)| p).collect()
    }
    fn parse(&self, path: &Path) -> Result<Vec<Session>> {
        let text = read_log(path)?;
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        let Some(header) = lines
            .next()
            .and_then(|l| serde_json::from_str::<Value>(l).ok())
        else {
            bail!("no header")
        };
        if header.get("type").and_then(|v| v.as_str()) != Some("session") {
            bail!("first line is not a session header");
        }
        if header.get("origin").and_then(|v| v.as_str()) == Some("subagent") {
            return Ok(vec![]); // children are summarized in the parent
        }
        let cwd = header.get("cwd").and_then(|v| v.as_str()).map(String::from);
        let mut s = Session {
            id: header
                .get("id")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| super::stem(path)),
            agent: "dsh".into(),
            source: path.display().to_string(),
            project: cwd.clone(),
            title: None,
            model: None,
            started: header.get("createdAt").and_then(|v| v.as_i64()),
            updated: None,
            messages: vec![],
        };
        let abs = |f: &str| -> String {
            match &cwd {
                Some(c) if !f.starts_with('/') && !f.is_empty() => {
                    format!("{}/{}", c.trim_end_matches('/'), f)
                }
                _ => f.to_string(),
            }
        };
        let mut shell_calls: HashMap<String, bool> = HashMap::new();
        for line in lines {
            let Ok(ev) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let ts = ev.get("time").and_then(|v| v.as_i64());
            let d = ev.get("data").cloned().unwrap_or(Value::Null);
            match ev.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                "user/message" => {
                    let kind = d
                        .pointer("/source/kind")
                        .and_then(|v| v.as_str())
                        .unwrap_or("user");
                    let raw = text_of(d.get("content").unwrap_or(&Value::Null));
                    if kind == "compact-checkpoint" {
                        continue; // the compaction/summary event carries the summary itself
                    }
                    if kind != "user" {
                        continue; // runtime context, instructions, skill catalogs…
                    }
                    if let Some(c) = user_text(&raw) {
                        let mut m = Message::new(
                            if c.is_system {
                                Role::System
                            } else {
                                Role::User
                            },
                            c.text,
                        )
                        .at(ts);
                        m.author = c.author;
                        s.messages.push(m);
                    }
                }
                "assistant/message" => {
                    let msg = d.get("message").cloned().unwrap_or(Value::Null);
                    if s.model.is_none() {
                        s.model = msg
                            .pointer("/source/model")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                    }
                    for b in msg
                        .get("content")
                        .and_then(|v| v.as_array())
                        .into_iter()
                        .flatten()
                    {
                        if b.get("type").and_then(|v| v.as_str()) == Some("text") {
                            let t = b.get("text").and_then(|v| v.as_str()).unwrap_or("").trim();
                            if !crate::clean::is_silent(t) {
                                s.messages.push(Message::new(Role::Assistant, t).at(ts));
                            }
                        }
                    }
                }
                "request/context" if s.model.is_none() => {
                    s.model = d.get("model").and_then(|v| v.as_str()).map(String::from);
                }
                "tool/call" => {
                    let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                    let mut args = match d.get("arguments") {
                        Some(Value::String(a)) => serde_json::from_str(a).unwrap_or(Value::Null),
                        Some(v) => v.clone(),
                        None => Value::Null,
                    };
                    for k in ["file_path", "path"] {
                        if let Some(Value::String(p)) = args.get(k).cloned() {
                            args[k] = Value::String(abs(&p));
                        }
                    }
                    let id = d.get("callId").and_then(|v| v.as_str()).map(String::from);
                    let call = tool_call(name, &args, id.clone());
                    if let Some(id) = &id {
                        shell_calls.insert(id.clone(), call.kind == crate::model::ToolKind::Shell);
                    }
                    if let Some(reply) = call.command.as_deref().and_then(relayed_reply) {
                        s.messages.push(Message::new(Role::Assistant, reply).at(ts));
                    }
                    let mut m = Message::new(Role::Tool, String::new()).at(ts);
                    m.tool = Some(call);
                    s.messages.push(m);
                }
                "tool/result" => {
                    let msg = d.get("message").cloned().unwrap_or(Value::Null);
                    // v4: {role:"tool", toolCallId, content, isError}; v2/v3: content:[{type:"tool-result", …}]
                    let inner = msg
                        .get("content")
                        .and_then(|c| c.as_array())
                        .and_then(|a| {
                            a.iter().find(|b| {
                                b.get("type").and_then(|t| t.as_str()) == Some("tool-result")
                            })
                        })
                        .cloned();
                    let src = inner.as_ref().unwrap_or(&msg);
                    let out = text_of(src.get("content").unwrap_or(&Value::Null));
                    let id = src
                        .get("toolCallId")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    let flagged = src.get("isError").and_then(|v| v.as_bool()) == Some(true)
                        || d.get("error").is_some_and(|e| !e.is_null());
                    let exit_fail = id
                        .as_ref()
                        .and_then(|i| shell_calls.get(i))
                        .copied()
                        .unwrap_or(false)
                        && EXIT_SUFFIX
                            .captures(out.trim_end())
                            .is_some_and(|c| &c[1] != "0");
                    let mut m = Message::new(Role::Tool, crate::clean::tool_output(&out)).at(ts);
                    m.result_of = id;
                    m.is_error = flagged || exit_fail;
                    s.messages.push(m);
                }
                "compaction/summary" => {
                    let sum = text_of(d.get("summary").unwrap_or(&Value::Null));
                    if !sum.trim().is_empty() {
                        let mut m = Message::new(Role::System, sum).at(ts);
                        m.is_summary = true;
                        s.messages.push(m);
                    }
                }
                "session/title" => {
                    if let Some(t) = d.get("title").and_then(|v| v.as_str()) {
                        s.title = Some(t.to_string());
                    }
                }
                _ => {}
            }
        }
        s.finish(mtime_ms(path));
        Ok(vec![s])
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn picks_generation_from_name() {
        use std::path::Path;
        assert_eq!(super::version_of(Path::new("a/session.jsonl")), Some(0));
        assert_eq!(
            super::version_of(Path::new("a/session.v4.jsonl.zstd")),
            Some(4)
        );
        assert_eq!(super::version_of(Path::new("a/session.lock")), None);
        assert_eq!(super::version_of(Path::new("a/session.v04.jsonl")), None);
    }
}
