//! Agent space: a plain-Markdown workspace that any agent can read and write.
//!
//! ```text
//! AGENTS.md                 rules every agent reads (CLAUDE.md / GEMINI.md / QWEN.md link here)
//! memory/USER.md            who the user is, preferences          (always loaded, capped)
//! memory/NOW.md             current priorities, open loops        (always loaded, capped)
//! memory/MEMORY.md          generated index of entries            (always loaded)
//! memory/<type>/<slug>.md   one fact per file: feedback | project | decision | reference
//! memory/_proposals/        changes waiting for the user's approval
//! wiki/                     long-lived knowledge, index.md lists pages
//! mistakes/                 error log: what went wrong, why, how to avoid it
//! journal/YYYY-MM-DD.md     daily notes (search only, never loaded whole)
//! .agentstory.toml                marks the space
//! ```

use crate::clean::{one_line, redact};
use anyhow::{Context, Result, bail};
use chrono::Local;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

pub const MARKER: &str = ".agentstory.toml";
pub const TYPES: &[&str] = &["feedback", "project", "decision", "reference"];
pub const USER_CAP: usize = 3000;
pub const NOW_CAP: usize = 2000;
pub const ALWAYS_LOADED_CAP: usize = 8000;
pub const ENTRY_CAP: usize = 2000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Zh,
}

impl Lang {
    pub fn parse(s: &str) -> Result<Lang> {
        match s {
            "en" => Ok(Lang::En),
            "zh" | "cn" | "zh-CN" => Ok(Lang::Zh),
            _ => bail!("--lang must be en or zh"),
        }
    }
    fn pick<'a>(self, en: &'a str, zh: &'a str) -> &'a str {
        match self {
            Lang::En => en,
            Lang::Zh => zh,
        }
    }
}

/// Walk up from `start` to the nearest directory holding the marker.
pub fn find(start: &Path) -> Option<PathBuf> {
    let start = start.canonicalize().ok()?;
    start
        .ancestors()
        .find(|d| d.join(MARKER).is_file())
        .map(Path::to_path_buf)
}

pub fn locate(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if p.join(MARKER).is_file() {
            return Ok(p.to_path_buf());
        }
        bail!(
            "{} is not an agent space (no {MARKER}); run `agentstory init {}`",
            p.display(),
            p.display()
        );
    }
    if let Ok(p) = std::env::var("AGENTSTORY_SPACE") {
        return locate(Some(Path::new(&p)));
    }
    let cwd = std::env::current_dir()?;
    find(&cwd).with_context(|| {
        format!(
            "no agent space here or above {} (run `agentstory init`, or pass --space)",
            cwd.display()
        )
    })
}

pub fn today() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.trim().chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let out: String = out.trim_matches('-').chars().take(60).collect();
    if out.is_empty() {
        "untitled".into()
    } else {
        out
    }
}

// ---------------------------------------------------------------- init

#[derive(Debug, Default, Serialize)]
pub struct InitReport {
    pub space: String,
    pub created: Vec<String>,
    pub kept: Vec<String>,
    pub linked: Vec<String>,
    pub notes: Vec<String>,
}

struct Writer<'a> {
    root: &'a Path,
    force: bool,
    report: InitReport,
}

impl Writer<'_> {
    fn file(&mut self, rel: &str, body: &str) -> Result<()> {
        let p = self.root.join(rel);
        if p.exists() && !self.force {
            self.report.kept.push(rel.into());
            return Ok(());
        }
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, body)?;
        self.report.created.push(rel.into());
        Ok(())
    }
    fn dir(&mut self, rel: &str) -> Result<()> {
        let p = self.root.join(rel);
        if !p.exists() {
            std::fs::create_dir_all(&p)?;
            std::fs::write(p.join(".gitkeep"), "")?;
            self.report.created.push(format!("{rel}/"));
        }
        Ok(())
    }
    /// CLAUDE.md, GEMINI.md… pointing at AGENTS.md. An existing real file is left alone.
    fn link(&mut self, name: &str) -> Result<()> {
        let p = self.root.join(name);
        if p.symlink_metadata().is_ok() {
            self.report.kept.push(name.into());
            return Ok(());
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink("AGENTS.md", &p)?;
        #[cfg(not(unix))]
        std::fs::write(&p, "See AGENTS.md.\n")?;
        self.report.linked.push(format!("{name} -> AGENTS.md"));
        Ok(())
    }
}

