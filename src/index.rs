//! Local index: one SQLite file with normalized sessions plus an FTS5 (trigram) table of turns.
//! Everything stored here has been redacted. Sources are never modified.

use crate::clean::{head_tail, one_line, redact, truncate};
use crate::connectors::{self, Connector};
use crate::model::{Role, Session, turns};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: i64 = 6;

/// (agent, sessions, turns, latest update)
pub type AgentStats = (String, i64, i64, Option<i64>);

pub struct Index {
    pub db: Connection,
}

#[derive(Debug, Default, Serialize)]
pub struct IndexStats {
    pub files_scanned: usize,
    pub files_changed: usize,
    pub files_removed: usize,
    pub sessions_written: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionRow {
    pub key: String,
    pub id: String,
    pub agent: String,
    pub project: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub started: Option<i64>,
    pub updated: Option<i64>,
    pub user_turns: i64,
    pub turns: i64,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub key: String,
    pub id: String,
    pub agent: String,
    pub turn: i64,
    pub ts: Option<i64>,
    pub speaker: String,
    pub score: f64,
    pub snippet: String,
    pub project: Option<String>,
    pub title: Option<String>,
    /// "session" for conversation turns, "note" for space Markdown (memory, wiki, mistakes, journal).
    pub source: String,
    /// File path for notes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Where the words were found: "chat" (what was said), "work" (commands, files), "output", "note".
    pub matched: String,
}

#[derive(Debug, Default, Clone)]
pub struct Filter {
    pub agents: Vec<String>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub project: Option<String>,
    /// Only "sessions" or only "notes".
    pub source: Option<String>,
    /// How deep to look inside sessions.
    pub level: Level,
}

/// Search depth inside sessions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// What the person and the agent said to each other.
    #[default]
    Chat,
    /// Plus what the agent did: commands run, files written.
    Work,
    /// Plus what tools printed.
    Full,
}

impl Level {
    pub fn parse(s: &str) -> Result<Level> {
        match s {
            "chat" => Ok(Level::Chat),
            "work" => Ok(Level::Work),
            "full" => Ok(Level::Full),
            _ => bail!("level must be chat, work or full"),
        }
    }
    fn columns(self) -> &'static [&'static str] {
        match self {
            Level::Chat => &["text"],
            Level::Work => &["text", "act"],
            Level::Full => &["text", "act", "out"],
        }
    }
}

