//! Agent-neutral session model. Every connector turns its native format into this.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// A human (or another agent speaking as the user).
    User,
    /// Text the agent wrote.
    Assistant,
    /// A tool call made by the agent, or its result.
    Tool,
    /// Harness-generated input: scheduled prompts, hook feedback, compaction summaries.
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Shell,
    Edit,
    Read,
    Search,
    Agent,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    pub kind: ToolKind,
    /// Shell command, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Files the call writes (edit/write/patch).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub files: Vec<String>,
    /// Short human summary of the input (description, query, path…).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    /// Unix milliseconds, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<i64>,
    /// Display name of the speaker when the transcript says who it is (e.g. a chat sender).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolCall>,
    /// For tool results: the call this answers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_of: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub is_error: bool,
    /// A compaction / continuation summary written by the harness.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub is_summary: bool,
}

impl Message {
    pub fn new(role: Role, text: impl Into<String>) -> Self {
        Message {
            role,
            ts: None,
            author: None,
            text: text.into(),
            tool: None,
            result_of: None,
            is_error: false,
            is_summary: false,
        }
    }
    pub fn at(mut self, ts: Option<i64>) -> Self {
        self.ts = ts;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Native session id (file stem / uuid / db key).
    pub id: String,
    /// Connector name, e.g. "claude-code".
    pub agent: String,
    /// File (or db) the session came from.
    pub source: String,
    /// Working directory / project, when recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub started: Option<i64>,
    pub updated: Option<i64>,
    pub messages: Vec<Message>,
}

impl Session {
    /// Fill missing timestamps by carrying the previous one forward, then derive started/updated.
    pub fn finish(&mut self, fallback_ms: i64) {
        // In a chat-bridge session (some messages name their sender), an unsigned "user"
        // message came from the harness: a scheduled prompt or an automation.
        if self
            .messages
            .iter()
            .any(|m| m.role == Role::User && m.author.is_some())
        {
            for m in &mut self.messages {
                if m.role == Role::User && m.author.is_none() {
                    m.role = Role::System;
                }
            }
        }
        let mut last = self.started;
        for m in &mut self.messages {
            match m.ts {
                Some(t) => last = Some(t),
                None => m.ts = last,
            }
        }
        let first = self.messages.iter().find_map(|m| m.ts);
        let latest = self.messages.iter().filter_map(|m| m.ts).max();
        self.started = self.started.or(first).or(Some(fallback_ms));
        self.updated = latest.or(self.updated).or(Some(fallback_ms));
        if self.title.is_none() {
            self.title = self
                .messages
                .iter()
                .find(|m| m.role == Role::User && !m.text.trim().is_empty())
                .map(|m| crate::clean::one_line(&m.text, 80));
        }
    }
}

/// A turn = one user (or system) input and everything the agent did until the next input.
#[derive(Debug, Clone, Serialize)]
pub struct Turn {
    pub index: usize,
    pub ts: Option<i64>,
    pub speaker: String,
    pub input: String,
    /// Assistant text in this turn.
    pub reply: String,
    pub is_system: bool,
    pub tool_calls: usize,
    /// Files written in this turn.
    pub files: Vec<String>,
    /// Shell commands run in this turn.
    pub commands: Vec<String>,
    pub errors: usize,
    /// Tool output in this turn, one entry per call; indexed, not shown.
    #[serde(skip)]
    pub outputs: Vec<String>,
}

impl Turn {
    fn synthetic(index: usize, ts: Option<i64>) -> Self {
        Turn {
            index,
            ts,
            speaker: "system".into(),
            input: String::new(),
            reply: String::new(),
            is_system: true,
            tool_calls: 0,
            files: vec![],
            commands: vec![],
            errors: 0,
            outputs: vec![],
        }
    }
}

pub fn turns(s: &Session) -> Vec<Turn> {
    let mut out: Vec<Turn> = Vec::new();
    for m in &s.messages {
        match m.role {
            Role::User | Role::System => {
                out.push(Turn {
                    index: out.len() + 1,
                    ts: m.ts,
                    speaker: match (m.role, &m.author, m.is_summary) {
                        (_, _, true) => "summary".into(),
                        (Role::System, _, _) => "system".into(),
                        (_, Some(a), _) => a.clone(),
                        _ => "user".into(),
                    },
                    input: m.text.clone(),
                    reply: String::new(),
                    is_system: m.role == Role::System,
                    tool_calls: 0,
                    files: vec![],
                    commands: vec![],
                    errors: 0,
                    outputs: vec![],
                });
            }
            Role::Assistant => {
                if out.is_empty() {
                    out.push(Turn::synthetic(1, m.ts));
                }
                let t = out.last_mut().unwrap();
                if !t.reply.is_empty() {
                    t.reply.push('\n');
                }
                t.reply.push_str(&m.text);
            }
            Role::Tool => {
                if out.is_empty() {
                    out.push(Turn::synthetic(1, m.ts));
                }
                let t = out.last_mut().unwrap();
                if m.is_error {
                    t.errors += 1;
                }
                if !m.text.trim().is_empty() {
                    t.outputs.push(m.text.trim().to_string());
                }
                if let Some(call) = &m.tool {
                    t.tool_calls += 1;
                    for f in &call.files {
                        if !t.files.contains(f) {
                            t.files.push(f.clone());
                        }
                    }
                    if call.kind == ToolKind::Shell
                        && let Some(c) = &call.command
                    {
                        t.commands.push(c.clone());
                    }
                }
            }
        }
    }
    out
}