pub struct InitOptions {
    pub name: String,
    pub lang: Lang,
    pub force: bool,
    /// Wire Claude Code hooks (.claude/settings.json) and Cursor rules.
    pub hooks: bool,
}

pub fn init(root: &Path, o: &InitOptions) -> Result<InitReport> {
    std::fs::create_dir_all(root)?;
    let root = root.canonicalize()?;
    let mut w = Writer {
        root: &root,
        force: o.force,
        report: InitReport {
            space: root.display().to_string(),
            ..Default::default()
        },
    };
    let l = o.lang;
    w.file(
        MARKER,
        &format!(
            "# Agent space managed by agentstory\nname = \"{}\"\nlang = \"{}\"\ncreated = \"{}\"\n",
            o.name.replace('"', "'"),
            l.pick("en", "zh"),
            today()
        ),
    )?;
    w.file("AGENTS.md", &agents_md(&o.name, l))?;
    for n in ["CLAUDE.md", "GEMINI.md", "QWEN.md"] {
        w.link(n)?;
    }
    w.file("memory/USER.md", &user_md(l))?;
    w.file("memory/NOW.md", &now_md(l))?;
    for t in TYPES {
        w.dir(&format!("memory/{t}"))?;
    }
    w.dir("memory/_proposals")?;
    w.file("wiki/index.md", &wiki_index(l))?;
    w.file("wiki/_template.md", &wiki_template(l))?;
    w.file("mistakes/index.md", &mistakes_index(l))?;
    w.file("mistakes/_template.md", &mistake_template(l))?;
    w.dir("journal")?;
    if o.hooks {
        wire_claude_hooks(&root, &mut w.report)?;
        w.file(
            ".cursor/rules/agent-space.mdc",
            "---\ndescription: Agent space rules (memory, wiki, mistakes)\nalwaysApply: true\n---\n\nRead and follow @AGENTS.md. Memory lives in memory/, knowledge in wiki/, the error log in mistakes/.\n",
        )?;
    }
    regenerate_index(&root, l)?;
    w.report.notes.push(l.pick(
            "Codex, OpenCode and most other agents read AGENTS.md directly. MCP: add `agentstory mcp` to your agent for session search.",
            "Codex、OpenCode 等大多数 Agent 会直接读 AGENTS.md。会话检索可以给 Agent 配上 MCP：`agentstory mcp`。"
        ).to_string());
    Ok(w.report)
}

/// Merge the memory nudge hook into .claude/settings.json without touching anything else.
fn wire_claude_hooks(root: &Path, r: &mut InitReport) -> Result<()> {
    let p = root.join(".claude/settings.json");
    let mut v: serde_json::Value = if p.exists() {
        serde_json::from_str(&std::fs::read_to_string(&p)?)
            .with_context(|| format!("{} is not valid JSON", p.display()))?
    } else {
        serde_json::json!({})
    };
    let cmd = "agentstory hook nudge";
    let hooks = v
        .as_object_mut()
        .context("settings.json is not an object")?
        .entry("hooks")
        .or_insert(serde_json::json!({}));
    let list = hooks
        .as_object_mut()
        .context("hooks is not an object")?
        .entry("UserPromptSubmit")
        .or_insert(serde_json::json!([]));
    let already = list.to_string().contains(cmd);
    if already {
        r.kept
            .push(".claude/settings.json (hook already present)".into());
        return Ok(());
    }
    list.as_array_mut()
        .context("UserPromptSubmit is not a list")?
        .push(serde_json::json!({"hooks": [{"type": "command", "command": cmd, "timeout": 5}]}));
    std::fs::create_dir_all(p.parent().unwrap())?;
    std::fs::write(&p, serde_json::to_string_pretty(&v)? + "\n")?;
    r.created
        .push(".claude/settings.json (UserPromptSubmit → agentstory hook nudge)".into());
    Ok(())
}