pub fn default_db_path() -> PathBuf {
    if let Ok(p) = std::env::var("AGENTSTORY_DB") {
        return PathBuf::from(p);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("agentstory")
        .join("agentstory.db")
}

pub fn default_home() -> PathBuf {
    std::env::var("AGENTSTORY_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
}

impl Index {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
        }
        let db = Connection::open(path)?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        let v: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if v != SCHEMA_VERSION {
            db.execute_batch(
                "DROP TABLE IF EXISTS files; DROP TABLE IF EXISTS sessions; DROP TABLE IF EXISTS turns; DROP TABLE IF EXISTS notes;",
            )?;
        }
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, agent TEXT NOT NULL, mtime INTEGER, size INTEGER);
             CREATE TABLE IF NOT EXISTS sessions(
                key TEXT PRIMARY KEY, id TEXT NOT NULL, agent TEXT NOT NULL, file TEXT NOT NULL, source TEXT,
                project TEXT, title TEXT, model TEXT, started INTEGER, updated INTEGER,
                user_turns INTEGER, n_turns INTEGER, body TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS sessions_updated ON sessions(updated);
             CREATE INDEX IF NOT EXISTS sessions_id ON sessions(id);
             CREATE INDEX IF NOT EXISTS sessions_file ON sessions(file);
             CREATE VIRTUAL TABLE IF NOT EXISTS turns USING fts5(
                key UNINDEXED, turn UNINDEXED, ts UNINDEXED, agent UNINDEXED, speaker UNINDEXED, text, act, out,
                tokenize = 'trigram');
             CREATE TABLE IF NOT EXISTS spaces(path TEXT PRIMARY KEY, added INTEGER);
             CREATE VIRTUAL TABLE IF NOT EXISTS notes USING fts5(
                path UNINDEXED, space UNINDEXED, kind UNINDEXED, mtime UNINDEXED, title, text,
                tokenize = 'trigram');",
        )?;
        db.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(Index { db })
    }

    /// Scan every connector under `home`, re-parse changed files, drop vanished ones.
    pub fn update(&mut self, home: &Path, only: &[String], full: bool) -> Result<IndexStats> {
        let mut st = IndexStats::default();
        if full {
            self.db.execute_batch(
                "DELETE FROM files; DELETE FROM sessions; DELETE FROM turns; DELETE FROM notes;",
            )?;
        }
        let conns: Vec<Box<dyn Connector>> = connectors::all()
            .into_iter()
            .filter(|c| only.is_empty() || only.iter().any(|o| o == c.name()))
            .collect();
        let mut seen = std::collections::HashSet::new();
        for c in &conns {
            for f in c.discover(home) {
                st.files_scanned += 1;
                let key = f.display().to_string();
                seen.insert(key.clone());
                let meta = std::fs::metadata(&f).ok();
                let mtime = connectors::mtime_ms(&f);
                let size = meta.map(|m| m.len() as i64).unwrap_or(0);
                // SQLite stores keep recent writes in -wal; count it as part of the file.
                let wal = PathBuf::from(format!("{key}-wal"));
                let mtime = mtime.max(connectors::mtime_ms(&wal));
                let known: Option<(i64, i64)> = self
                    .db
                    .query_row("SELECT mtime, size FROM files WHERE path=?", [&key], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })
                    .optional()?;
                if known == Some((mtime, size)) {
                    continue;
                }
                match c.parse(&f) {
                    Ok(sessions) => {
                        let tx = self.db.transaction()?;
                        remove_file(&tx, &key)?;
                        for s in &sessions {
                            write_session(&tx, &key, s)?;
                            st.sessions_written += 1;
                        }
                        tx.execute(
                            "REPLACE INTO files VALUES (?,?,?,?)",
                            params![key, c.name(), mtime, size],
                        )?;
                        tx.commit()?;
                        st.files_changed += 1;
                    }
                    Err(e) => st.errors.push(format!("{}: {e:#}", f.display())),
                }
            }
        }
        if only.is_empty() || only.iter().any(|o| o == "notes") {
            self.update_notes(&mut st, &mut seen)?;
        }
        // Forget files that disappeared (only for connectors we scanned).
        let mut names: Vec<&str> = conns.iter().map(|c| c.name()).collect();
        if only.is_empty() || only.iter().any(|o| o == "notes") {
            names.push("note");
        }
        let gone: Vec<String> = {
            let mut stmt = self.db.prepare("SELECT path, agent FROM files")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            rows.flatten()
                .filter(|(p, a)| names.contains(&a.as_str()) && !seen.contains(p))
                .map(|(p, _)| p)
                .collect()
        };
        for p in gone {
            let tx = self.db.transaction()?;
            remove_file(&tx, &p)?;
            tx.execute("DELETE FROM notes WHERE path=?", [&p])?;
            tx.execute("DELETE FROM files WHERE path=?", [&p])?;
            tx.commit()?;
            st.files_removed += 1;
        }
        Ok(st)
    }

    /// Remember an agent space so its Markdown is indexed with the sessions.
    pub fn register_space(&self, root: &Path) -> Result<()> {
        self.db.execute(
            "INSERT OR IGNORE INTO spaces VALUES (?, ?)",
            params![
                root.display().to_string(),
                chrono::Utc::now().timestamp_millis()
            ],
        )?;
        Ok(())
    }

    pub fn spaces(&self) -> Result<Vec<PathBuf>> {
        let mut stmt = self.db.prepare("SELECT path FROM spaces ORDER BY added")?;
        Ok(stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .flatten()
            .map(PathBuf::from)
            .collect())
    }

    fn update_notes(
        &mut self,
        st: &mut IndexStats,
        seen: &mut std::collections::HashSet<String>,
    ) -> Result<()> {
        for root in self.spaces()? {
            if !root.join(crate::space::MARKER).is_file() {
                continue;
            }
            let mut files: Vec<(PathBuf, &str)> = vec![];
            for kind in ["memory", "wiki", "mistakes", "journal"] {
                let dir = root.join(kind);
                if !dir.exists() {
                    continue;
                }
                for e in walkdir::WalkDir::new(&dir)
                    .into_iter()
                    .filter_map(|e| e.ok())
                {
                    let p = e.path();
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let in_private_dir = p.strip_prefix(&dir).ok().is_some_and(|r| {
                        r.components()
                            .any(|c| c.as_os_str().to_string_lossy().starts_with('_'))
                    });
                    if e.file_type().is_file() && name.ends_with(".md") && !in_private_dir {
                        files.push((p.to_path_buf(), kind));
                    }
                }
            }
            files.push((root.join("AGENTS.md"), "rules"));
            for (f, kind) in files {
                if !f.is_file() {
                    continue;
                }
                st.files_scanned += 1;
                let key = f.display().to_string();
                seen.insert(key.clone());
                let mtime = connectors::mtime_ms(&f);
                let size = std::fs::metadata(&f).map(|m| m.len() as i64).unwrap_or(0);
                let known: Option<(i64, i64)> = self
                    .db
                    .query_row("SELECT mtime, size FROM files WHERE path=?", [&key], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })
                    .optional()?;
                if known == Some((mtime, size)) {
                    continue;
                }
                let text = std::fs::read_to_string(&f).unwrap_or_default();
                let tx = self.db.transaction()?;
                tx.execute("DELETE FROM notes WHERE path=?", [&key])?;
                for (title, body) in chunk_markdown(&text, &f) {
                    tx.execute(
                        "INSERT INTO notes(path, space, kind, mtime, title, text) VALUES (?,?,?,?,?,?)",
                        params![key, root.display().to_string(), kind, mtime, redact(&title), redact(&body)],
                    )?;
                }
                tx.execute(
                    "REPLACE INTO files VALUES (?,?,?,?)",
                    params![key, "note", mtime, size],
                )?;
                tx.commit()?;
                st.files_changed += 1;
            }
        }
        Ok(())
    }

    fn search_notes(&self, terms: &[String], limit: usize) -> Result<Vec<Hit>> {
        let long: Vec<&String> = terms.iter().filter(|t| t.chars().count() >= 3).collect();
        let short: Vec<&String> = terms.iter().filter(|t| t.chars().count() < 3).collect();
        let mut sql = format!(
            "SELECT path, space, kind, mtime, title, text, {} FROM notes WHERE 1=1",
            if long.is_empty() {
                "0.0"
            } else {
                "bm25(notes)"
            }
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![];
        if !long.is_empty() {
            sql.push_str(" AND notes MATCH ?");
            args.push(Box::new(
                long.iter()
                    .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
                    .collect::<Vec<_>>()
                    .join(" AND "),
            ));
        }
        for t in &short {
            sql.push_str(" AND (text LIKE ? OR title LIKE ?)");
            args.push(Box::new(format!("%{t}%")));
            args.push(Box::new(format!("%{t}%")));
        }
        sql.push_str(" LIMIT 500");
        let mut stmt = self.db.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())),
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, f64>(6)?,
                ))
            },
        )?;
        let mut hits: Vec<Hit> = rows
            .flatten()
            .map(|(path, space, kind, mtime, title, text, bm)| {
                let rel = path
                    .strip_prefix(&format!("{space}/"))
                    .unwrap_or(&path)
                    .to_string();
                let rel_score = if long.is_empty() { 1.0 } else { -bm };
                let rel_score = rel_score
                    + short
                        .iter()
                        .map(|t| text.matches(t.as_str()).count() as f64 * 0.5)
                        .sum::<f64>();
                Hit {
                    key: path.clone(),
                    id: rel,
                    agent: format!("note:{kind}"),
                    turn: 0,
                    ts: mtime,
                    speaker: "note".into(),
                    // Curated knowledge outranks raw conversation.
                    score: rel_score * 1.5,
                    snippet: snippet(&text, terms, 280),
                    project: Some(space),
                    title: Some(title),
                    source: "note".into(),
                    path: Some(path),
                    matched: "note".into(),
                }
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut per: std::collections::HashMap<String, usize> = Default::default();
        hits.retain(|h| {
            let n = per.entry(h.key.clone()).or_default();
            *n += 1;
            *n <= 1
        });
        hits.truncate(limit);
        Ok(hits)
    }

    pub fn is_empty(&self) -> Result<bool> {
        Ok(self
            .db
            .query_row("SELECT count(*) FROM files", [], |r| r.get::<_, i64>(0))?
            == 0)
    }

    pub fn sessions(
        &self,
        f: &Filter,
        grep: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SessionRow>> {
        let (mut sql, mut args) = filter_sql(
            "SELECT key,id,agent,project,title,model,started,updated,user_turns,n_turns,source FROM sessions WHERE 1=1",
            f,
            "updated",
        );
        if let Some(g) = grep {
            sql.push_str(
                " AND key IN (SELECT key FROM turns WHERE text LIKE ? OR act LIKE ? OR out LIKE ?)",
            );
            for _ in 0..3 {
                args.push(Box::new(format!("%{g}%")));
            }
        }
        sql.push_str(" ORDER BY updated DESC LIMIT ?");
        args.push(Box::new(limit as i64));
        let mut stmt = self.db.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())),
            row_to_session,
        )?;
        Ok(rows.flatten().collect())
    }

    /// Resolve an id prefix, `agent:id` key, or path to exactly one session.
    pub fn resolve(&self, needle: &str) -> Result<SessionRow> {
        let mut stmt = self.db.prepare(
            "SELECT key,id,agent,project,title,model,started,updated,user_turns,n_turns,source FROM sessions
             WHERE key = ?1 OR id = ?1 OR id LIKE ?2 OR key LIKE ?2 OR source = ?1 OR id LIKE ?3 OR id LIKE ?4 ORDER BY updated DESC LIMIT 20",
        )?;
        let rows: Vec<SessionRow> = stmt
            .query_map(
                params![
                    needle,
                    format!("{needle}%"),
                    format!("session-{needle}%"),
                    format!("ses_{needle}%")
                ],
                row_to_session,
            )?
            .flatten()
            .collect();
        if let Some(exact) = rows.iter().find(|r| r.id == needle || r.key == needle) {
            return Ok(exact.clone());
        }
        match rows.len() {
            0 => bail!(
                "no session matches `{needle}` (try `agentstory sessions` or `agentstory search`)"
            ),
            1 => Ok(rows.into_iter().next().unwrap()),
            _ => {
                let list: Vec<String> = rows
                    .iter()
                    .map(|r| format!("  {}  {}", r.key, r.title.clone().unwrap_or_default()))
                    .collect();
                bail!(
                    "`{needle}` matches {} sessions, give more characters:\n{}",
                    rows.len(),
                    list.join("\n")
                )
            }
        }
    }

    pub fn load(&self, key: &str) -> Result<Session> {
        let body: String =
            self.db
                .query_row("SELECT body FROM sessions WHERE key=?", [key], |r| r.get(0))?;
        Ok(serde_json::from_str(&body)?)
    }

    pub fn search(
        &self,
        query: &str,
        f: &Filter,
        limit: usize,
        include_system: bool,
    ) -> Result<Vec<Hit>> {
        let terms: Vec<String> = split_terms(query);
        if terms.is_empty() {
            bail!("empty query");
        }
        let want_notes =
            f.source.as_deref() != Some("sessions") && f.agents.is_empty() && f.project.is_none();
        if f.source.as_deref() == Some("notes") {
            return self.search_notes(&terms, limit);
        }
        let long: Vec<&String> = terms.iter().filter(|t| t.chars().count() >= 3).collect();
        let short: Vec<&String> = terms.iter().filter(|t| t.chars().count() < 3).collect();
        let cols = f.level.columns();
        let mut sql = String::from(
            "SELECT t.key, t.turn, t.ts, t.agent, t.speaker, t.text, s.id, s.project, s.title, t.act, t.out, ",
        );
        // What was said outranks what was done, which outranks what tools printed.
        sql.push_str(if long.is_empty() {
            "0.0"
        } else {
            "bm25(turns, 0, 0, 0, 0, 0, 1.0, 0.5, 0.3)"
        });
        sql.push_str(" FROM turns t JOIN sessions s ON s.key = t.key WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![];
        if !long.is_empty() {
            sql.push_str(" AND turns MATCH ?");
            let q = long
                .iter()
                .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(" AND ");
            args.push(Box::new(format!("{{{}}} : ({q})", cols.join(" "))));
        }
        for t in &short {
            let any: Vec<String> = cols.iter().map(|c| format!("t.{c} LIKE ?")).collect();
            sql.push_str(&format!(" AND ({})", any.join(" OR ")));
            for _ in cols {
                args.push(Box::new(format!("%{t}%")));
            }
        }
        if !include_system {
            sql.push_str(" AND t.speaker NOT IN ('system')");
        }
        if !f.agents.is_empty() {
            sql.push_str(&format!(
                " AND t.agent IN ({})",
                vec!["?"; f.agents.len()].join(",")
            ));
            for a in &f.agents {
                args.push(Box::new(a.clone()));
            }
        }
        if let Some(x) = f.since {
            sql.push_str(" AND CAST(t.ts AS INTEGER) >= ?");
            args.push(Box::new(x));
        }
        if let Some(x) = f.until {
            sql.push_str(" AND CAST(t.ts AS INTEGER) < ?");
            args.push(Box::new(x));
        }
        if let Some(p) = &f.project {
            sql.push_str(" AND s.project LIKE ?");
            args.push(Box::new(format!("%{p}%")));
        }
        sql.push_str(" LIMIT 2000");
        let now = chrono::Utc::now().timestamp_millis();
        let mut stmt = self.db.prepare(&sql)?;
        let mut hits: Vec<Hit> = stmt
            .query_map(
                rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, Option<String>>(7)?,
                        r.get::<_, Option<String>>(8)?,
                        r.get::<_, String>(9)?,
                        r.get::<_, String>(10)?,
                        r.get::<_, f64>(11)?,
                    ))
                },
            )?
            .flatten()
            .map(
                |(key, turn, ts, agent, speaker, text, id, project, title, act, out, bm)| {
                    let rel = if long.is_empty() { 1.0 } else { -bm };
                    let rel = rel
                        + short
                            .iter()
                            .map(|t| {
                                text.matches(t.as_str()).count() as f64 * 0.5
                                    + act.matches(t.as_str()).count() as f64 * 0.25
                                    + out.matches(t.as_str()).count() as f64 * 0.15
                            })
                            .sum::<f64>();
                    // The shallowest level that holds every term decides the label; the snippet
                    // centres on a term only found at that level.
                    let (lt, la) = (text.to_lowercase(), act.to_lowercase());
                    let lterms: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();
                    let in_cols = |cols: &[&str]| {
                        lterms
                            .iter()
                            .all(|t| cols.iter().any(|c| c.contains(t.as_str())))
                    };
                    let missing = |cols: &[&str]| -> Vec<String> {
                        terms
                            .iter()
                            .filter(|t| !cols.iter().any(|c| c.contains(&t.to_lowercase())))
                            .cloned()
                            .collect()
                    };
                    let (matched, snip) = if f.level == Level::Chat || in_cols(&[&lt]) {
                        ("chat", snippet(&text, &terms, 280))
                    } else if f.level == Level::Work || in_cols(&[&lt, &la]) {
                        (
                            "work",
                            format!("[did] {}", snippet(&act, &missing(&[&lt]), 270)),
                        )
                    } else {
                        (
                            "output",
                            format!("[output] {}", snippet(&out, &missing(&[&lt, &la]), 270)),
                        )
                    };
                    let age_days = ts
                        .map(|t| ((now - t) as f64 / 86_400_000.0).max(0.0))
                        .unwrap_or(365.0);
                    let decay = 0.5f64.powf(age_days / 30.0);
                    let human = if speaker == "system" || speaker == "summary" {
                        0.6
                    } else {
                        1.0
                    };
                    Hit {
                        snippet: snip,
                        score: rel * human * (0.3 + 0.7 * decay),
                        key,
                        id,
                        agent,
                        turn,
                        ts,
                        speaker,
                        project,
                        title,
                        source: "session".into(),
                        path: None,
                        matched: matched.into(),
                    }
                },
            )
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        // At most 2 hits per session so one long session doesn't flood the results.
        let mut per: std::collections::HashMap<String, usize> = Default::default();
        hits.retain(|h| {
            let n = per.entry(h.key.clone()).or_default();
            *n += 1;
            *n <= 2
        });
        if want_notes {
            let since = f.since.unwrap_or(i64::MIN);
            hits.extend(
                self.search_notes(&terms, limit)?
                    .into_iter()
                    .filter(|h| h.ts.unwrap_or(0) >= since),
            );
            hits.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        hits.truncate(limit);
        Ok(hits)
    }

    /// Search at the requested level; when the level was not chosen explicitly and the light
    /// `chat` search finds nothing, look again at `full`. Returns the level that produced the hits.
    pub fn search_auto(
        &self,
        query: &str,
        f: &Filter,
        limit: usize,
        include_system: bool,
        explicit: bool,
    ) -> Result<(Vec<Hit>, Level)> {
        let hits = self.search(query, f, limit, include_system)?;
        let sessions_found = hits.iter().any(|h| h.source == "session");
        if explicit
            || f.level != Level::Chat
            || sessions_found
            || f.source.as_deref() == Some("notes")
        {
            return Ok((hits, f.level));
        }
        let mut deeper = f.clone();
        deeper.level = Level::Full;
        let more = self.search(query, &deeper, limit, include_system)?;
        if more.iter().any(|h| h.source == "session") {
            return Ok((more, Level::Full));
        }
        Ok((hits, f.level))
    }

    pub fn stats(&self) -> Result<Vec<AgentStats>> {
        let mut stmt = self.db.prepare("SELECT agent, count(*), sum(n_turns), max(updated) FROM sessions GROUP BY agent ORDER BY 2 DESC")?;
        Ok(stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .flatten()
            .collect())
    }
}

