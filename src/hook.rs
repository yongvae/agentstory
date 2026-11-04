//! Agent hooks. `agentstory hook nudge` is a Claude Code `UserPromptSubmit` hook (and works for any
//! harness that pipes `{session_id, prompt}` JSON on stdin): every N human prompts it injects a
//! reminder to write down what was learned, because long chat sessions never hit a natural end.

use anyhow::Result;
use serde_json::{Value, json};
use std::io::Read;

pub fn nudge(every: u64, space_hint: Option<&str>) -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let v: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let Some(sid) = v.get("session_id").and_then(|x| x.as_str()) else {
        return Ok(());
    };
    let prompt = v.get("prompt").and_then(|x| x.as_str()).unwrap_or("");
    if prompt.starts_with("[SYSTEM NOTIFICATION")
        || prompt.contains("<task-notification>")
        || prompt.trim().is_empty()
    {
        return Ok(());
    }
    let dir = dirs::state_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("agentstory/turns");
    std::fs::create_dir_all(&dir)?;
    let safe: String = sid
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let f = dir.join(safe);
    let n: u64 = std::fs::read_to_string(&f)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
        + 1;
    std::fs::write(&f, n.to_string())?;
    if every == 0 || n % every != 0 {
        return Ok(());
    }
    let place = space_hint.unwrap_or("the agent space (memory/, wiki/, mistakes/)");
    let msg = format!(
        "[memory check, turn {n}] After answering, take one step to review the last {every} turns: did the user state a fact, correct you, or make a decision, or did you work out something non-obvious, that is not yet in {place}? Corrections about how to do a task go to that task's skill or wiki page; mistakes go to `agentstory mistake add`; facts to `agentstory mem add`. Search first (`agentstory search`) to avoid duplicates. Ask before changing the user's profile or preferences. If nothing is new, do nothing and don't mention this reminder."
    );
    println!(
        "{}",
        json!({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": msg}})
    );
    Ok(())
}