// ---------------------------------------------------------------- templates

fn agents_md(name: &str, l: Lang) -> String {
    match l {
        Lang::En => format!(
            r#"# {name}

This is an agent space. Every agent working here (Claude Code, Codex, Cursor, Gemini, …) shares the memory below.

## Start of every session

1. Read `memory/USER.md` (who the user is, preferences), `memory/NOW.md` (current priorities, open loops) and `memory/MEMORY.md` (index of entries). Open entries only when relevant.
2. Before working on a topic, check `wiki/index.md` and `mistakes/index.md`.
3. Asked about past work? Run `agentstory search <words>` before answering. Continuing someone else's session? Run `agentstory handoff <id>` first. If nothing turns up, say so; don't guess.

## Where things go

| What | Where |
|---|---|
| The user's facts, corrections, decisions; project state | `memory/<type>/<slug>.md` via `agentstory mem add` |
| Long-lived knowledge: how things work, procedures | `wiki/` via `agentstory wiki add` |
| Something went wrong: cause, fix, prevention | `mistakes/` via `agentstory mistake add` |
| Daily notes | `journal/YYYY-MM-DD.md` |

## Rules for memory

- Write in the same turn you learn it. Use absolute dates (2026-11-04), never "yesterday".
- Search before writing (`agentstory search`). Then add, update, supersede (`agentstory mem supersede old new`), or leave it.
- Store what the user said and what took effort to find out. Don't store one-off requests, things the code already shows, or "tool X doesn't work" without the conditions.
- Changes to `memory/USER.md` and preference entries: ask the user first (draft them in `memory/_proposals/`).
- Entries marked `private: true` are for one-to-one use only; don't bring them up in group chats.
- Never write secrets. `agentstory doctor` checks caps, links and leaked keys.
- A correction about how to do a kind of task belongs in that task's skill or wiki page first; only general preferences go to `memory/feedback/`.
"#
        ),
        Lang::Zh => format!(
            r#"# {name}

这是一个 Agent 空间。在这里干活的每个 Agent（Claude Code、Codex、Cursor、Gemini……）共用下面这套记忆。

## 每次开工

1. 读 `memory/USER.md`（用户是谁、偏好）、`memory/NOW.md`（现在的优先级、没收尾的事）和 `memory/MEMORY.md`（条目索引）。条目按需打开。
2. 涉及某个主题时，先查 `wiki/index.md` 和 `mistakes/index.md`。
3. 问到过去的事，先 `agentstory search <关键词>` 再答；接手别人的会话，先 `agentstory handoff <id>`。查不到就说查不到，不猜。

## 东西放哪

| 内容 | 位置 |
|---|---|
| 用户说的事实、纠正、决定，项目现状 | `memory/<类型>/<slug>.md`，用 `agentstory mem add` |
| 长期有效的知识：原理、流程、操作手册 | `wiki/`，用 `agentstory wiki add` |
| 做错的事：原因、正确做法、怎么防 | `mistakes/`，用 `agentstory mistake add` |
| 每日笔记 | `journal/YYYY-MM-DD.md` |

## 记忆规则

- 当轮就记。日期写绝对日期（2026-11-04），不写「昨天」。
- 写之前先 `agentstory search` 查重，再选新增、改写、替代（`agentstory mem supersede 旧 新`）或不动。
- 记用户亲口说的，和花了力气才查清的。不记一次性请求、代码里直接能看到的东西、不带条件的「某工具不能用」。
- 改 `memory/USER.md` 和偏好类条目，先问用户（草稿放 `memory/_proposals/`）。
- 标了 `private: true` 的条目只在一对一时用，群聊里不提。
- 绝不写密钥。`agentstory doctor` 会检查字数上限、坏链接和疑似泄露的密钥。
- 对某类任务做法的纠正，先写进那类任务的 skill 或 wiki 页；只有通用偏好才进 `memory/feedback/`。
"#
        ),
    }
}