fn row_to_session(r: &rusqlite::Row) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        key: r.get(0)?,
        id: r.get(1)?,
        agent: r.get(2)?,
        project: r.get(3)?,
        title: r.get(4)?,
        model: r.get(5)?,
        started: r.get(6)?,
        updated: r.get(7)?,
        user_turns: r.get(8)?,
        turns: r.get(9)?,
        source: r.get(10)?,
    })
}

fn filter_sql(base: &str, f: &Filter, ts_col: &str) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut sql = base.to_string();
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![];
    if !f.agents.is_empty() {
        sql.push_str(&format!(
            " AND agent IN ({})",
            vec!["?"; f.agents.len()].join(",")
        ));
        for a in &f.agents {
            args.push(Box::new(a.clone()));
        }
    }
    if let Some(x) = f.since {
        sql.push_str(&format!(" AND {ts_col} >= ?"));
        args.push(Box::new(x));
    }
    if let Some(x) = f.until {
        sql.push_str(" AND started < ?");
        args.push(Box::new(x));
    }
    if let Some(p) = &f.project {
        sql.push_str(" AND project LIKE ?");
        args.push(Box::new(format!("%{p}%")));
    }
    (sql, args)
}

fn remove_file(tx: &rusqlite::Transaction, file: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM turns WHERE key IN (SELECT key FROM sessions WHERE file=?)",
        [file],
    )?;
    tx.execute("DELETE FROM sessions WHERE file=?", [file])?;
    Ok(())
}

