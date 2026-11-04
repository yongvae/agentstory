//! OpenAI Codex (CLI, IDE and desktop): `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` and
//! `~/.codex/archived_sessions/*.jsonl` (`$CODEX_HOME` overrides `~/.codex`).
//! Lines are `{timestamp, type, payload}`; conversation items are `type: "response_item"`.

use super::{Connector, mtime_ms, stem, text_of, tool_call, ts_ms};
use crate::clean::user_text;
use crate::model::{Message, Role, Session};
use anyhow::Result;
use regex::Regex;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub struct Codex;

/// User-role items Codex injects for context, not typed by a person.
const CONTEXT_PREFIXES: &[&str] = &[
    "# AGENTS.md instructions",
    "<environment_context>",
    "<user_instructions>",
    "<skill>",
    "<INSTRUCTIONS>",
    "<turn_aborted>",
];

static EXIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:Process exited with code|Exit code:)\s*(-?\d+)").unwrap());

fn output_is_error(out: &str) -> bool {
    if let Some(c) = EXIT.captures(out) {
        return &c[1] != "0";
    }
    if let Ok(v) = serde_json::from_str::<Value>(out)
        && let Some(code) = v.pointer("/metadata/exit_code").and_then(|x| x.as_i64())
    {
        return code != 0;
    }
    false
}

impl Connector for Codex {
    fn name(&self) -> &'static str {
        "codex"
    }
    fn locations(&self) -> &'static [&'static str] {
        &[
            "~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl",
            "~/.codex/archived_sessions/*.jsonl",
            "$CODEX_HOME/…",
        ]
    }
    fn discover(&self, home: &Path) -> Vec<PathBuf> {
        let base = std::env::var("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".codex"));
        let mut v = super::files_with_ext(&base.join("sessions"), "jsonl", 4);
        v.extend(super::files_with_ext(
            &base.join("archived_sessions"),
            "jsonl",
            2,
        ));
        v
    }
    fn parse(&self, path: &Path) -> Result<Vec<Session>> {
        let reader = BufReader::new(std::fs::File::open(path)?);
        let mut s = Session {
            id: stem(path),
            agent: "codex".into(),
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
            let p = o.get("payload").cloned().unwrap_or(Value::Null);
            match o.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                "session_meta" => {
                    if let Some(id) = p
                        .get("id")
                        .or_else(|| p.get("session_id"))
                        .and_then(|v| v.as_str())
                    {
                        s.id = id.to_string();
                    }
                    s.project = p.get("cwd").and_then(|v| v.as_str()).map(String::from);
                    s.started = p.get("timestamp").and_then(ts_ms);
                }
                "turn_context" => {
                    if s.model.is_none() {
                        s.model = p.get("model").and_then(|v| v.as_str()).map(String::from);
                    }
                }
                "compacted" => {
                    let text = p.get("message").map(text_of).unwrap_or_default();
                    if !text.is_empty() {
                        let mut m = Message::new(Role::System, text).at(ts);
                        m.is_summary = true;
                        s.messages.push(m);
                    }
                }
                "response_item" => match p.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                    "message" => {
                        let role = p.get("role").and_then(|v| v.as_str()).unwrap_or("");
                        let text = text_of(p.get("content").unwrap_or(&Value::Null));
                        match role {
                            "user" => {
                                let t = text.trim_start();
                                if CONTEXT_PREFIXES.iter().any(|x| t.starts_with(x)) {
                                    continue;
                                }
                                if let Some(c) = user_text(&text) {
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
                            "assistant" if !text.trim().is_empty() => {
                                s.messages
                                    .push(Message::new(Role::Assistant, text.trim()).at(ts));
                            }
                            _ => {}
                        }
                    }
                    "function_call" | "custom_tool_call" | "local_shell_call" => {
                        let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("shell");
                        let input = match (p.get("arguments"), p.get("input"), p.get("action")) {
                            (Some(Value::String(a)), _, _) => {
                                serde_json::from_str(a).unwrap_or(Value::String(a.clone()))
                            }
                            (_, Some(Value::String(i)), _) => serde_json::json!({ "input": i }),
                            (_, _, Some(a)) => a.clone(),
                            (Some(v), _, _) => v.clone(),
                            _ => Value::Null,
                        };
                        let call_id = p.get("call_id").and_then(|v| v.as_str()).map(String::from);
                        let mut m = Message::new(Role::Tool, String::new()).at(ts);
                        m.tool = Some(tool_call(name, &input, call_id));
                        s.messages.push(m);
                    }
                    "function_call_output"
                    | "custom_tool_call_output"
                    | "local_shell_call_output" => {
                        let out = match p.get("output") {
                            Some(Value::String(x)) => x.clone(),
                            Some(v) => v.to_string(),
                            None => String::new(),
                        };
                        let mut m =
                            Message::new(Role::Tool, crate::clean::tool_output(&out)).at(ts);
                        m.result_of = p.get("call_id").and_then(|v| v.as_str()).map(String::from);
                        m.is_error = output_is_error(&out);
                        s.messages.push(m);
                    }
                    _ => {}
                },
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
    fn detects_exit_codes() {
        assert!(super::output_is_error(
            "Chunk ID: 1\nProcess exited with code 1\nOutput:\nboom"
        ));
        assert!(!super::output_is_error(
            "Exit code: 0\nWall time: 0 seconds"
        ));
        assert!(super::output_is_error(
            r#"{"output":"x","metadata":{"exit_code":2}}"#
        ));
    }
}