fn user_md(l: Lang) -> String {
    l.pick(
        "# User\n\n> Always loaded. Keep under 3000 characters. Preferences are written as \"date: instruction\"; when one changes, replace it and move the old one to Superseded. Ask the user before editing.\n\n## Who\n\n## Preferences (active)\n\n## Superseded\n",
        "# 用户\n\n> 常驻层，每次开工都读，控制在 3000 字以内。偏好写成「日期：指令」，变了就原地替换，旧的挪到「已替代」。改这份文件先问用户。\n\n## 是谁\n\n## 偏好（有效）\n\n## 已替代\n",
    )
    .to_string()
}

fn now_md(l: Lang) -> String {
    l.pick(
        "# Now\n\n> Always loaded. Keep under 2000 characters. Current priorities, things waiting on the user, open loops. Move finished items into project entries.\n\nLast contact: \n\n## Priorities\n\n## Waiting on the user\n\n## Open loops\n",
        "# 现在\n\n> 常驻层，每次开工都读，控制在 2000 字以内。只写当前优先级、等用户决定的事、没收尾的事；做完的挪进项目条目。\n\n上次联系：\n\n## 当前优先级\n\n## 等用户决定\n\n## 没收尾\n",
    )
    .to_string()
}

fn wiki_index(l: Lang) -> String {
    l.pick(
        "# Wiki\n\n> Long-lived knowledge. One line per page; `agentstory wiki add` keeps this list.\n\n## Pages\n\n",
        "# Wiki 索引\n\n> 长期有效的知识。一页一行，`agentstory wiki add` 会自动维护。\n\n## 页面\n\n",
    )
    .to_string()
}

fn wiki_template(l: Lang) -> String {
    l.pick(
        "# {title}\n\n> Created {date}. One sentence: what this page is for.\n\n## Summary\n\n## Details\n\n## Related\n",
        "# {title}\n\n> {date} 建。一句话说明这页讲什么。\n\n## 要点\n\n## 细节\n\n## 相关\n",
    )
    .to_string()
}

fn mistakes_index(l: Lang) -> String {
    l.pick(
        "# Mistakes\n\n> One line per mistake, newest first: `- [date topic](file) — #tag — lesson`. When the same kind happens twice, turn it into a wiki checklist.\n\n",
        "# 错题本\n\n> 每次出错、被纠正、走了弯路，登记一行，新的在最上面：`- [日期 主题](文件) — #标签 — 一句话教训`。同类错误出现两次，提炼成 wiki 里的检查清单。\n\n",
    )
    .to_string()
}

fn mistake_template(l: Lang) -> String {
    l.pick(
        "# {date} {title}\n\n- Tag: {tag}\n- Source: {source}\n\n## What happened\n\n{what}\n\n## Why\n\n## Correct approach\n\n{lesson}\n\n## Prevention\n",
        "# {date} {title}\n\n- 标签：{tag}\n- 来源：{source}\n\n## 发生了什么\n\n{what}\n\n## 为什么\n\n## 正确做法\n\n{lesson}\n\n## 怎么防\n",
    )
    .to_string()
}

fn space_lang(root: &Path) -> Lang {
    std::fs::read_to_string(root.join(MARKER))
        .ok()
        .filter(|t| t.contains("lang = \"zh\""))
        .map(|_| Lang::Zh)
        .unwrap_or(Lang::En)
}

// ---------------------------------------------------------------- entries

pub fn frontmatter(text: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    if let Some(rest) = text.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            for line in rest[..end].lines() {
                if let Some((k, v)) = line.split_once(':') {
                    m.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
                }
            }
        }
    }
    m
}

pub struct NewEntry {
    pub kind: String,
    pub slug: String,
    pub description: String,
    pub body: String,
    pub source: String,
    pub private: bool,
}