/// Redact and shrink a session before it is stored.
pub fn sanitize(s: &Session) -> Session {
    let mut s = s.clone();
    s.title = s.title.as_deref().map(redact);
    for m in &mut s.messages {
        let limit = if m.role == Role::Tool { 600 } else { 20_000 };
        m.text = redact(&truncate(&m.text, limit));
        if let Some(t) = &mut m.tool {
            // Long enough that an action after a heredoc still reaches the handoff card.
            t.command = t.command.as_deref().map(|c| redact(&truncate(c, 8000)));
            t.summary = t.summary.as_deref().map(redact);
        }
    }
    s
}

fn write_session(tx: &rusqlite::Transaction, file: &str, raw: &Session) -> Result<()> {
    let s = sanitize(raw);
    // The stored body keeps 600 chars per tool output; the search column gets more,
    // from the raw session, so errors and values deep in an output are findable.
    let outputs: Vec<String> = turns(raw)
        .iter()
        .map(|t| {
            let joined: Vec<String> = t.outputs.iter().map(|o| head_tail(o, 1500, 500)).collect();
            redact(&truncate(&joined.join("\n"), 16_000))
        })
        .collect();
    let key = format!("{}:{}", s.agent, s.id);
    let ts = turns(&s);
    let user_turns = ts.iter().filter(|t| !t.is_system).count() as i64;
    tx.execute("DELETE FROM turns WHERE key=?", [&key])?;
    tx.execute(
        "REPLACE INTO sessions VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params![
            key,
            s.id,
            s.agent,
            file,
            s.source,
            s.project,
            s.title,
            s.model,
            s.started,
            s.updated,
            user_turns,
            ts.len() as i64,
            serde_json::to_string(&s)?
        ],
    )?;
    let mut stmt = tx.prepare_cached(
        "INSERT INTO turns(key, turn, ts, agent, speaker, text, act, out) VALUES (?,?,?,?,?,?,?,?)",
    )?;
    let store_output = crate::config::CONFIG.index.store_output.unwrap_or(true);
    for t in &ts {
        let text = format!(
            "[{}] {}\n[assistant] {}",
            t.speaker,
            truncate(&t.input, 20_000),
            truncate(&t.reply, 20_000)
        );
        // Files written and commands run make "which session touched X" searchable.
        let act: Vec<String> = t
            .files
            .iter()
            .map(|f| format!("edit {f}"))
            .chain(t.commands.iter().map(|c| format!("$ {}", one_line(c, 300))))
            .collect();
        let act = truncate(&act.join("\n"), 4000);
        let out = if store_output {
            outputs.get(t.index - 1).map(String::as_str).unwrap_or("")
        } else {
            ""
        };
        stmt.execute(params![
            key,
            t.index as i64,
            t.ts,
            s.agent,
            t.speaker,
            text,
            act,
            out
        ])?;
    }
    Ok(())
}

