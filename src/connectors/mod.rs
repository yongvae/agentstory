//! One connector per agent. A connector finds its files under a home directory and
//! turns each into agent-neutral [`Session`]s.

use crate::model::{Session, ToolCall, ToolKind};
use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub mod claude;
pub mod codex;
pub mod cursor;
pub mod dsh;
pub mod gemini;
pub mod hermes;
pub mod opencode;
pub mod pi;
pub mod qwen;

pub trait Connector: Sync {
    /// Stable short name, used in ids and filters ("claude-code", "codex", …).
    fn name(&self) -> &'static str;
    /// Where it looks, for `agentstory agents`.
    fn locations(&self) -> &'static [&'static str];
    /// Files (or databases) that hold sessions.
    fn discover(&self, home: &Path) -> Vec<PathBuf>;
    /// Parse one discovered file into zero or more sessions.
    fn parse(&self, path: &Path) -> Result<Vec<Session>>;
}

pub fn all() -> Vec<Box<dyn Connector>> {
    vec![
        Box::new(claude::ClaudeCode),
        Box::new(cursor::CursorAgent),
        Box::new(codex::Codex),
        Box::new(hermes::Hermes),
        Box::new(gemini::Gemini),
        Box::new(qwen::Qwen),
        Box::new(opencode::OpenCode),
        Box::new(pi::Pi),
        Box::new(dsh::Dsh),
    ]
}

pub fn by_name(name: &str) -> Option<Box<dyn Connector>> {
    all().into_iter().find(|c| c.name() == name)
}

// ---------- helpers shared by connectors ----------

pub fn files_with_ext(dir: &Path, ext: &str, max_depth: usize) -> Vec<PathBuf> {
    if !dir.exists() {
        return vec![];
    }
    walkdir::WalkDir::new(dir)
        .max_depth(max_depth)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| x == ext))
        .map(|e| e.into_path())
        .collect()
}

pub fn mtime_ms(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// ISO-8601 string, unix seconds (float) or unix millis → unix millis.
pub fn ts_ms(v: &Value) -> Option<i64> {
    match v {
        Value::String(s) => chrono::DateTime::parse_from_rfc3339(s)
            .map(|d| d.timestamp_millis())
            .ok()
            .or_else(|| s.parse::<f64>().ok().map(num_ms)),
        Value::Number(n) => n.as_f64().map(num_ms),
        _ => None,
    }
}

fn num_ms(x: f64) -> i64 {
    if x > 1e12 {
        x as i64
    } else {
        (x * 1000.0) as i64
    }
}

pub fn stem(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn str_of<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(*k).and_then(|x| x.as_str()))
}

/// Text from a string or a list of `{type: "text"|"input_text"|"output_text", text}` blocks.
pub fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|b| match b {
                Value::String(s) => Some(s.as_str()),
                _ => b.get("text").and_then(|t| t.as_str()),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) => v
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

/// Files touched by an apply_patch style patch.
pub fn patch_files(patch: &str) -> Vec<String> {
    patch
        .lines()
        .filter_map(|l| {
            [
                "*** Add File: ",
                "*** Update File: ",
                "*** Delete File: ",
                "*** Move to: ",
            ]
            .iter()
            .find_map(|p| l.strip_prefix(p))
            .map(|f| f.trim().to_string())
        })
        .collect()
}

/// Build a ToolCall from a tool name and its JSON input, recognising common shapes across agents.
pub fn tool_call(name: &str, input: &Value, call_id: Option<String>) -> ToolCall {
    let lname = name.to_ascii_lowercase();
    let lname = lname.rsplit("__").next().unwrap_or(&lname).to_string();
    let command = match input.get("command").or_else(|| input.get("cmd")) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(a)) => {
            let parts: Vec<&str> = a.iter().filter_map(|x| x.as_str()).collect();
            // ["bash","-lc","…"] → the script itself
            if parts.len() >= 3 && (parts[1] == "-lc" || parts[1] == "-c") {
                Some(parts[2..].join(" "))
            } else {
                Some(parts.join(" "))
            }
        }
        _ => None,
    };
    let path = str_of(
        input,
        &[
            "file_path",
            "path",
            "filePath",
            "absolute_path",
            "target_file",
            "notebook_path",
        ],
    )
    .map(|s| s.to_string());
    let patch = str_of(input, &["patch", "input"])
        .map(patch_files)
        .unwrap_or_default();
    let kind = if command.is_some()
        || [
            "bash",
            "shell",
            "exec_command",
            "run_shell_command",
            "terminal",
            "execute_command",
            "run_terminal_cmd",
        ]
        .contains(&lname.as_str())
    {
        ToolKind::Shell
    } else if !patch.is_empty()
        || lname.contains("edit")
        || lname.contains("write")
        || lname.contains("replace")
        || lname.contains("patch")
        || lname == "delete"
        || lname == "create_file"
    {
        ToolKind::Edit
    } else if lname.contains("read") || lname == "view" || lname == "cat" {
        ToolKind::Read
    } else if lname.contains("grep")
        || lname.contains("glob")
        || lname.contains("search")
        || lname == "ls"
        || lname == "find"
    {
        ToolKind::Search
    } else if lname == "agent"
        || lname == "task"
        || lname.contains("subagent")
        || lname.contains("delegate")
    {
        ToolKind::Agent
    } else {
        ToolKind::Other
    };
    let mut files = patch;
    if kind == ToolKind::Edit
        && let Some(p) = &path
    {
        files.push(p.clone());
    }
    let summary = str_of(
        input,
        &[
            "description",
            "query",
            "pattern",
            "prompt",
            "url",
            "skill",
            "title",
        ],
    )
    .map(|s| crate::clean::one_line(s, 120))
    .or_else(|| path.clone());
    ToolCall {
        name: name.to_string(),
        kind,
        command,
        files,
        summary,
        call_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_tools() {
        assert_eq!(
            tool_call("Bash", &json!({"command":"ls"}), None).kind,
            ToolKind::Shell
        );
        let e = tool_call("Edit", &json!({"file_path":"/a/b.rs"}), None);
        assert_eq!(e.kind, ToolKind::Edit);
        assert_eq!(e.files, vec!["/a/b.rs"]);
        assert_eq!(
            tool_call("Read", &json!({"file_path":"/a"}), None).kind,
            ToolKind::Read
        );
        let p = tool_call(
            "apply_patch",
            &json!({"input":"*** Begin Patch\n*** Update File: x.rs\n*** Add File: y.rs\n"}),
            None,
        );
        assert_eq!(p.files, vec!["x.rs", "y.rs"]);
        let sh = tool_call(
            "shell",
            &json!({"command":["bash","-lc","git status"]}),
            None,
        );
        assert_eq!(sh.command.as_deref(), Some("git status"));
        assert_eq!(
            tool_call("mcp__x__Agent", &json!({"description":"d"}), None).kind,
            ToolKind::Agent
        );
    }

    #[test]
    fn parses_timestamps() {
        assert_eq!(ts_ms(&json!("2026-11-04T02:00:00Z")), Some(1793757600000));
        assert_eq!(ts_ms(&json!(1793757600.5)), Some(1793757600500));
        assert_eq!(ts_ms(&json!(1793757600500i64)), Some(1793757600500));
    }
}