pub fn add_entry(root: &Path, e: &NewEntry, replace: bool) -> Result<PathBuf> {
    if !TYPES.contains(&e.kind.as_str()) {
        bail!("type must be one of {}", TYPES.join(", "));
    }
    if !["owner", "agent", "untrusted"].contains(&e.source.as_str()) {
        bail!("--source must be owner, agent or untrusted");
    }
    let slug = slugify(&e.slug);
    let p = root.join("memory").join(&e.kind).join(format!("{slug}.md"));
    let mut created = today();
    if p.exists() {
        if !replace {
            bail!(
                "{} exists; use --update to rewrite it or `agentstory mem supersede` to replace it",
                p.display()
            );
        }
        created = frontmatter(&std::fs::read_to_string(&p)?)
            .get("created")
            .cloned()
            .unwrap_or(created);
    }
    let body = redact(e.body.trim());
    let mut fm = format!(
        "---\nname: {slug}\ndescription: {}\ntype: {}\ncreated: {created}\nupdated: {}\nsource: {}\nstatus: active\n",
        one_line(&e.description, 200),
        e.kind,
        today(),
        e.source
    );
    if e.private {
        fm.push_str("private: true\n");
    }
    fm.push_str("---\n\n");
    std::fs::create_dir_all(p.parent().unwrap())?;
    std::fs::write(&p, fm + &body + "\n")?;
    regenerate_index(root, space_lang(root))?;
    Ok(p)
}

fn entry_path(root: &Path, slug: &str) -> Result<PathBuf> {
    let hits: Vec<PathBuf> = TYPES
        .iter()
        .map(|t| root.join("memory").join(t).join(format!("{slug}.md")))
        .filter(|p| p.exists())
        .collect();
    match hits.len() {
        1 => Ok(hits[0].clone()),
        0 => bail!("no memory entry named {slug}"),
        _ => bail!("{slug} exists under several types"),
    }
}