/// Short display id: the first 8 characters after a generic prefix (`session-`, `ses_`, `rollout-`).
pub fn short_id(id: &str) -> &str {
    let rest = ["session-", "ses_", "rollout-"]
        .iter()
        .find_map(|p| id.strip_prefix(p))
        .unwrap_or(id);
    let end = rest
        .char_indices()
        .nth(8)
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    &rest[..end]
}

pub fn split_terms(q: &str) -> Vec<String> {
    // "quoted phrases" stay together; everything else splits on whitespace.
    let mut out = vec![];
    let mut cur = String::new();
    let mut quoted = false;
    for ch in q.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                if !cur.trim().is_empty() {
                    out.push(cur.trim().to_string());
                }
                cur.clear();
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(cur.clone());
                }
                cur.clear();
            }
            c => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

pub fn snippet(text: &str, terms: &[String], width: usize) -> String {
    let lower = text.to_lowercase();
    let pos = terms
        .iter()
        .filter_map(|t| lower.find(&t.to_lowercase()))
        .min()
        .unwrap_or(0);
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let ci = chars.iter().position(|(b, _)| *b >= pos).unwrap_or(0);
    let start = ci.saturating_sub(width / 3);
    let end = (start + width).min(chars.len());
    let mut s: String = chars[start..end].iter().map(|(_, c)| *c).collect();
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        s,
        if end < chars.len() { "…" } else { "" }
    )
}

