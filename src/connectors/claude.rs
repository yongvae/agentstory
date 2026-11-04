//! Claude Code: `~/.claude/projects/<cwd-slug>/<session-uuid>.jsonl`, one JSON event per line.

use super::{Connector, mtime_ms, stem, text_of, tool_call, ts_ms};
use crate::clean::{relayed_reply, user_text};
use crate::model::{Message, Role, Session};
use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

pub struct ClaudeCode;

impl Connector for ClaudeCode {
    fn name(&self) -> &'static str {
        "claude-code"
    }
    fn locations(&self) -> &'static [&'static str] {
        &[
            "~/.claude/projects/*/*.jsonl",
            "$CLAUDE_CONFIG_DIR/projects/*/*.jsonl",
        ]
    }
    fn discover(&self, home: &Path) -> Vec<PathBuf> {
        let mut roots = vec![home.join(".claude/projects")];
        if let Ok(d) = std::env::var("CLAUDE_CONFIG_DIR") {
            roots.push(PathBuf::from(d).join("projects"));
        }
        roots
            .iter()
            .flat_map(|r| super::files_with_ext(r, "jsonl", 2))
            .collect()
    }
    fn parse(&self, path: &Path) -> Result<Vec<Session>> {
        Ok(vec![parse_file(path)?])
    }
}

pub fn parse_file(path: &Path) -> Result<Session> {
    let reader = BufReader::new(std::fs::File::open(path)?);
    let mut s = Session {
        id: stem(path),
        agent: "claude-code".into(),
        source: path.display().to_string(),
        project: None,
        title: None,
        model: None,
        started: None,
        updated: None,
        messages: vec![],
    };
    let mut seen = HashSet::new();
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        let Ok(o) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if o.get("isSidechain").and_then(|v| v.as_bool()) == Some(true) {
            continue;
        }
        if let Some(u) = o.get("uuid").and_then(|v| v.as_str())
            && !seen.insert(u.to_string())
        {
            continue;
        }
        if s.project.is_none() {
            s.project = o.get("cwd").and_then(|v| v.as_str()).map(String::from);
        }
        let kind = o.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let ts = o.get("timestamp").and_then(ts_ms);
        let Some(msg) = o.get("message") else {
            continue;
        };
        if s.model.is_none() {
            s.model = msg
                .get("model")
                .and_then(|v| v.as_str())
                .filter(|m| !m.starts_with('<'))
                .map(String::from);
        }
        let blocks: Vec<Value> = match msg.get("content") {
            Some(Value::String(t)) => vec![serde_json::json!({"type":"text","text":t})],
            Some(Value::Array(a)) => a.clone(),
            _ => continue,
        };
        match kind {
            "user" => {
                let compact = o.get("isCompactSummary").and_then(|v| v.as_bool()) == Some(true);
                for b in &blocks {
                    match b.get("type").and_then(|v| v.as_str()) {
                        Some("text") => {
                            let raw = b.get("text").and_then(|v| v.as_str()).unwrap_or("");
                            if let Some(c) = user_text(raw) {
                                let summary = compact || c.is_summary;
                                let role = if c.is_system {
                                    Role::System
                                } else {
                                    Role::User
                                };
                                let mut m =
                                    Message::new(if summary { Role::System } else { role }, c.text)
                                        .at(ts);
                                m.author = c.author;
                                m.is_summary = summary;
                                s.messages.push(m);
                            }
                        }
                        Some("tool_result") => {
                            let mut m = Message::new(
                                Role::Tool,
                                crate::clean::tool_output(&text_of(
                                    b.get("content").unwrap_or(&Value::Null),
                                )),
                            )
                            .at(ts);
                            m.result_of = b
                                .get("tool_use_id")
                                .and_then(|v| v.as_str())
                                .map(String::from);
                            m.is_error = b.get("is_error").and_then(|v| v.as_bool()) == Some(true);
                            s.messages.push(m);
                        }
                        _ => {}
                    }
                }
            }
            "assistant" => {
                for b in &blocks {
                    match b.get("type").and_then(|v| v.as_str()) {
                        Some("text") => {
                            let t = b.get("text").and_then(|v| v.as_str()).unwrap_or("").trim();
                            if !crate::clean::is_silent(t) {
                                s.messages.push(Message::new(Role::Assistant, t).at(ts));
                            }
                        }
                        Some("tool_use") => {
                            let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                            let input = b.get("input").cloned().unwrap_or(Value::Null);
                            let call = tool_call(
                                name,
                                &input,
                                b.get("id").and_then(|v| v.as_str()).map(String::from),
                            );
                            if let Some(reply) = call.command.as_deref().and_then(relayed_reply) {
                                s.messages.push(Message::new(Role::Assistant, reply).at(ts));
                            }
                            let mut m = Message::new(Role::Tool, String::new()).at(ts);
                            m.tool = Some(call);
                            s.messages.push(m);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    s.finish(mtime_ms(path));
    Ok(s)
}
