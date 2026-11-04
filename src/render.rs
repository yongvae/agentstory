//! Reading a session back as text: whole, last N turns, or around a keyword.

use crate::clean::one_line;
use crate::model::{Session, Turn, turns};
use crate::time::fmt_local;

pub struct View {
    pub last: Option<usize>,
    pub around: Option<String>,
    pub context: usize,
    pub tools: bool,
}

pub fn select<'a>(all: &'a [Turn], v: &View) -> Vec<&'a Turn> {
    if let Some(q) = &v.around {
        let q = q.to_lowercase();
        let hits: Vec<usize> = all
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.input.to_lowercase().contains(&q)
                    || t.reply.to_lowercase().contains(&q)
                    || t.commands.iter().any(|c| c.to_lowercase().contains(&q))
                    || t.files.iter().any(|f| f.to_lowercase().contains(&q))
            })
            .map(|(i, _)| i)
            .collect();
        let mut keep: Vec<usize> = hits
            .iter()
            .flat_map(|&i| {
                i.saturating_sub(v.context)..=(i + v.context).min(all.len().saturating_sub(1))
            })
            .collect();
        keep.sort();
        keep.dedup();
        return keep.into_iter().map(|i| &all[i]).collect();
    }
    let skip = v.last.map(|n| all.len().saturating_sub(n)).unwrap_or(0);
    all.iter().skip(skip).collect()
}

pub fn markdown(s: &Session, v: &View) -> String {
    let all = turns(s);
    let picked = select(&all, v);
    let mut o = format!(
        "# {} · {}\n\n- id: `{}`\n- project: {}\n- {} → {}, {} turns\n\n",
        s.agent,
        s.title
            .as_deref()
            .map(|t| one_line(t, 80))
            .unwrap_or_default(),
        s.id,
        s.project.as_deref().unwrap_or("?"),
        fmt_local(s.started),
        fmt_local(s.updated),
        all.len()
    );
    if picked.is_empty() {
        o.push_str("(nothing matched)\n");
        return o;
    }
    let mut prev: Option<usize> = None;
    for t in picked {
        if let Some(p) = prev
            && t.index != p + 1
        {
            o.push_str("\n… (skipped) …\n\n");
        }
        o.push_str(&format!(
            "## Turn {} · {} · {}\n\n",
            t.index,
            fmt_local(t.ts),
            t.speaker
        ));
        if !t.input.trim().is_empty() {
            o.push_str(t.input.trim());
            o.push_str("\n\n");
        }
        if !t.reply.trim().is_empty() {
            o.push_str("**Agent:**\n\n");
            o.push_str(t.reply.trim());
            o.push_str("\n\n");
        }
        if v.tools || t.tool_calls > 0 {
            let mut bits = vec![];
            if t.tool_calls > 0 {
                bits.push(format!("{} tool calls", t.tool_calls));
            }
            if !t.files.is_empty() {
                bits.push(format!(
                    "wrote {}",
                    t.files
                        .iter()
                        .map(|f| format!("`{f}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if t.errors > 0 {
                bits.push(format!("{} errors", t.errors));
            }
            if !bits.is_empty() {
                o.push_str(&format!("_{}_\n\n", bits.join(" · ")));
            }
            if v.tools {
                for c in &t.commands {
                    o.push_str(&format!("    $ {}\n", one_line(c, 200)));
                }
                if !t.commands.is_empty() {
                    o.push('\n');
                }
            }
        }
        prev = Some(t.index);
    }
    o
}
