//! Pi coding agent (pi.dev): `~/.pi/agent/sessions/--<encoded-cwd>--/<timestamp>_<id>.jsonl`
//! (`$PI_CODING_AGENT_DIR` replaces `~/.pi/agent`; `$PI_CODING_AGENT_SESSION_DIR` is a flat store).
//! Records form a tree through `parentId`; the active conversation is the path from the last
//! record back to the root. Format reference: SpecStory CLI `pkg/providers/piagent/PI-FORMAT.md`
//! (Apache-2.0), checked against real pi 0.84 sessions.

use super::{Connector, mtime_ms, text_of, tool_call, ts_ms};
use crate::clean::{relayed_reply, user_text};
use crate::model::{Message, Role, Session};
use anyhow::{Result, bail};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub struct Pi;

impl Connector for Pi {
    fn name(&self) -> &'static str {
        "pi"
    }
    fn locations(&self) -> &'static [&'static str] {
        &[
            "~/.pi/agent/sessions/*/*.jsonl",
            "$PI_CODING_AGENT_DIR/sessions/…",
            "$PI_CODING_AGENT_SESSION_DIR/*.jsonl",
        ]
    }
    fn discover(&self, home: &Path) -> Vec<PathBuf> {
        if let Ok(d) = std::env::var("PI_CODING_AGENT_SESSION_DIR") {
            return super::files_with_ext(Path::new(&d), "jsonl", 1);
        }
        let base = std::env::var("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".pi/agent"));
        super::files_with_ext(&base.join("sessions"), "jsonl", 2)
    }
    fn parse(&self, path: &Path) -> Result<Vec<Session>> {
        let text = std::fs::read_to_string(path)?;
        let mut records: Vec<Value> = text
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let Some(header) = records
            .first()
            .filter(|h| h.get("type").and_then(|v| v.as_str()) == Some("session"))
            .cloned()
        else {
            bail!("no session header");
        };
        records.remove(0);
        // Active branch: walk parents from the last record.
        let by_id: HashMap<String, usize> = records
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                r.get("id")
                    .and_then(|v| v.as_str())
                    .map(|id| (id.to_string(), i))
            })
            .collect();
        let mut path_idx = vec![];
        let mut seen = HashSet::new();
        let mut cur = records.len().checked_sub(1);
        while let Some(i) = cur {
            if !seen.insert(i) {
                break;
            }
            path_idx.push(i);
            cur = records[i]
                .get("parentId")
                .and_then(|v| v.as_str())
                .and_then(|p| by_id.get(p).copied());
        }
        path_idx.reverse();
        // Files without parent links (or a broken chain) fall back to file order.
        if path_idx.len() < records.len() / 2 && records.iter().all(|r| r.get("parentId").is_none())
        {
            path_idx = (0..records.len()).collect();
        }
        let mut s = Session {
            id: header
                .get("id")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| super::stem(path)),
            agent: "pi".into(),
            source: path.display().to_string(),
            project: header.get("cwd").and_then(|v| v.as_str()).map(String::from),
            title: None,
            model: None,
            started: header.get("timestamp").and_then(ts_ms),
            updated: None,
            messages: vec![],
        };
        for i in path_idx {
            let r = &records[i];
            let ts = r.get("timestamp").and_then(ts_ms);
            match r.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                "model_change" if s.model.is_none() => {
                    s.model = r.get("modelId").and_then(|v| v.as_str()).map(String::from);
                }
                "session_info" => {
                    if let Some(n) = r.get("name").and_then(|v| v.as_str()) {
                        s.title = Some(n.to_string());
                    }
                }
                "compaction" => {
                    if let Some(sum) = r.get("summary").and_then(|v| v.as_str()) {
                        let mut m = Message::new(Role::System, sum).at(ts);
                        m.is_summary = true;
                        s.messages.push(m);
                    }
                }
                "message" => {
                    let Some(msg) = r.get("message") else {
                        continue;
                    };
                    match msg.get("role").and_then(|v| v.as_str()).unwrap_or("") {
                        "user" => {
                            let raw = text_of(msg.get("content").unwrap_or(&Value::Null));
                            if let Some(c) = user_text(&raw) {
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
                            if s.model.is_none() {
                                s.model =
                                    msg.get("model").and_then(|v| v.as_str()).map(String::from);
                            }
                            for b in msg
                                .get("content")
                                .and_then(|v| v.as_array())
                                .into_iter()
                                .flatten()
                            {
                                match b.get("type").and_then(|v| v.as_str()) {
                                    Some("text") => {
                                        let t = b
                                            .get("text")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .trim();
                                        if !crate::clean::is_silent(t) {
                                            s.messages
                                                .push(Message::new(Role::Assistant, t).at(ts));
                                        }
                                    }
                                    Some("toolCall") => {
                                        let name = b
                                            .get("name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("tool");
                                        let id =
                                            b.get("id").and_then(|v| v.as_str()).map(String::from);
                                        let call = tool_call(
                                            name,
                                            b.get("arguments").unwrap_or(&Value::Null),
                                            id,
                                        );
                                        if let Some(reply) =
                                            call.command.as_deref().and_then(relayed_reply)
                                        {
                                            s.messages
                                                .push(Message::new(Role::Assistant, reply).at(ts));
                                        }
                                        let mut m = Message::new(Role::Tool, String::new()).at(ts);
                                        m.tool = Some(call);
                                        s.messages.push(m);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        "toolResult" => {
                            let mut m = Message::new(
                                Role::Tool,
                                crate::clean::tool_output(&text_of(
                                    msg.get("content").unwrap_or(&Value::Null),
                                )),
                            )
                            .at(ts);
                            m.result_of = msg
                                .get("toolCallId")
                                .and_then(|v| v.as_str())
                                .map(String::from);
                            m.is_error = msg.get("isError").and_then(|v| v.as_bool()) == Some(true);
                            s.messages.push(m);
                        }
                        "bashExecution" => {
                            let cmd = msg.get("command").and_then(|v| v.as_str()).unwrap_or("");
                            let mut m = Message::new(Role::Tool, String::new()).at(ts);
                            m.tool = Some(tool_call(
                                "bash",
                                &serde_json::json!({ "command": cmd }),
                                None,
                            ));
                            s.messages.push(m);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        s.finish(mtime_ms(path));
        Ok(vec![s])
    }
}
