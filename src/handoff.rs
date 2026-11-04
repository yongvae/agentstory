//! Handoff card: everything another agent needs to pick up a session, extracted mechanically
//! (no model call), so it is fast, deterministic and never invents progress.

use crate::clean::{one_line, truncate};
use crate::model::{Role, Session, turns};
use crate::time::fmt_local;
use regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;

#[derive(Debug, Serialize)]
pub struct Handoff {
    pub key: String,
    pub agent: String,
    pub id: String,
    pub project: Option<String>,
    pub model: Option<String>,
    pub started: Option<i64>,
    pub updated: Option<i64>,
    pub human_turns: usize,
    pub total_turns: usize,
    pub goal: Option<Said>,
    pub later_instructions: Vec<Said>,
    pub latest_summary: Option<String>,
    pub last_input: Option<Said>,
    pub last_reply: Option<String>,
    pub files_written: Vec<(String, usize)>,
    pub key_actions: Vec<Action>,
    pub errors: Vec<Action>,
    pub resume_command: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct Said {
    pub ts: Option<i64>,
    pub who: String,
    pub text: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct Action {
    pub ts: Option<i64>,
    pub kind: String,
    pub detail: String,
}

struct Watch(String, Regex);

/// Generic side effects; harness-specific ones (chat messages, docs…) come from the user's
/// config (`[handoff.actions]`).
static WATCHES: LazyLock<Vec<Watch>> = LazyLock::new(|| {
    let w = |k: &str, p: &str| Watch(k.to_string(), Regex::new(p).unwrap());
    let mut v = vec![
        w("commit", r#"git commit[^\n]*?-m\s+["']([^"'\n]{1,160})"#),
        w("push", r"(git push[^\n;&|]{0,80})"),
        w(
            "review",
            r"((?:gh(?:\s+-\S+(?:\s+[^\s-]\S*)?)*\s+pr|glab(?:\s+-\S+(?:\s+[^\s-]\S*)?)*\s+mr)\s+(?:create|merge|edit|comment|review|close)\b[^\n;&|]{0,120})",
        ),
        w("release", r"((?:cargo|npm|pnpm|yarn) publish[^\n]{0,80})"),
        w(
            "deploy",
            r"((?:kubectl apply|helm (?:install|upgrade)|docker push|launchctl (?:load|unload|bootstrap|bootout|kickstart)|systemctl (?:restart|start|stop))[^\n]{0,100})",
        ),
        w("schedule", r"(crontab\s+(?:-[er]\b|[^-\s])[^\n]{0,140})"),
        w("delete", r"(\brm -[a-zA-Z]*r[a-zA-Z]* [^\n;&|]{1,120})"),
    ];
    for (kind, pat) in &crate::config::CONFIG.handoff.actions {
        match Regex::new(pat) {
            Ok(re) if re.captures_len() > 1 => v.push(Watch(kind.clone(), re)),
            Ok(_) => eprintln!("agentstory: handoff action {kind:?} needs one capture group"),
            Err(e) => eprintln!("agentstory: ignoring handoff action {kind:?}: {e}"),
        }
    }
    v
});

/// Every side-effect action found in one shell command: (kind, display fragment).
fn side_effects(cmd: &str) -> Vec<(String, String)> {
    WATCHES
        .iter()
        .flat_map(|Watch(kind, re)| {
            re.captures_iter(cmd)
                .map(move |c| (kind.clone(), one_line(&c[1], 160)))
        })
        // `tool create --help` is a lookup, not an action.
        .filter(|(_, d)| !d.contains("--help") && !d.split_whitespace().any(|w| w == "-h"))
        .collect()
}

pub fn resume_command(agent: &str, id: &str) -> Option<String> {
    match agent {
        "claude-code" => Some(format!("claude --resume {id}")),
        "codex" => Some(format!("codex resume {id}")),
        "opencode" => Some(format!("opencode -s {id}")),
        "cursor" => Some(format!("cursor-agent --resume={id}")),
        _ => None,
    }
}

pub fn build(s: &Session) -> Handoff {
    let ts = turns(s);
    let human: Vec<_> = ts
        .iter()
        .filter(|t| !t.is_system && !t.input.trim().is_empty())
        .collect();
    let said = |t: &crate::model::Turn, n: usize| Said {
        ts: t.ts,
        who: t.speaker.clone(),
        text: truncate(t.input.trim(), n),
    };
    let goal = human.first().map(|t| said(t, 1200));
    let later: Vec<Said> = human
        .iter()
        .skip(1)
        .rev()
        .take(15)
        .rev()
        .map(|t| said(t, 400))
        .collect();
    let latest_summary = s
        .messages
        .iter()
        .rev()
        .find(|m| m.is_summary)
        .map(|m| truncate(m.text.trim(), 2500));
    let last_input = ts
        .iter()
        .rev()
        .find(|t| !t.input.trim().is_empty())
        .map(|t| said(t, 600));
    let last_reply = ts
        .iter()
        .rev()
        .find(|t| !t.reply.trim().is_empty())
        .map(|t| truncate(t.reply.trim(), 1500));

    let mut files: Vec<(String, usize)> = vec![];
    let mut actions = vec![];
    let mut errors = vec![];
    let mut calls: std::collections::HashMap<String, String> = Default::default();
    for m in &s.messages {
        if let Some(t) = &m.tool {
            for f in &t.files {
                match files.iter_mut().find(|(p, _)| p == f) {
                    Some(e) => e.1 += 1,
                    None => files.push((f.clone(), 1)),
                }
            }
            let label = t
                .command
                .clone()
                .or_else(|| t.summary.clone())
                .unwrap_or_else(|| t.name.clone());
            if let Some(id) = &t.call_id {
                calls.insert(id.clone(), format!("{}: {}", t.name, one_line(&label, 80)));
            }
            if let Some(cmd) = &t.command {
                for (kind, detail) in side_effects(cmd) {
                    actions.push(Action {
                        ts: m.ts,
                        kind,
                        detail,
                    });
                }
            }
            if t.kind == crate::model::ToolKind::Agent {
                actions.push(Action {
                    ts: m.ts,
                    kind: "subagent".into(),
                    detail: one_line(&label, 120),
                });
            }
        }
        if m.is_error && m.role == Role::Tool {
            let what = m
                .result_of
                .as_ref()
                .and_then(|id| calls.get(id))
                .cloned()
                .unwrap_or_default();
            let detail = if what.is_empty() {
                one_line(&m.text, 200)
            } else {
                format!("{what} → {}", one_line(&m.text, 160))
            };
            if !errors.iter().any(|e: &Action| e.detail == detail) {
                errors.push(Action {
                    ts: m.ts,
                    kind: "error".into(),
                    detail,
                });
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    actions.retain(|a| seen.insert((a.kind.clone(), a.detail.clone())));
    files.sort_by_key(|f| std::cmp::Reverse(f.1));
    let n_err = errors.len();
    Handoff {
        key: format!("{}:{}", s.agent, s.id),
        agent: s.agent.clone(),
        id: s.id.clone(),
        project: s.project.clone(),
        model: s.model.clone(),
        started: s.started,
        updated: s.updated,
        human_turns: human.len(),
        total_turns: ts.len(),
        goal,
        later_instructions: later,
        latest_summary,
        last_input,
        last_reply,
        files_written: files.into_iter().take(40).collect(),
        key_actions: actions,
        errors: errors.into_iter().skip(n_err.saturating_sub(8)).collect(),
        resume_command: resume_command(&s.agent, &s.id),
    }
}

fn quote(s: &str) -> String {
    s.lines()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn markdown(h: &Handoff) -> String {
    let mut o = String::new();
    let short = crate::index::short_id(&h.id);
    o.push_str(&format!("# Handoff · {} {}\n\n", h.agent, short));
    o.push_str(&format!(
        "- Session: `{}`{}\n- Time: {} → {}, {} human turns / {} total\n",
        h.key,
        h.project
            .as_ref()
            .map(|p| format!(", project `{p}`"))
            .unwrap_or_default(),
        fmt_local(h.started),
        fmt_local(h.updated),
        h.human_turns,
        h.total_turns
    ));
    if let Some(m) = &h.model {
        o.push_str(&format!("- Model: {m}\n"));
    }
    o.push_str(&format!(
        "- Read the source: `agentstory show {short} --last 3` or `agentstory show {short} --around <word>`\n"
    ));
    if let Some(r) = &h.resume_command {
        o.push_str(&format!("- Native resume (same agent only): `{r}`\n"));
    }
    o.push_str(
        "- Extracted mechanically, no model summary. When in doubt, the transcript wins.\n\n",
    );
    if let Some(g) = &h.goal {
        o.push_str(&format!(
            "## Goal (first request, {} {})\n\n{}\n\n",
            fmt_local(g.ts),
            g.who,
            quote(&g.text)
        ));
    }
    if !h.later_instructions.is_empty() {
        o.push_str("## What they said since (oldest first)\n\n");
        for s in &h.later_instructions {
            o.push_str(&format!(
                "- {} {}: {}\n",
                fmt_local(s.ts),
                s.who,
                one_line(&s.text, 260)
            ));
        }
        o.push('\n');
    }
    if let Some(sum) = &h.latest_summary {
        o.push_str(&format!(
            "## Latest compaction summary (excerpt)\n\n{}\n\n",
            quote(sum)
        ));
    }
    o.push_str("## Where it stopped\n\n");
    if let Some(l) = &h.last_input {
        o.push_str(&format!(
            "Last input ({} {}): {}\n\n",
            fmt_local(l.ts),
            l.who,
            one_line(&l.text, 300)
        ));
    }
    match &h.last_reply {
        Some(r) => o.push_str(&format!("Agent's last words:\n\n{}\n\n", quote(r))),
        None => o.push_str("The agent never replied. It may have died mid-task: read the last turns and the errors below first.\n\n"),
    }
    if !h.files_written.is_empty() {
        o.push_str("## Files written\n\n");
        for (f, n) in &h.files_written {
            o.push_str(&format!("- `{f}` ({n}×)\n"));
        }
        o.push('\n');
    }
    if !h.key_actions.is_empty() {
        o.push_str("## Actions with side effects\n\n");
        for a in &h.key_actions {
            o.push_str(&format!("- {} {}: {}\n", fmt_local(a.ts), a.kind, a.detail));
        }
        o.push('\n');
    }
    if !h.errors.is_empty() {
        o.push_str("## Errors (latest)\n\n");
        for a in &h.errors {
            o.push_str(&format!("- {} {}\n", fmt_local(a.ts), a.detail));
        }
        o.push('\n');
    }
    o.push_str(
        "## Before you continue\n\n\
         1. Read this card, then the last 3 turns of the transcript, and confirm what is actually done.\n\
         2. Check current state (git log, running jobs, schedules) instead of trusting the card.\n\
         3. Do not repeat side effects listed above (messages, posts, pushes, payments, deploys).\n\
         4. If the latest request is ambiguous, ask before acting.\n",
    );
    o
}

#[cfg(test)]
mod tests {
    use super::side_effects;

    fn kinds(cmd: &str) -> Vec<String> {
        side_effects(cmd).into_iter().map(|(k, _)| k).collect()
    }

    #[test]
    fn reviews_are_side_effects() {
        for cmd in [
            "glab mr create -t x",
            "glab mr merge 3",
            "gh pr create --title x",
            "gh -R o/r pr merge 5 --squash",
            "gh pr comment 5 --body x",
            "gh pr review 5 --approve",
        ] {
            assert_eq!(kinds(cmd), ["review"], "{cmd}");
        }
    }

    #[test]
    fn read_only_commands_are_not_side_effects() {
        for cmd in [
            "gh pr view 5",
            "gh pr list",
            "gh pr checks 5",
            "glab mr diff 3",
            "gh pr merge -h",
            "gh pr create --help",
            "git status",
            "crontab -l",
        ] {
            let k = kinds(cmd);
            assert!(k.is_empty(), "{cmd} should not match: {k:?}");
        }
    }

    #[test]
    fn chained_actions_are_each_captured() {
        let mut got = kinds("git commit -m 'fix' && git push origin main && npm publish");
        got.sort_unstable();
        assert_eq!(got, ["commit", "push", "release"]);
    }

    #[test]
    fn only_first_line_of_a_heredoc_command_is_shown() {
        let got = side_effects("gh pr create --body-file - <<'EOF'\nlong body\nEOF");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, "gh pr create --body-file - <<'EOF'");
    }
}