/// Split Markdown into heading-sized chunks for the notes index.
fn chunk_markdown(text: &str, path: &Path) -> Vec<(String, String)> {
    let mut out = vec![];
    let mut title = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut cur = String::new();
    for line in text.lines() {
        let heading = line.starts_with("# ") || line.starts_with("## ") || line.starts_with("### ");
        if (heading && cur.chars().count() > 200) || cur.chars().count() > 1500 {
            out.push((title.clone(), std::mem::take(&mut cur)));
        }
        if heading {
            title = line.trim_start_matches('#').trim().to_string();
        }
        cur.push_str(line);
        cur.push('\n');
    }
    if !cur.trim().is_empty() {
        out.push((title, cur));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn splits_quoted_terms() {
        assert_eq!(
            split_terms(r#"memory "two words" x"#),
            vec!["memory", "two words", "x"]
        );
    }
    #[test]
    fn head_tail_keeps_both_ends() {
        assert_eq!(head_tail("short", 3, 3), "short");
        let s = format!("{}{}{}", "a".repeat(10), "b".repeat(100), "ERR");
        let ht = head_tail(&s, 10, 3);
        assert!(
            ht.starts_with("aaaaaaaaaa\n…\n") && ht.ends_with("ERR"),
            "{ht}"
        );
    }

    #[test]
    fn snippet_centers_on_term() {
        let t = format!("{}needle{}", "a ".repeat(300), " b".repeat(300));
        let s = snippet(&t, &["needle".into()], 60);
        assert!(s.contains("needle") && s.starts_with('…'));
    }
}
