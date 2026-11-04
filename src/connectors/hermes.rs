//! Hermes Agent (Nous Research): SQLite `~/.hermes/state.db` (`$HERMES_HOME` overrides),
//! tables `sessions` and `messages`. Profiles live under `~/.hermes/profiles/<name>/state.db`.

use super::{Connector, mtime_ms, tool_call};
use crate::clean::user_text;
use crate::model::{Message, Role, Session};
use anyhow::Result;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Hermes;

fn sec_ms(x: Option<f64>) -> Option<i64> {
    x.map(|v| (v * 1000.0) as i64)
}

fn tool_error(content: &str) -> bool {
    match serde_json::from_str::<Value>(content) {
        Ok(v) => {
            v.get("success").and_then(|x| x.as_bool()) == Some(false)
                || matches!(
                    v.get("status").and_then(|x| x.as_str()),
                    Some("error" | "failed")
                )
                || v.get("error").is_some_and(|e| !e.is_null() && e != "")
                || v.get("exit_code")
                    .and_then(|x| x.as_i64())
                    .is_some_and(|c| c != 0)
        }
        Err(_) => content.starts_with("Error"),
    }
}

impl Connector for Hermes {
    fn name(&self) -> &'static str {
        "hermes"
    }
    fn locations(&self) -> &'static [&'static str] {
        &[
            "~/.hermes/state.db",
            "~/.hermes/profiles/*/state.db",
            "$HERMES_HOME/state.db",
        ]
    }
    fn discover(&self, home: &Path) -> Vec<PathBuf> {
        let base = std::env::var("HERMES_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".hermes"));
        let mut v: Vec<PathBuf> = vec![base.join("state.db")];
        if let Ok(rd) = std::fs::read_dir(base.join("profiles")) {
            v.extend(rd.filter_map(|e| e.ok()).map(|e| e.path().join("state.db")));
        }
        v.into_iter().filter(|p| p.is_file()).collect()
    }
    fn parse(&self, path: &Path) -> Result<Vec<Session>> {
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let cols: Vec<String> = db
            .prepare("PRAGMA table_info(sessions)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        let has = |c: &str| cols.iter().any(|x| x == c);
        let title = if has("title") {
            "COALESCE(title, display_name)"
        } else {
            "display_name"
        };
        let cwd = if has("cwd") { "cwd" } else { "NULL" };
        let sql = format!("SELECT id, source, {title}, model, started_at, {cwd} FROM sessions");
        let mut sessions: Vec<Session> = vec![];
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut stmt = db.prepare(&sql)?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<f64>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        for row in rows.flatten() {
            let (id, source, title, model, started, cwd) = row;
            index.insert(id.clone(), sessions.len());
            sessions.push(Session {
                id,
                agent: "hermes".into(),
                source: format!("{}#{}", path.display(), source.unwrap_or_default()),
                project: cwd,
                title: title.filter(|t| !t.trim().is_empty()),
                model,
                started: sec_ms(started),
                updated: None,
                messages: vec![],
            });
        }
        let mcols: Vec<String> = db
            .prepare("PRAGMA table_info(messages)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        let active = if mcols.iter().any(|c| c == "active") {
            "WHERE COALESCE(active, 1) != 0"
        } else {
            ""
        };
        let mut stmt = db.prepare(&format!(
            "SELECT session_id, role, content, tool_calls, tool_call_id, timestamp FROM messages {active} ORDER BY session_id, timestamp, id"
        ))?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<f64>>(5)?,
            ))
        })?;
        for row in rows.flatten() {
            let (sid, role, content, calls, call_id, ts) = row;
            let Some(&i) = index.get(&sid) else { continue };
            let ts = sec_ms(ts);
            let content = content.unwrap_or_default();
            let s = &mut sessions[i];
            match role.as_str() {
                "user" => {
                    if let Some(c) = user_text(&content) {
                        let mut m = Message::new(
                            if c.is_system || c.is_summary {
                                Role::System
                            } else {
                                Role::User
                            },
                            c.text,
                        )
                        .at(ts);
                        m.author = c.author;
                        m.is_summary = c.is_summary;
                        s.messages.push(m);
                    }
                }
                "assistant" => {
                    if !content.trim().is_empty() {
                        s.messages
                            .push(Message::new(Role::Assistant, content.trim()).at(ts));
                    }
                    if let Some(Ok(Value::Array(list))) =
                        calls.as_deref().map(serde_json::from_str::<Value>)
                    {
                        for c in list {
                            let f = c.get("function").cloned().unwrap_or(Value::Null);
                            let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                            let args = match f.get("arguments") {
                                Some(Value::String(a)) => {
                                    serde_json::from_str(a).unwrap_or(Value::Null)
                                }
                                Some(v) => v.clone(),
                                None => Value::Null,
                            };
                            let id = c
                                .get("id")
                                .or_else(|| c.get("call_id"))
                                .and_then(|v| v.as_str())
                                .map(String::from);
                            let mut m = Message::new(Role::Tool, String::new()).at(ts);
                            m.tool = Some(tool_call(name, &args, id));
                            s.messages.push(m);
                        }
                    }
                }
                "tool" => {
                    let mut m =
                        Message::new(Role::Tool, crate::clean::tool_output(&content)).at(ts);
                    m.result_of = call_id;
                    m.is_error = tool_error(&content);
                    s.messages.push(m);
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