/// Mark `old` superseded by `new` (which must exist). The old file is kept for history.
pub fn supersede(root: &Path, old: &str, new: &str) -> Result<()> {
    entry_path(root, new)?;
    let p = entry_path(root, old)?;
    let text = std::fs::read_to_string(&p)?;
    let mut out = String::new();
    let mut in_fm = false;
    let mut done = false;
    for (i, line) in text.lines().enumerate() {
        if i == 0 && line == "---" {
            in_fm = true;
        } else if in_fm && line == "---" {
            if !done {
                out.push_str(&format!("status: superseded\nsuperseded_by: {new}\n"));
            }
            in_fm = false;
        } else if in_fm && (line.starts_with("status:") || line.starts_with("superseded_by:")) {
            if !done {
                out.push_str(&format!("status: superseded\nsuperseded_by: {new}\n"));
                done = true;
            }
            continue;
        } else if in_fm && line.starts_with("updated:") {
            out.push_str(&format!("updated: {}\n", today()));
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    std::fs::write(&p, out)?;
    regenerate_index(root, space_lang(root))?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct EntryInfo {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub description: String,
    pub updated: String,
    pub status: String,
    pub private: bool,
}

pub fn list_entries(root: &Path) -> Vec<EntryInfo> {
    let mut out = vec![];
    for t in TYPES {
        let Ok(rd) = std::fs::read_dir(root.join("memory").join(t)) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .collect();
        files.sort();
        for p in files {
            let fm = frontmatter(&std::fs::read_to_string(&p).unwrap_or_default());
            out.push(EntryInfo {
                name: fm
                    .get("name")
                    .cloned()
                    .unwrap_or_else(|| p.file_stem().unwrap().to_string_lossy().into_owned()),
                kind: t.to_string(),
                description: fm.get("description").cloned().unwrap_or_default(),
                updated: fm.get("updated").cloned().unwrap_or_default(),
                status: fm.get("status").cloned().unwrap_or_else(|| "active".into()),
                private: fm.get("private").is_some_and(|v| v == "true"),
                path: p.strip_prefix(root).unwrap_or(&p).display().to_string(),
            });
        }
    }
    out
}

/// Rebuild memory/MEMORY.md from entry frontmatter (active entries only).
pub fn regenerate_index(root: &Path, l: Lang) -> Result<usize> {
    let titles = [
        (
            "feedback",
            l.pick("Preferences and corrections", "偏好和纠正"),
        ),
        ("project", l.pick("Projects and state", "项目和现状")),
        ("decision", l.pick("Decisions", "决策")),
        ("reference", l.pick("References", "资源和指针")),
    ];
    let mut out = vec![
        l.pick("# Memory index", "# 记忆索引").to_string(),
        String::new(),
        l.pick(
            "> Generated by `agentstory mem index`; don't edit by hand. Read USER.md, NOW.md and this index at the start of every session; open entries when relevant. 🔒 = private.",
            "> 由 `agentstory mem index` 生成，别手改。每次开工读 USER.md、NOW.md 和本索引，条目按需打开。🔒 表示私密。",
        )
        .to_string(),
        String::new(),
    ];
    let entries = list_entries(root);
    let mut n = 0;
    for (kind, title) in titles {
        let rows: Vec<&EntryInfo> = entries
            .iter()
            .filter(|e| e.kind == kind && e.status == "active")
            .collect();
        if rows.is_empty() {
            continue;
        }
        out.push(format!("## {title}"));
        out.push(String::new());
        for e in rows {
            out.push(format!(
                "- [{}]({}){} — {} ({})",
                e.name,
                e.path.trim_start_matches("memory/"),
                if e.private { " 🔒" } else { "" },
                e.description,
                e.updated
            ));
            n += 1;
        }
        out.push(String::new());
    }
    std::fs::write(root.join("memory/MEMORY.md"), out.join("\n"))?;
    Ok(n)
}

// ---------------------------------------------------------------- mistakes & wiki

pub struct NewMistake {
    pub title: String,
    pub tag: String,
    pub lesson: String,
    pub what: String,
    pub source: String,
}

pub fn add_mistake(root: &Path, m: &NewMistake) -> Result<PathBuf> {
    let date = today();
    let file = format!("{date}-{}.md", slugify(&m.title));
    let p = root.join("mistakes").join(&file);
    if p.exists() {
        bail!("{} exists", p.display());
    }
    let tpl = std::fs::read_to_string(root.join("mistakes/_template.md"))
        .unwrap_or_else(|_| mistake_template(space_lang(root)));
    let tag = if m.tag.starts_with('#') {
        m.tag.clone()
    } else {
        format!("#{}", m.tag)
    };
    let body = tpl
        .replace("{date}", &date)
        .replace("{title}", &m.title)
        .replace("{tag}", &tag)
        .replace("{lesson}", &m.lesson)
        .replace("{what}", &m.what)
        .replace("{source}", &m.source);
    std::fs::write(&p, redact(&body))?;
    // Newest first: insert right after the header block.
    let idx = root.join("mistakes/index.md");
    let text = std::fs::read_to_string(&idx).unwrap_or_else(|_| mistakes_index(space_lang(root)));
    let line = format!(
        "- [{date} {}]({file}) — {tag} — {}",
        m.title,
        one_line(&m.lesson, 120)
    );
    let mut lines: Vec<&str> = text.lines().collect();
    let pos = lines
        .iter()
        .position(|l| l.starts_with("- ["))
        .unwrap_or(lines.len());
    lines.insert(pos, &line);
    std::fs::write(&idx, lines.join("\n") + "\n")?;
    Ok(p)
}

/// Add a wiki page. `dir` puts it in a subfolder (wiki/<dir>/); `section` files the index line
/// under the first heading containing that text instead of at the end.
pub fn add_wiki(
    root: &Path,
    title: &str,
    summary: &str,
    body: &str,
    dir: Option<&str>,
    section: Option<&str>,
) -> Result<PathBuf> {
    let slug = slugify(title);
    let rel = match dir {
        Some(d) => format!("{}/{slug}.md", d.trim_matches('/')),
        None => format!("{slug}.md"),
    };
    let p = root.join("wiki").join(&rel);
    if p.exists() {
        bail!("{} exists", p.display());
    }
    let tpl = std::fs::read_to_string(root.join("wiki/_template.md"))
        .unwrap_or_else(|_| wiki_template(space_lang(root)));
    let mut text = tpl.replace("{title}", title).replace("{date}", &today());
    if !body.trim().is_empty() {
        text.push_str(&format!("\n{}\n", body.trim()));
    }
    std::fs::create_dir_all(p.parent().unwrap())?;
    std::fs::write(&p, redact(&text))?;
    let idx = root.join("wiki/index.md");
    let t = std::fs::read_to_string(&idx).unwrap_or_else(|_| wiki_index(space_lang(root)));
    let line = format!("- [{title}]({rel}) — {}", one_line(summary, 120));
    let mut lines: Vec<String> = t.lines().map(String::from).collect();
    let at = section.and_then(|sec| {
        let h = lines
            .iter()
            .position(|l| l.starts_with('#') && l.contains(sec))?;
        // After the last line of that section's content (before the next heading).
        let end = lines[h + 1..]
            .iter()
            .position(|l| l.starts_with('#') || l.starts_with("---"))
            .map(|i| h + 1 + i)
            .unwrap_or(lines.len());
        let mut pos = end;
        while pos > h + 1 && lines[pos - 1].trim().is_empty() {
            pos -= 1;
        }
        Some(pos)
    });
    match at {
        Some(pos) => lines.insert(pos, line),
        None => {
            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.pop();
            }
            lines.push(line);
        }
    }
    std::fs::write(&idx, lines.join("\n") + "\n")?;
    Ok(p)
}

// ---------------------------------------------------------------- doctor

#[derive(Debug, Default, Serialize)]
pub struct Doctor {
    pub problems: Vec<String>,
    pub notes: Vec<String>,
}

fn md_files(dir: &Path) -> Vec<PathBuf> {
    if !dir.exists() {
        return vec![];
    }
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "md"))
        .map(|e| e.into_path())
        .collect()
}

