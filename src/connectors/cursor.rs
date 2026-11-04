//! Cursor agent (CLI `cursor-agent` and IDE agent):
//! `~/.cursor/projects/<path-slug>/agent-transcripts/<id>/<id>.jsonl`.
//! Lines are `{"role": "user"|"assistant", "message": {"content": [blocks]}}`. Tool results are not
//! recorded; timestamps only appear as `<timestamp>…</timestamp>` inside user text.

use super::{Connector, mtime_ms, stem, tool_call};
use crate::clean::{relayed_reply, user_text};
use crate::model::{Message, Role, Session};
use anyhow::Result;
use regex::Regex;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub struct CursorAgent;

static TS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<timestamp>\s*\w+, (\w+ \d{1,2}, \d{4}, \d{1,2}:\d{2} [AP]M) \(UTC([+-]\d{1,2})(?::?(\d{2}))?\)\s*</timestamp>").unwrap()
});

fn parse_ts(raw: &str) -> Option<i64> {
    let c = TS.captures(raw)?;
    let naive = chrono::NaiveDateTime::parse_from_str(&c[1], "%b %d, %Y, %I:%M %p").ok()?;
    let h: i32 = c[2].parse().ok()?;
    let m: i32 = c
        .get(3)
        .map(|x| x.as_str().parse().unwrap_or(0))
        .unwrap_or(0);
    let off = chrono::FixedOffset::east_opt(h.signum() * (h.abs() * 3600 + m * 60))?;
    Some(naive.and_local_timezone(off).single()?.timestamp_millis())
}

impl Connector for CursorAgent {
    fn name(&self) -> &'static str {
        "cursor"
    }
    fn locations(&self) -> &'static [&'static str] {
        &["~/.cursor/projects/*/agent-transcripts/*/*.jsonl"]
    }
    fn discover(&self, home: &Path) -> Vec<PathBuf> {
        let root = home.join(".cursor/projects");
        let Ok(rd) = std::fs::read_dir(&root) else {
            return vec![];
        };
        rd.filter_map(|e| e.ok())
            .flat_map(|e| super::files_with_ext(&e.path().join("agent-transcripts"), "jsonl", 2))
            .collect()
    }
    fn parse(&self, path: &Path) -> Result<Vec<Session>> {
        let reader = BufReader::new(std::fs::File::open(path)?);
        // …/projects/<slug>/agent-transcripts/<id>/<id>.jsonl
        let project = path
            .ancestors()
            .nth(3)
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned());
        let mut s = Session {
            id: stem(path),
            agent: "cursor".into(),
            source: path.display().to_string(),
            project,
            title: None,
            model: None,
            started: None,
            updated: None,
            messages: vec![],
        };
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            let Ok(o) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let role = o.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let Some(Value::Array(blocks)) = o.get("message").and_then(|m| m.get("content")) else {
                continue;
            };
            for b in blocks {
                match (role, b.get("type").and_then(|v| v.as_str())) {
                    ("user", Some("text")) => {
                        let raw = b.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        if let Some(c) = user_text(raw) {
                            let role = if c.is_summary || c.is_system {
                                Role::System
                            } else {
                                Role::User
                            };
                            let mut m = Message::new(role, c.text).at(parse_ts(raw));
                            m.author = c.author;
                            m.is_summary = c.is_summary;
                            s.messages.push(m);
                        }
                    }
                    ("assistant", Some("text")) => {
                        let t = b.get("text").and_then(|v| v.as_str()).unwrap_or("").trim();
                        if !crate::clean::is_silent(t) {
                            s.messages.push(Message::new(Role::Assistant, t));
                        }
                    }
                    ("assistant", Some("tool_use")) => {
                        let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                        let call = tool_call(name, b.get("input").unwrap_or(&Value::Null), None);
                        if let Some(r) = call.command.as_deref().and_then(relayed_reply) {
                            s.messages.push(Message::new(Role::Assistant, r));
                        }
                        let mut m = Message::new(Role::Tool, String::new());
                        m.tool = Some(call);
                        s.messages.push(m);
                    }
                    _ => {}
                }
            }
        }
        s.finish(mtime_ms(path));
        Ok(vec![s])
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_cursor_timestamp() {
        let t = super::parse_ts("<timestamp>Tuesday, Nov 3, 2026, 3:16 PM (UTC+8)</timestamp>")
            .unwrap();
        assert_eq!(t, 1793690160000);
    }
}
