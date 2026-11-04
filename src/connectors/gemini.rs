//! Gemini CLI: `~/.gemini/tmp/<project-dir>/chats/session-*.json` (one JSON document per session;
//! the project's real path is in `<project-dir>/.project_root`).
//! Format reference: SpecStory CLI `pkg/providers/geminicli` (Apache-2.0).

use super::{Connector, mtime_ms, text_of, tool_call, ts_ms};
use crate::clean::user_text;
use crate::model::{Message, Role, Session};
use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct Gemini;

impl Connector for Gemini {
    fn name(&self) -> &'static str {
        "gemini"
    }
    fn locations(&self) -> &'static [&'static str] {
        &["~/.gemini/tmp/*/chats/session-*.json"]
    }
    fn discover(&self, home: &Path) -> Vec<PathBuf> {
        let Ok(rd) = std::fs::read_dir(home.join(".gemini/tmp")) else {
            return vec![];
        };
        rd.filter_map(|e| e.ok())
            .flat_map(|e| super::files_with_ext(&e.path().join("chats"), "json", 1))
            .collect()
    }
    fn parse(&self, path: &Path) -> Result<Vec<Session>> {
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        let project = path
            .parent()
            .and_then(|p| p.parent())
            .and_then(|d| std::fs::read_to_string(d.join(".project_root")).ok())
            .map(|s| s.trim().to_string());
        let mut s = Session {
            id: doc
                .get("sessionId")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| super::stem(path)),
            agent: "gemini".into(),
            source: path.display().to_string(),
            project,
            title: None,
            model: None,
            started: doc.get("startTime").and_then(ts_ms),
            updated: doc.get("lastUpdated").and_then(ts_ms),
            messages: vec![],
        };
        for m in doc
            .get("messages")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            let ts = m.get("timestamp").and_then(ts_ms);
            let text = text_of(m.get("content").unwrap_or(&Value::Null));
            match m.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                "user" => {
                    if let Some(c) = user_text(&text) {
                        let mut msg = Message::new(
                            if c.is_system {
                                Role::System
                            } else {
                                Role::User
                            },
                            c.text,
                        )
                        .at(ts);
                        msg.author = c.author;
                        s.messages.push(msg);
                    }
                }
                "gemini" | "model" | "assistant" => {
                    if s.model.is_none() {
                        s.model = m.get("model").and_then(|v| v.as_str()).map(String::from);
                    }
                    if !text.trim().is_empty() {
                        s.messages
                            .push(Message::new(Role::Assistant, text.trim()).at(ts));
                    }
                    for call in m
                        .get("toolCalls")
                        .and_then(|v| v.as_array())
                        .into_iter()
                        .flatten()
                    {
                        let name = call.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                        let id = call.get("id").and_then(|v| v.as_str()).map(String::from);
                        let cts = call.get("timestamp").and_then(ts_ms).or(ts);
                        let mut tm = Message::new(Role::Tool, String::new()).at(cts);
                        tm.tool = Some(tool_call(
                            name,
                            call.get("args").unwrap_or(&Value::Null),
                            id.clone(),
                        ));
                        s.messages.push(tm);
                        let resp = call
                            .pointer("/result/0/functionResponse/response")
                            .cloned()
                            .unwrap_or(Value::Null);
                        let out = resp
                            .get("output")
                            .or_else(|| resp.get("error"))
                            .map(|v| {
                                v.as_str()
                                    .map(String::from)
                                    .unwrap_or_else(|| v.to_string())
                            })
                            .unwrap_or_default();
                        let mut rm =
                            Message::new(Role::Tool, crate::clean::tool_output(&out)).at(cts);
                        rm.result_of = id;
                        rm.is_error = matches!(
                            call.get("status").and_then(|v| v.as_str()),
                            Some("error" | "cancelled")
                        ) || resp.get("error").is_some_and(|e| !e.is_null());
                        s.messages.push(rm);
                    }
                }
                "error" => {
                    let mut em = Message::new(Role::System, text).at(ts);
                    em.is_error = true;
                    s.messages.push(em);
                }
                _ => {}
            }
        }
        s.finish(mtime_ms(path));
        Ok(vec![s])
    }
}