pub fn doctor(root: &Path) -> Result<Doctor> {
    let mut d = Doctor::default();
    let chars = |rel: &str| {
        std::fs::read_to_string(root.join(rel))
            .map(|t| t.chars().count())
            .unwrap_or(0)
    };
    let (u, n, i) = (
        chars("memory/USER.md"),
        chars("memory/NOW.md"),
        chars("memory/MEMORY.md"),
    );
    if u > USER_CAP {
        d.problems.push(format!(
            "memory/USER.md is {u} chars (cap {USER_CAP}): merge or move details into entries"
        ));
    }
    if n > NOW_CAP {
        d.problems.push(format!(
            "memory/NOW.md is {n} chars (cap {NOW_CAP}): move finished items into project entries"
        ));
    }
    if u + n + i > ALWAYS_LOADED_CAP {
        d.problems.push(format!(
            "always-loaded files total {} chars (cap {ALWAYS_LOADED_CAP})",
            u + n + i
        ));
    }
    d.notes.push(format!(
        "always-loaded: {} / {ALWAYS_LOADED_CAP} chars",
        u + n + i
    ));

    let mut names: HashSet<String> = HashSet::new();
    let mut newest = std::time::SystemTime::UNIX_EPOCH;
    for t in TYPES {
        for p in md_files(&root.join("memory").join(t)) {
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            let rel = p.strip_prefix(root).unwrap_or(&p).display().to_string();
            newest = newest.max(
                std::fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .unwrap_or(newest),
            );
            let fm = frontmatter(&text);
            if fm.is_empty() {
                d.problems.push(format!("{rel}: no frontmatter"));
                continue;
            }
            for k in [
                "name",
                "description",
                "type",
                "created",
                "updated",
                "source",
                "status",
            ] {
                if !fm.contains_key(k) {
                    d.problems.push(format!("{rel}: missing `{k}`"));
                }
            }
            if fm.get("status").is_some_and(|s| s == "superseded")
                && !fm.contains_key("superseded_by")
            {
                d.problems
                    .push(format!("{rel}: superseded without superseded_by"));
            }
            let c = text.chars().count();
            if c > ENTRY_CAP {
                d.problems
                    .push(format!("{rel}: {c} chars (cap {ENTRY_CAP}), split it"));
            }
            names.insert(fm.get("name").cloned().unwrap_or_default());
            names.insert(p.file_stem().unwrap().to_string_lossy().into_owned());
        }
    }
    let idx_time = std::fs::metadata(root.join("memory/MEMORY.md"))
        .and_then(|m| m.modified())
        .ok();
    if idx_time.is_some_and(|t| t < newest) {
        d.problems
            .push("memory/MEMORY.md is older than its entries: run `agentstory mem index`".into());
    }
    for dir in ["wiki", "mistakes", "journal"] {
        for p in md_files(&root.join(dir)) {
            names.insert(p.file_stem().unwrap().to_string_lossy().into_owned());
        }
    }
    let link = regex::Regex::new(r"\[\[([^\]|#]+)").unwrap();
    let mut broken = BTreeMap::new();
    let mut all = vec![];
    for dir in ["memory", "wiki", "mistakes", "journal"] {
        all.extend(md_files(&root.join(dir)));
    }
    all.push(root.join("AGENTS.md"));
    for p in &all {
        if p.file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('_'))
        {
            continue; // templates
        }
        let Ok(text) = std::fs::read_to_string(p) else {
            continue;
        };
        let rel = p.strip_prefix(root).unwrap_or(p).display().to_string();
        for c in link.captures_iter(&text) {
            let target = c[1].trim().rsplit('/').next().unwrap_or("").to_string();
            if !names.contains(&target) {
                broken
                    .entry(target)
                    .or_insert_with(Vec::new)
                    .push(rel.clone());
            }
        }
        let (_, kinds) = crate::clean::redact_labeled(&text);
        if !kinds.is_empty() {
            let kinds: Vec<String> = kinds
                .iter()
                .map(|k| {
                    k.replace("[REDACTED:", "")
                        .replace("$1 [REDACTED]", "bearer")
                        .replace("${1}[REDACTED]", "assignment")
                        .replace(']', "")
                })
                .collect();
            d.problems.push(format!(
                "{rel}: looks like it contains a secret or personal number ({})",
                kinds.join(", ")
            ));
        }
    }
    for (t, files) in broken {
        d.notes.push(format!(
            "[[{t}]] has no page yet (linked from {})",
            files.join(", ")
        ));
    }
    let pending: Vec<String> = md_files(&root.join("memory/_proposals"))
        .into_iter()
        .filter(|p| {
            std::fs::read_to_string(p)
                .unwrap_or_default()
                .contains("status: pending")
        })
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    if !pending.is_empty() {
        d.notes.push(format!(
            "{} proposal(s) waiting for the user: {}",
            pending.len(),
            pending.join(", ")
        ));
    }
    let n_m = md_files(&root.join("mistakes")).len().saturating_sub(2);
    let n_w = md_files(&root.join("wiki")).len().saturating_sub(2);
    d.notes.push(format!(
        "{} memory entries, {n_w} wiki pages, {n_m} mistakes",
        list_entries(root).len()
    ));
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(
            slugify("Paid gen: small batches!"),
            "paid-gen-small-batches"
        );
        assert_eq!(slugify("付费出片 小批量"), "付费出片-小批量");
        assert_eq!(slugify("***"), "untitled");
    }

    #[test]
    fn reads_frontmatter() {
        let fm = frontmatter("---\nname: a\nstatus: active\n---\n\nbody");
        assert_eq!(fm["name"], "a");
        assert_eq!(fm["status"], "active");
    }
}
