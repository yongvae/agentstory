//! OpenCode 2.x: SQLite `$XDG_DATA_HOME/opencode/opencode.db` (default `~/.local/share/opencode`),
//! tables `session_v2` and `session_message` (`type` + JSON `data`). Tool calls and their results
//! live in the same assistant content part. Format reference: SpecStory CLI
//! `pkg/providers/opencode/OPENCODE-FORMAT.md` (Apache-2.0).

use super::{Connector, mtime_ms, text_of, tool_call};
use crate::clean::user_text;
use crate::model::{Message, Role, Session};
use anyhow::Result;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct OpenCode;

impl Connector for OpenCode {
    fn name(&self) -> &'static str {
        "opencode"
    }
    fn locations(&self) -> &'static [&'static str] {
        &[
            "~/.local/share/opencode/opencode*.db",
            "$XDG_DATA_HOME/opencode/…",
        ]
    }
    fn discover(&self, home: &Path) -> Vec<PathBuf> {
        let base = std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".local/share"))
            .join("opencode");
        let Ok(rd) = std::fs::read_dir(&base) else {
            return vec![];
        };
        rd.filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                let n = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                n.starts_with("opencode") && n.ends_with(".db")
            })
            .collect()
    }
    fn parse(&self, path: &Path) -> Result<Vec<Session>> {
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let has_v2: bool = db
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='session_v2'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)?;
        if !has_v2 {
            return Ok(vec![]);
        }
        let mut sessions = vec![];
        let mut index = HashMap::new();
        let mut stmt = db.prepare(
            "SELECT id, directory, title, time_created, time_updated, parent_id FROM session_v2",
        )?;
        for row in stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })?
            .flatten()
        {
            let (id, dir, title, created, updated, parent) = row;
            if parent.is_some() {
                continue; // subagent child sessions: their result already appears in the parent
            }
            index.insert(id.clone(), sessions.len());
            sessions.push(Session {
                id,
                agent: "opencode".into(),
                source: path.display().to_string(),
                project: dir,
                title: title.filter(|t| !t.is_empty()),
                model: None,
                started: created,
                updated,
                messages: vec![],
            });
        }
        let mut stmt = db.prepare("SELECT session_id, type, time_created, data FROM session_message ORDER BY session_id, seq")?;
        for row in stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .flatten()
        {
            let (sid, kind, ts, data) = row;
            let Some(&i) = index.get(&sid) else { continue };
            let Ok(d) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            let s = &mut sessions[i];
            match kind.as_str() {
                "user" => {
                    if let Some(c) = user_text(d.get("text").and_then(|v| v.as_str()).unwrap_or(""))
                    {
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
                "shell" => {
                    // A user `!command`.
                    let cmd = d.get("command").and_then(|v| v.as_str()).unwrap_or("");
                    let mut m = Message::new(Role::Tool, String::new()).at(ts);
                    m.tool = Some(tool_call(
                        "shell",
                        &serde_json::json!({ "command": cmd }),
                        None,
                    ));
                    s.messages.push(m);
                }
                "assistant" => {
                    if s.model.is_none() {
                        s.model = d
                            .pointer("/model/id")
                            .and_then(|v| v.as_str())
                            .filter(|x| !x.is_empty())
                            .map(String::from);
                    }
                    for p in d
                        .get("content")
                        .and_then(|v| v.as_array())
                        .into_iter()
                        .flatten()
                    {
                        match p.get("type").and_then(|v| v.as_str()) {
                            Some("text") => {
                                if let Some(t) = p
                                    .get("text")
                                    .and_then(|v| v.as_str())
                                    .filter(|t| !t.trim().is_empty())
                                {
                                    s.messages
                                        .push(Message::new(Role::Assistant, t.trim()).at(ts));
                                }
                            }
                            Some("tool") => {
                                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                                let id = p.get("id").and_then(|v| v.as_str()).map(String::from);
                                let state = p.get("state").cloned().unwrap_or(Value::Null);
                                let input = state.get("input").cloned().unwrap_or(Value::Null);
                                let mut m = Message::new(Role::Tool, String::new()).at(ts);
                                m.tool = Some(tool_call(name, &input, id.clone()));
                                s.messages.push(m);
                                let status =
                                    state.get("status").and_then(|v| v.as_str()).unwrap_or("");
                                let out = state
                                    .pointer("/error/message")
                                    .and_then(|v| v.as_str())
                                    .map(String::from)
                                    .unwrap_or_else(|| {
                                        text_of(state.get("content").unwrap_or(&Value::Null))
                                    });
                                let mut r =
                                    Message::new(Role::Tool, crate::clean::tool_output(&out))
                                        .at(ts);
                                r.result_of = id;
                                r.is_error = status == "error"
                                    || state.pointer("/metadata/error").and_then(|v| v.as_bool())
                                        == Some(true)
                                    || state
                                        .pointer("/metadata/exit")
                                        .and_then(|v| v.as_i64())
                                        .is_some_and(|c| c != 0);
                                s.messages.push(r);
                            }
                            _ => {}
                        }
                    }
                    if let Some(e) = d.pointer("/error/message").and_then(|v| v.as_str()) {
                        let mut m = Message::new(Role::System, e).at(ts);
                        m.is_error = true;
                        s.messages.push(m);
                    }
                }
                "compaction" => {
                    if let Some(sum) = d
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .filter(|x| !x.is_empty())
                    {
                        let mut m = Message::new(Role::System, sum).at(ts);
                        m.is_summary = true;
                        s.messages.push(m);
                    }
                }
                _ => {}
            }
        }
        let fallback = mtime_ms(path);
        for s in &mut sessions {
            s.finish(fallback);
        }
        Ok(sessions
            .into_iter()
            .filter(|s| !s.messages.is_empty())
            .collect())
    }
}
