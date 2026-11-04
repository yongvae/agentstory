//! Qwen Code: `~/.qwen/projects/<sanitized-cwd>/chats/<session-id>.jsonl` (`$QWEN_HOME` overrides).
//! Records carry `type` (user / assistant / tool_result / system), `provenance`, and a Gemini-shaped
//! `message: {role, parts}`. Format reference: SpecStory CLI `pkg/providers/qwencode` (Apache-2.0).

use super::{Connector, mtime_ms, stem, tool_call, ts_ms};
use crate::clean::user_text;
use crate::model::{Message, Role, Session};
use anyhow::Result;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

pub struct Qwen;

fn parts(o: &Value) -> Vec<Value> {
    o.pointer("/message/parts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

impl Connector for Qwen {
    fn name(&self) -> &'static str {
        "qwen"
    }
    fn locations(&self) -> &'static [&'static str] {
        &["~/.qwen/projects/*/chats/*.jsonl", "$QWEN_HOME/projects/…"]
    }
    fn discover(&self, home: &Path) -> Vec<PathBuf> {
        let base = std::env::var("QWEN_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".qwen"));
        let Ok(rd) = std::fs::read_dir(base.join("projects")) else {
            return vec![];
        };
        rd.filter_map(|e| e.ok())
            .flat_map(|e| super::files_with_ext(&e.path().join("chats"), "jsonl", 1))
            .filter(|p| !p.to_string_lossy().ends_with(".ledger.jsonl"))
            .collect()
    }
    fn parse(&self, path: &Path) -> Result<Vec<Session>> {
        let reader = BufReader::new(std::fs::File::open(path)?);
        let mut s = Session {
            id: stem(path),
            agent: "qwen".into(),
            source: path.display().to_string(),
            project: None,
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
            let ts = o.get("timestamp").and_then(ts_ms);
            if s.project.is_none() {
                s.project = o.get("cwd").and_then(|v| v.as_str()).map(String::from);
            }
            let prov = o.get("provenance").and_then(|v| v.as_str()).unwrap_or("");
            match o.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                "user" => {
                    let text: Vec<String> = parts(&o)
                        .iter()
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()).map(String::from))
                        .collect();
                    if let Some(c) = user_text(&text.join("\n")) {
                        let system = c.is_system || (!prov.is_empty() && prov != "real_user");
                        let mut m =
                            Message::new(if system { Role::System } else { Role::User }, c.text)
                                .at(ts);
                        m.author = c.author;
                        s.messages.push(m);
                    }
                }
                "assistant" => {
                    if s.model.is_none() {
                        s.model = o.get("model").and_then(|v| v.as_str()).map(String::from);
                    }
                    for p in parts(&o) {
                        if p.get("thought").and_then(|v| v.as_bool()) == Some(true) {
                            continue;
                        }
                        if let Some(t) = p
                            .get("text")
                            .and_then(|v| v.as_str())
                            .filter(|t| !t.trim().is_empty())
                        {
                            s.messages
                                .push(Message::new(Role::Assistant, t.trim()).at(ts));
                        }
                        if let Some(fc) = p.get("functionCall") {
                            let name = fc.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                            let id = fc.get("id").and_then(|v| v.as_str()).map(String::from);
                            let mut m = Message::new(Role::Tool, String::new()).at(ts);
                            m.tool =
                                Some(tool_call(name, fc.get("args").unwrap_or(&Value::Null), id));
                            s.messages.push(m);
                        }
                    }
                }
                "tool_result" => {
                    let status = o.pointer("/toolCallResult/status").and_then(|v| v.as_str());
                    for p in parts(&o) {
                        let Some(fr) = p.get("functionResponse") else {
                            continue;
                        };
                        let resp = fr.get("response").cloned().unwrap_or(Value::Null);
                        let out = resp
                            .get("output")
                            .or_else(|| resp.get("error"))
                            .map(|v| {
                                v.as_str()
                                    .map(String::from)
                                    .unwrap_or_else(|| v.to_string())
                            })
                            .unwrap_or_default();
                        let mut m =
                            Message::new(Role::Tool, crate::clean::tool_output(&out)).at(ts);
                        m.result_of = fr
                            .get("id")
                            .and_then(|v| v.as_str())
                            .or_else(|| {
                                o.pointer("/toolCallResult/callId").and_then(|v| v.as_str())
                            })
                            .map(String::from);
                        m.is_error = matches!(status, Some("error" | "cancelled"))
                            || resp.get("error").is_some();
                        s.messages.push(m);
                    }
                }
                "system"
                    if o.get("subtype").and_then(|v| v.as_str()) == Some("chat_compression") =>
                {
                    let summary: Vec<String> = o
                        .pointer("/systemPayload/compressedHistory")
                        .and_then(|v| v.as_array())
                        .into_iter()
                        .flatten()
                        .flat_map(|m| {
                            m.get("parts")
                                .and_then(|p| p.as_array())
                                .cloned()
                                .unwrap_or_default()
                        })
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()).map(String::from))
                        .collect();
                    if !summary.is_empty() {
                        let mut m = Message::new(Role::System, summary.join("\n")).at(ts);
                        m.is_summary = true;
                        s.messages.push(m);
                    }
                }
                _ => {}
            }
        }
        s.finish(mtime_ms(path));
        Ok(vec![s])
    }
}
