use agentstory::clean::one_line;
use agentstory::index::{Filter, Index, Level, default_db_path, default_home};
use agentstory::render::View;
use agentstory::time::{fmt_local, parse_when};
use agentstory::{connectors, handoff, hook, mcp, mine, render, space, wiki};
use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

/// Memory workbench for coding agents: search and hand off sessions across agents, and keep a shared memory, wiki and mistakes log.
///
/// Reads Claude Code, Codex, Cursor, Gemini CLI, Qwen Code, OpenCode and Hermes sessions
/// in place (never modifies them), keeps a redacted full-text index, and prints results
/// for people or, with --json, for other agents.
#[derive(Parser)]
#[command(name = "agentstory", version, about, long_about = None)]
struct Cli {
    /// Index database (default: <data dir>/agentstory/agentstory.db, or $AGENTSTORY_DB)
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    /// Home directory to scan for agent data (default: $HOME, or $AGENTSTORY_HOME)
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    /// Don't refresh the index before answering
    #[arg(long, global = true)]
    no_update: bool,
    /// Machine-readable JSON output
    #[arg(long, global = true)]
    json: bool,
    /// Agent space directory (default: nearest parent with .agentstory.toml, or $AGENTSTORY_SPACE)
    #[arg(long, global = true)]
    space: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum WikiCmd {
    /// Add a wiki page from the template and list it in wiki/index.md
    Add {
        title: String,
        /// One line for the index
        #[arg(long, short = 's', default_value = "")]
        summary: String,
        /// Page body (use - to read stdin)
        #[arg(long, default_value = "")]
        body: String,
        /// Subfolder under wiki/, e.g. playbooks
        #[arg(long)]
        dir: Option<String>,
        /// File the index line under the first heading containing this text
        #[arg(long)]
        section: Option<String>,
    },
    /// Export sessions as a Markdown wiki (session pages, daily digests, index)
    Export {
        dir: PathBuf,
        /// Skip sessions with fewer human turns than this
        #[arg(long, default_value_t = 1)]
        min_turns: i64,
        #[command(flatten)]
        filter: FilterArgs,
    },
}

#[derive(Subcommand)]
enum MemCmd {
    /// Add (or with --update rewrite) a memory entry: feedback | project | decision | reference
    Add {
        #[arg(value_parser = ["feedback", "project", "decision", "reference"])]
        kind: String,
        slug: String,
        /// One line for the index
        #[arg(long, short = 'd')]
        description: String,
        /// Entry body (use - to read stdin)
        #[arg(long, default_value = "")]
        body: String,
        /// owner (the user said it), agent (inferred/measured), untrusted (web, third parties)
        #[arg(long, default_value = "owner")]
        source: String,
        /// Only for one-to-one conversations
        #[arg(long)]
        private: bool,
        /// Overwrite an existing entry with the same slug
        #[arg(long)]
        update: bool,
    },
    /// Mark an entry superseded by another (the old file is kept)
    Supersede { old: String, new: String },
    /// Rebuild memory/MEMORY.md from entry frontmatter
    Index,
    /// List entries
    List,
}

#[derive(Subcommand)]
enum MistakeCmd {
    /// Add a mistake page (dated, from the template) and put it at the top of mistakes/index.md
    Add {
        title: String,
        /// One-sentence lesson for the index
        #[arg(long, short = 'l')]
        lesson: String,
        #[arg(long, short = 't', default_value = "general")]
        tag: String,
        /// What happened (use - to read stdin)
        #[arg(long, default_value = "")]
        what: String,
        /// Where it came from, e.g. a session id
        #[arg(long, default_value = "")]
        source: String,
    },
}

#[derive(Subcommand)]
enum HookCmd {
    /// UserPromptSubmit hook: every N human prompts, remind the agent to record what it learned
    Nudge {
        #[arg(long, default_value_t = 10)]
        every: u64,
    },
}

#[derive(Args, Clone, Default)]
struct FilterArgs {
    /// Only these agents (repeatable): claude-code, codex, cursor, gemini, qwen, opencode, hermes
    #[arg(long = "agent", short = 'a')]
    agents: Vec<String>,
    /// Since: 2026-11-01, 2026-11-01T09:00, 7d, 24h, today
    #[arg(long)]
    since: Option<String>,
    /// Before this time
    #[arg(long)]
    until: Option<String>,
    /// Working directory contains this text
    #[arg(long, short = 'p')]
    project: Option<String>,
}

impl FilterArgs {
    fn build(&self) -> Result<Filter> {
        Ok(Filter {
            agents: self.agents.clone(),
            since: self.since.as_deref().map(parse_when).transpose()?,
            until: self.until.as_deref().map(parse_when).transpose()?,
            project: self.project.clone(),
            source: None,
            level: Default::default(),
        })
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Build or refresh the index (incremental; only changed files are re-read)
    Index {
        /// Drop everything and rebuild
        #[arg(long)]
        full: bool,
        #[arg(long = "agent", short = 'a')]
        agents: Vec<String>,
    },
    /// Full-text search across all sessions (CJK works; terms under 3 chars use substring match)
    Search {
        query: Vec<String>,
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
        /// Also match harness messages (scheduled prompts, hook feedback, summaries)
        #[arg(long)]
        all: bool,
        /// Only "sessions" or only "notes" (space Markdown: memory, wiki, mistakes, journal)
        #[arg(long, value_parser = ["sessions", "notes"])]
        only: Option<String>,
        /// How deep to look: chat (what was said; default), work (+ commands and files),
        /// full (+ tool output). Without this flag, an empty chat search retries at full.
        #[arg(long, short = 'l', value_parser = ["chat", "work", "full"])]
        level: Option<String>,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// List sessions, newest first
    Sessions {
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        /// Only sessions whose text contains this
        #[arg(long, short = 'g')]
        grep: Option<String>,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// Read one session in order
    Show {
        /// Session id, unique prefix, agent:id, or source path
        id: String,
        /// Only the last N turns
        #[arg(long)]
        last: Option<usize>,
        /// Only turns mentioning this, with context
        #[arg(long)]
        around: Option<String>,
        /// Turns of context around each --around hit
        #[arg(long, default_value_t = 2)]
        context: usize,
        /// Also list shell commands per turn
        #[arg(long)]
        tools: bool,
    },
    /// Handoff card so another agent (or person) can continue a session
    Handoff { id: String },
    /// Create an agent space: AGENTS.md, memory/, wiki/, mistakes/, journal/, agent wiring
    Init {
        /// Directory (default: current)
        dir: Option<PathBuf>,
        #[arg(long)]
        name: Option<String>,
        /// Template language: en or zh
        #[arg(long, default_value = "en")]
        lang: String,
        /// Overwrite existing template files
        #[arg(long)]
        force: bool,
        /// Don't add the Claude Code hook or Cursor rule
        #[arg(long)]
        no_hooks: bool,
    },
    /// Memory entries (one fact per file)
    #[command(subcommand)]
    Mem(MemCmd),
    /// The mistakes log
    #[command(subcommand)]
    Mistake(MistakeCmd),
    /// Wiki pages, and exporting sessions as a wiki
    #[command(subcommand)]
    Wiki(WikiCmd),
    /// Check the space: size caps, frontmatter, stale index, broken [[links]], leaked secrets
    Doctor,
    /// Find places where the user corrected an agent (raw material for the mistakes log)
    Corrections {
        #[arg(short = 'n', long, default_value_t = 30)]
        limit: usize,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// Everything said on one day across agents, redacted (input for nightly distillation)
    Digest {
        /// YYYY-MM-DD (default: yesterday)
        day: Option<String>,
        #[arg(long = "agent", short = 'a')]
        agents: Vec<String>,
    },
    /// Hooks for agent harnesses
    #[command(subcommand)]
    Hook(HookCmd),
    /// Which agents are supported, where they keep data, and what was found here
    Agents,
    /// Sessions and turns per agent in the index
    Stats,
    /// Run as an MCP server on stdio (tools: search_sessions, list_sessions, read_session, handoff)
    Mcp,
}

fn main() {
    // Behave like a normal Unix tool when piped into `head`: exit quietly on a closed pipe.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    if let Err(e) = run() {
        eprintln!("agentstory: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let db = cli.db.clone().unwrap_or_else(default_db_path);
    let home = cli.home.clone().unwrap_or_else(default_home);
    if let Cmd::Mcp = cli.cmd {
        return mcp::serve(db, home);
    }
    if let Cmd::Hook(HookCmd::Nudge { every }) = &cli.cmd {
        return hook::nudge(*every, None);
    }
    let read_arg = |s: &str| -> Result<String> {
        if s == "-" {
            let mut b = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut b)?;
            Ok(b)
        } else {
            Ok(s.to_string())
        }
    };
    // Commands that only touch the space's files don't need the index.
    match &cli.cmd {
        Cmd::Init {
            dir,
            name,
            lang,
            force,
            no_hooks,
        } => {
            let root = dir.clone().unwrap_or(std::env::current_dir()?);
            let name = name.clone().unwrap_or_else(|| {
                root.canonicalize()
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_else(|| "Agent space".into())
            });
            let r = space::init(
                &root,
                &space::InitOptions {
                    name,
                    lang: space::Lang::parse(lang)?,
                    force: *force,
                    hooks: !*no_hooks,
                },
            )?;
            Index::open(&db)?.register_space(Path::new(&r.space))?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                println!("agent space: {}", r.space);
                for c in &r.created {
                    println!("  + {c}");
                }
                for c in &r.linked {
                    println!("  ~ {c}");
                }
                if !r.kept.is_empty() {
                    println!(
                        "  kept {} existing file(s): {}",
                        r.kept.len(),
                        r.kept.join(", ")
                    );
                }
                for n in &r.notes {
                    println!("{n}");
                }
            }
            return Ok(());
        }
        Cmd::Mem(m) => {
            let root = space::locate(cli.space.as_deref())?;
            match m {
                MemCmd::Add {
                    kind,
                    slug,
                    description,
                    body,
                    source,
                    private,
                    update,
                } => {
                    let p = space::add_entry(
                        &root,
                        &space::NewEntry {
                            kind: kind.clone(),
                            slug: slug.clone(),
                            description: description.clone(),
                            body: read_arg(body)?,
                            source: source.clone(),
                            private: *private,
                        },
                        *update,
                    )?;
                    println!("{}", p.display());
                }
                MemCmd::Supersede { old, new } => {
                    space::supersede(&root, old, new)?;
                    println!("{old} → superseded by {new}");
                }
                MemCmd::Index => {
                    let n = space::regenerate_index(
                        &root,
                        if std::fs::read_to_string(root.join(space::MARKER))
                            .unwrap_or_default()
                            .contains("\"zh\"")
                        {
                            space::Lang::Zh
                        } else {
                            space::Lang::En
                        },
                    )?;
                    println!("memory/MEMORY.md: {n} active entries");
                }
                MemCmd::List => {
                    let es = space::list_entries(&root);
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&es)?);
                    } else {
                        for e in es {
                            println!(
                                "{:<10} {:<11} {:<36} {}{}",
                                e.kind,
                                e.status,
                                e.name,
                                e.description,
                                if e.private { " 🔒" } else { "" }
                            );
                        }
                    }
                }
            }
            return Ok(());
        }
        Cmd::Mistake(MistakeCmd::Add {
            title,
            lesson,
            tag,
            what,
            source,
        }) => {
            let root = space::locate(cli.space.as_deref())?;
            let p = space::add_mistake(
                &root,
                &space::NewMistake {
                    title: title.clone(),
                    lesson: lesson.clone(),
                    tag: tag.clone(),
                    what: read_arg(what)?,
                    source: source.clone(),
                },
            )?;
            println!("{}", p.display());
            return Ok(());
        }
        Cmd::Wiki(WikiCmd::Add {
            title,
            summary,
            body,
            dir,
            section,
        }) => {
            let root = space::locate(cli.space.as_deref())?;
            let p = space::add_wiki(
                &root,
                title,
                summary,
                &read_arg(body)?,
                dir.as_deref(),
                section.as_deref(),
            )?;
            println!("{}", p.display());
            return Ok(());
        }
        Cmd::Doctor => {
            let root = space::locate(cli.space.as_deref())?;
            let d = space::doctor(&root)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&d)?);
            } else {
                println!(
                    "{} problem(s){}",
                    d.problems.len(),
                    if d.problems.is_empty() { "." } else { ":" }
                );
                for p in &d.problems {
                    println!("  ✗ {p}");
                }
                for n in &d.notes {
                    println!("  · {n}");
                }
            }
            if !d.problems.is_empty() {
                std::process::exit(2);
            }
            return Ok(());
        }
        _ => {}
    }
    let mut idx = Index::open(&db)?;
    if let Some(root) = cli
        .space
        .clone()
        .or_else(|| std::env::var("AGENTSTORY_SPACE").ok().map(PathBuf::from))
        .or_else(|| std::env::current_dir().ok().and_then(|d| space::find(&d)))
    {
        if root.join(space::MARKER).is_file() {
            idx.register_space(&root.canonicalize()?)?;
        }
    }
    let refresh = |idx: &mut Index| -> Result<()> {
        if !cli.no_update || idx.is_empty()? {
            let first = idx.is_empty()?;
            if first && std::io::stderr().is_terminal() {
                eprintln!("agentstory: building the index for the first time…");
            }
            let st = idx.update(&home, &[], false)?;
            for e in st.errors.iter().take(3) {
                eprintln!("agentstory: skipped {e}");
            }
        }
        Ok(())
    };
    match &cli.cmd {
        Cmd::Index { full, agents } => {
            let t = std::time::Instant::now();
            let st = idx.update(&home, agents, *full)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&st)?);
            } else {
                println!(
                    "scanned {} files, re-read {}, removed {}, wrote {} sessions in {:.1}s",
                    st.files_scanned,
                    st.files_changed,
                    st.files_removed,
                    st.sessions_written,
                    t.elapsed().as_secs_f64()
                );
                for e in &st.errors {
                    println!("  skipped {e}");
                }
            }
        }
        Cmd::Search {
            query,
            limit,
            all,
            only,
            level,
            filter,
        } => {
            refresh(&mut idx)?;
            let mut f = filter.build()?;
            f.source = only.clone();
            f.level = level
                .as_deref()
                .map(Level::parse)
                .transpose()?
                .unwrap_or_default();
            let (hits, used) =
                idx.search_auto(&query.join(" "), &f, *limit, *all, level.is_some())?;
            if !cli.json && level.is_none() && used == Level::Full && !hits.is_empty() {
                println!(
                    "(not found in what was said; showing matches in commands and tool output)\n"
                );
            }
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&hits)?);
            } else if hits.is_empty() {
                println!("No matches. Try other words, a shorter term, or --all.");
            } else {
                for h in hits {
                    if h.source == "note" {
                        println!(
                            "{} · {} · {}",
                            h.agent,
                            fmt_local(h.ts),
                            h.path.as_deref().unwrap_or(&h.id)
                        );
                        println!("  {}\n", h.snippet);
                        continue;
                    }
                    let short = agentstory::index::short_id(&h.id);
                    println!(
                        "{} · {} · turn {} · {}  (agentstory show {short} --around …)",
                        h.agent,
                        fmt_local(h.ts),
                        h.turn,
                        h.speaker
                    );
                    if let Some(p) = &h.project {
                        println!("  {p}");
                    }
                    println!("  {}\n", h.snippet);
                }
            }
        }
        Cmd::Sessions {
            limit,
            grep,
            filter,
        } => {
            refresh(&mut idx)?;
            let rows = idx.sessions(&filter.build()?, grep.as_deref(), *limit)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                for r in rows {
                    let short = agentstory::index::short_id(&r.id);
                    println!(
                        "{short}  {:<11} {} → {}  {:>3} human / {:>3} turns  {}",
                        r.agent,
                        fmt_local(r.started),
                        fmt_local(r.updated).get(5..).unwrap_or(""),
                        r.user_turns,
                        r.turns,
                        r.project
                            .as_deref()
                            .map(|p| one_line(p, 40))
                            .unwrap_or_default()
                    );
                    println!(
                        "          {}",
                        r.title
                            .as_deref()
                            .map(|t| one_line(t, 100))
                            .unwrap_or_default()
                    );
                }
            }
        }
        Cmd::Show {
            id,
            last,
            around,
            context,
            tools,
        } => {
            refresh(&mut idx)?;
            let row = idx.resolve(id)?;
            let s = idx.load(&row.key)?;
            let v = View {
                last: *last,
                around: around.clone(),
                context: *context,
                tools: *tools,
            };
            if cli.json {
                let all = agentstory::model::turns(&s);
                let picked: Vec<_> = render::select(&all, &v);
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "key": row.key, "agent": s.agent, "id": s.id, "project": s.project, "title": s.title,
                        "started": s.started, "updated": s.updated, "turns": picked
                    }))?
                );
            } else {
                print!("{}", render::markdown(&s, &v));
            }
        }
        Cmd::Handoff { id } => {
            refresh(&mut idx)?;
            let row = idx.resolve(id)?;
            let h = handoff::build(&idx.load(&row.key)?);
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&h)?);
            } else {
                print!("{}", handoff::markdown(&h));
            }
        }
        Cmd::Corrections { limit, filter } => {
            refresh(&mut idx)?;
            let cs = mine::corrections(&idx, &filter.build()?, *limit)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&cs)?);
            } else {
                print!("{}", mine::corrections_markdown(&cs));
            }
        }
        Cmd::Digest { day, agents } => {
            refresh(&mut idx)?;
            let day = day.clone().unwrap_or_else(|| {
                (chrono::Local::now().date_naive() - chrono::Duration::days(1))
                    .format("%Y-%m-%d")
                    .to_string()
            });
            print!("{}", mine::digest(&idx, &day, agents)?);
        }
        Cmd::Wiki(WikiCmd::Export {
            dir,
            min_turns,
            filter,
        }) => {
            refresh(&mut idx)?;
            let st = wiki::export(&idx, dir, &filter.build()?, *min_turns)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&st)?);
            } else {
                println!(
                    "{} sessions, {} days → {} ({} pages written, {} unchanged)",
                    st.sessions,
                    st.days,
                    dir.display(),
                    st.pages_written,
                    st.pages_unchanged
                );
            }
        }
        Cmd::Agents => {
            let mut out = vec![];
            for c in connectors::all() {
                let found = c.discover(&home).len();
                out.push(serde_json::json!({"agent": c.name(), "locations": c.locations(), "files_found": found}));
                if !cli.json {
                    println!(
                        "{:<12} {:>5} files   {}",
                        c.name(),
                        found,
                        c.locations().join("  ")
                    );
                }
            }
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
        }
        Cmd::Stats => {
            refresh(&mut idx)?;
            let rows = idx.stats()?;
            if cli.json {
                let v: Vec<_> = rows.iter().map(|(a, n, t, u)| serde_json::json!({"agent": a, "sessions": n, "turns": t, "latest": u})).collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("index: {}", db.display());
                for (a, n, t, u) in rows {
                    println!(
                        "{a:<12} {n:>6} sessions {t:>8} turns   latest {}",
                        fmt_local(u)
                    );
                }
            }
        }
        Cmd::Mcp
        | Cmd::Hook(_)
        | Cmd::Init { .. }
        | Cmd::Mem(_)
        | Cmd::Mistake(_)
        | Cmd::Doctor
        | Cmd::Wiki(WikiCmd::Add { .. }) => unreachable!(),
    }
    Ok(())
}
