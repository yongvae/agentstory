//! Mining sessions for material the memory system needs: the user's corrections (raw material
//! for the mistakes log and preferences) and a per-day digest (input for nightly distillation).

use crate::clean::{one_line, truncate};
use crate::index::{Filter, Index};
use crate::model::turns;
use crate::time::fmt_local;
use anyhow::Result;
use chrono::{Local, NaiveDate, TimeZone};
use regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;

/// Phrases people use when correcting an agent. Weighted: strong ones alone are enough.
static STRONG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)(不要|不用|别再|别[^的人名处样些说]|不对|错了|搞错|不是这样|不是我要|以后|下次|记住|不应该|为什么不|为什么要|我说了|说过了|重新来|返工|太激进|太长|太短|太啰嗦|不需要|没让你|谁让你|",
        r"\bdon'?t\b|\bdo not\b|\bstop\b|\bwrong\b|\binstead\b|\bnever\b|\balways\b|\bremember\b|\bnext time\b|",
        r"why did you|i (?:said|told you)|not what i|that'?s not|you should(?:n'?t)?)"
    ))
    .unwrap()
});

#[derive(Debug, Serialize)]
pub struct Correction {
    pub key: String,
    pub agent: String,
    pub project: Option<String>,
    pub turn: usize,
    pub ts: Option<i64>,
    pub who: String,
    /// What the user said.
    pub said: String,
    /// The agent's previous reply the user reacted to.
    pub after: String,
    pub markers: Vec<String>,
}

pub fn corrections(idx: &Index, f: &Filter, limit: usize) -> Result<Vec<Correction>> {
    let mut out = vec![];
    for row in idx.sessions(f, None, 100_000)? {
        let s = idx.load(&row.key)?;
        let ts = turns(&s);
        for (i, t) in ts.iter().enumerate() {
            if t.is_system || i == 0 || t.input.chars().count() > 1200 {
                continue;
            }
            if let (Some(since), Some(at)) = (f.since, t.ts) {
                if at < since {
                    continue;
                }
            }
            let markers: Vec<String> = STRONG
                .find_iter(&t.input)
                .map(|m| m.as_str().trim().to_lowercase())
                .collect();
            if markers.is_empty() {
                continue;
            }
            let prev = ts[..i]
                .iter()
                .rev()
                .find(|p| !p.reply.trim().is_empty())
                .map(|p| p.reply.as_str())
                .unwrap_or("");
            if prev.is_empty() {
                continue;
            }
            let mut m = markers;
            m.dedup();
            out.push(Correction {
                key: row.key.clone(),
                agent: s.agent.clone(),
                project: s.project.clone(),
                turn: t.index,
                ts: t.ts,
                who: t.speaker.clone(),
                said: truncate(t.input.trim(), 600),
                after: truncate(prev.trim(), 400),
                markers: m,
            });
        }
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.ts));
    // The same text said again and again is a template, not a correction.
    let mut seen = std::collections::HashMap::<String, usize>::new();
    for c in &out {
        *seen.entry(c.said.clone()).or_default() += 1;
    }
    out.retain(|c| seen[&c.said] < 3);
    out.truncate(limit);
    Ok(out)
}

pub fn corrections_markdown(cs: &[Correction]) -> String {
    let mut o = String::from(
        "# Corrections to review\n\nEach item: what the agent had said, then the user's reaction. Turn real lessons into `agentstory mistake add` or a skill/wiki fix; ignore false positives.\n\n",
    );
    for c in cs {
        let short = c
            .key
            .split_once(':')
            .map(|(_, id)| crate::index::short_id(id))
            .unwrap_or(&c.key);
        o.push_str(&format!(
            "## {} · {} · {} (turn {}, `agentstory show {short} --around …`)\n\n> agent: {}\n\n**{}:** {}\n\n_markers: {}_\n\n",
            fmt_local(c.ts),
            c.agent,
            c.project.as_deref().unwrap_or(""),
            c.turn,
            one_line(&c.after, 300),
            c.who,
            c.said,
            c.markers.join(", ")
        ));
    }
    o
}

/// Everything said on one local day, grouped by session, redacted. Input for nightly distillation.
pub fn digest(idx: &Index, day: &str, agents: &[String]) -> Result<String> {
    let d = NaiveDate::parse_from_str(day, "%Y-%m-%d")?;
    let lo = Local
        .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .map(|t| t.timestamp_millis())
        .unwrap_or(0);
    let hi = lo + 86_400_000;
    let f = Filter {
        agents: agents.to_vec(),
        since: Some(lo),
        ..Default::default()
    };
    let mut o = format!("# Digest {day}\n\n");
    let mut n = 0;
    let mut rows = idx.sessions(&f, None, 100_000)?;
    rows.sort_by_key(|r| r.started);
    for row in rows {
        let s = idx.load(&row.key)?;
        let picked: Vec<_> = turns(&s)
            .into_iter()
            .filter(|t| t.ts.is_some_and(|x| x >= lo && x < hi))
            .collect();
        if picked.is_empty() {
            continue;
        }
        n += 1;
        o.push_str(&format!(
            "## {} · {} · {}\n\n",
            s.agent,
            crate::index::short_id(&s.id),
            s.project.as_deref().unwrap_or("")
        ));
        for t in picked {
            o.push_str(&format!(
                "### {} {}\n\n{}\n\n",
                fmt_local(t.ts).get(11..).unwrap_or(""),
                t.speaker,
                truncate(t.input.trim(), 1500)
            ));
            if !t.reply.trim().is_empty() {
                o.push_str(&format!(
                    "**Agent:** {}\n\n",
                    truncate(t.reply.trim(), 1800)
                ));
            }
            if !t.files.is_empty() {
                o.push_str(&format!("_wrote: {}_\n\n", t.files.join(", ")));
            }
        }
    }
    if n == 0 {
        o.push_str("(no sessions that day)\n");
    }
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::STRONG;
    #[test]
    fn spots_corrections_not_ordinary_words() {
        assert!(STRONG.is_match("不要那么激进，就改一下倾向就行"));
        assert!(STRONG.is_match("别推 main"));
        assert!(STRONG.is_match("Don't push to main"));
        assert!(!STRONG.is_match("看看别的方案"));
        assert!(!STRONG.is_match("帮我查一下天气"));
    }
}
