//! End-to-end tests: a fake home directory with one session per supported agent.

use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn copy_dir(src: &Path, dst: &Path) {
    for e in walkdir::WalkDir::new(src) {
        let e = e.unwrap();
        let rel = e.path().strip_prefix(src).unwrap();
        let to = dst.join(rel);
        if e.file_type().is_dir() {
            std::fs::create_dir_all(&to).unwrap();
        } else {
            std::fs::copy(e.path(), &to).unwrap();
        }
    }
}

fn hermes_db(home: &Path) {
    let dir = home.join(".hermes");
    std::fs::create_dir_all(&dir).unwrap();
    let db = Connection::open(dir.join("state.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY, source TEXT NOT NULL, display_name TEXT, model TEXT, started_at REAL, cwd TEXT, title TEXT);
         CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT,
            tool_call_id TEXT, tool_calls TEXT, tool_name TEXT, timestamp REAL NOT NULL, active INTEGER);
         INSERT INTO sessions VALUES ('20261104_060000_hm5555', 'cli', NULL, 'hermes-4', 1793772000.0, '/work/notes', NULL);
         INSERT INTO messages (session_id, role, content, timestamp) VALUES ('20261104_060000_hm5555', 'user', 'remember that the staging db moved to port 6543', 1793772000.0);
         INSERT INTO messages (session_id, role, content, tool_calls, timestamp) VALUES ('20261104_060000_hm5555', 'assistant', '',
            '[{\"id\":\"h1\",\"type\":\"function\",\"function\":{\"name\":\"terminal\",\"arguments\":\"{\\\"command\\\":\\\"psql -p 6543 -c select 1\\\"}\"}}]', 1793772001.0);
         INSERT INTO messages (session_id, role, content, tool_call_id, tool_name, timestamp) VALUES ('20261104_060000_hm5555', 'tool', '{\"status\":\"error\",\"output\":\"connection refused\"}', 'h1', 'terminal', 1793772002.0);
         INSERT INTO messages (session_id, role, content, timestamp) VALUES ('20261104_060000_hm5555', 'assistant', 'Saved to memory; the port does not answer yet.', 1793772003.0);
         INSERT INTO messages (session_id, role, content, timestamp, active) VALUES ('20261104_060000_hm5555', 'user', 'REWOUND_AWAY', 1793772004.0, 0);",
    )
    .unwrap();
}

fn opencode_db(home: &Path) {
    let dir = home.join(".local/share/opencode");
    std::fs::create_dir_all(&dir).unwrap();
    let db = Connection::open(dir.join("opencode.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE session_v2 (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, parent_id TEXT, slug TEXT NOT NULL, directory TEXT NOT NULL,
            title TEXT, version TEXT NOT NULL, time_created INTEGER, time_updated INTEGER);
         CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, type TEXT NOT NULL, seq INTEGER NOT NULL,
            time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);
         INSERT INTO session_v2 VALUES ('ses_oc6666', 'p', NULL, 's', '/work/web', 'Fix navbar', '2.0.14', 1793775600000, 1793775660000);
         INSERT INTO session_v2 VALUES ('ses_child', 'p', 'ses_oc6666', 's', '/work/web', 'child', '2.0.14', 1793775600000, 1793775660000);
         INSERT INTO session_message VALUES ('m1', 'ses_oc6666', 'user', 1, 1793775600000, 1793775600000, '{\"text\":\"the navbar overlaps the hero on mobile\"}');
         INSERT INTO session_message VALUES ('m2', 'ses_oc6666', 'assistant', 2, 1793775601000, 1793775610000,
            '{\"agent\":\"build\",\"model\":{\"id\":\"claude-sonnet-5-5\",\"providerID\":\"anthropic\"},\"content\":[{\"type\":\"text\",\"text\":\"Adding a mobile breakpoint.\"},{\"type\":\"tool\",\"id\":\"o1\",\"name\":\"edit\",\"state\":{\"status\":\"completed\",\"input\":{\"path\":\"/work/web/nav.css\",\"oldString\":\"a\",\"newString\":\"b\"},\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}},{\"type\":\"tool\",\"id\":\"o2\",\"name\":\"shell\",\"state\":{\"status\":\"completed\",\"input\":{\"command\":\"npm run lint\"},\"content\":[{\"type\":\"text\",\"text\":\"2 problems\"}],\"metadata\":{\"exit\":1}}}]}');
         INSERT INTO session_message VALUES ('m3', 'ses_child', 'user', 1, 1793775600000, 1793775600000, '{\"text\":\"CHILD_SESSION_TEXT\"}');",
    )
    .unwrap();
}

struct Env {
    _tmp: TempDir,
    home: PathBuf,
    db: PathBuf,
}

fn env() -> Env {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for agent in ["claude", "cursor", "codex", "gemini", "qwen", "pi", "dsh"] {
        copy_dir(&fixtures.join(agent), &home);
    }
    hermes_db(&home);
    opencode_db(&home);
    let db = tmp.path().join("index/agentstory.db");
    Env {
        _tmp: tmp,
        home,
        db,
    }
}

fn agentstory(e: &Env) -> Command {
    let mut c = Command::cargo_bin("agentstory").unwrap();
    c.arg("--home")
        .arg(&e.home)
        .arg("--db")
        .arg(&e.db)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("HERMES_HOME")
        .env_remove("QWEN_HOME")
        .env_remove("XDG_DATA_HOME")
        .env("TZ", "Asia/Shanghai")
        .env(
            "AGENTSTORY_CONFIG",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config.toml"),
        );
    c
}

fn json(e: &Env, args: &[&str]) -> Value {
    let out = agentstory(e).arg("--json").args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn indexes_every_agent() {
    let e = env();
    let st = json(&e, &["index"]);
    assert_eq!(st["errors"].as_array().unwrap().len(), 0, "{st}");
    let stats = json(&e, &["stats"]);
    let agents: Vec<&str> = stats
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["agent"].as_str().unwrap())
        .collect();
    for a in [
        "claude-code",
        "cursor",
        "codex",
        "gemini",
        "qwen",
        "hermes",
        "opencode",
        "pi",
        "dsh",
    ] {
        assert!(agents.contains(&a), "missing {a} in {agents:?}");
    }
    // Second run re-reads nothing.
    let again = json(&e, &["index"]);
    assert_eq!(again["files_changed"], 0);
}

#[test]
fn search_finds_cjk_and_filters_by_agent() {
    let e = env();
    let hits = json(&e, &["search", "按钮"]);
    assert_eq!(hits[0]["agent"], "claude-code");
    assert_eq!(hits[0]["speaker"], "Alice");
    let hits = json(&e, &["search", "rate", "limiter", "--agent", "codex"]);
    assert_eq!(hits.as_array().unwrap().len(), 1);
    let hits = json(&e, &["search", "limiter", "--agent", "gemini"]);
    assert!(hits.as_array().unwrap().is_empty());
    // Files written and commands are searchable.
    let hits = json(&e, &["search", "nav.css"]);
    assert_eq!(hits[0]["agent"], "opencode");
    // Two-character CJK falls back to substring match.
    assert!(!json(&e, &["search", "分支"]).as_array().unwrap().is_empty());
    // Tool output is searchable and labelled as such.
    let hits = json(&e, &["search", "snapshot mismatch"]);
    assert_eq!(hits[0]["agent"], "claude-code");
    assert!(
        hits[0]["snippet"].as_str().unwrap().starts_with("[output]"),
        "{hits}"
    );
}

#[test]
fn excludes_sidechains_children_rewinds_and_injected_context() {
    let e = env();
    for needle in [
        "SIDECHAIN_SHOULD_NOT_APPEAR",
        "CHILD_SESSION_TEXT",
        "REWOUND_AWAY",
        "AGENTS.md instructions",
        "ABANDONED_BRANCH_TEXT",
        "STALE_GENERATION_TEXT",
        "Current runtime context",
    ] {
        let hits = json(&e, &["search", needle, "--all"]);
        assert!(
            hits.as_array().unwrap().is_empty(),
            "{needle} leaked: {hits}"
        );
    }
    let s = json(&e, &["sessions", "--agent", "claude-code"]);
    assert_eq!(
        s.as_array().unwrap().len(),
        1,
        "duplicate uuid should not create extra turns"
    );
}

#[test]
fn redacts_secrets_everywhere() {
    let e = env();
    agentstory(&e)
        .args(["show", "aaaa1111"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sk-test").not())
        .stdout(predicate::str::contains("[REDACTED:key]"));
    assert!(
        json(&e, &["search", "sk-test1234567890"])
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn show_reads_turns_in_order() {
    let e = env();
    let v = json(&e, &["show", "aaaa1111", "--last", "1"]);
    let turns = v["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 1);
    assert!(turns[0]["input"].as_str().unwrap().contains("别推 main"));
    let v = json(
        &e,
        &["show", "cdx-2222", "--around", "mocked", "--context", "0"],
    );
    assert_eq!(
        v["turns"].as_array().unwrap().len(),
        2,
        "assistant reply + compaction summary mention it"
    );
    agentstory(&e)
        .args(["show", "zzzz"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no session matches"));
}

#[test]
fn handoff_card_has_the_essentials() {
    let e = env();
    let h = json(&e, &["handoff", "aaaa1111"]);
    assert_eq!(h["goal"]["who"], "Alice");
    assert!(h["goal"]["text"].as_str().unwrap().contains("蓝色"));
    assert!(
        h["later_instructions"][0]["text"]
            .as_str()
            .unwrap()
            .contains("别推 main")
    );
    assert!(
        h["latest_summary"]
            .as_str()
            .unwrap()
            .contains("snapshot failing")
    );
    assert_eq!(h["files_written"][0][0], "/work/app/src/login.css");
    let kinds: Vec<&str> = h["key_actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"commit") && kinds.contains(&"push") && kinds.contains(&"message"),
        "{kinds:?}"
    );
    assert!(
        h["errors"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("snapshot mismatch")
    );
    assert!(
        h["last_reply"]
            .as_str()
            .unwrap()
            .contains("feature/blue-button")
    );
    assert_eq!(
        h["resume_command"],
        "claude --resume aaaa1111-0000-4000-8000-000000000001"
    );
    // Relay messages (a configured chat command) count as what the agent said.
    agentstory(&e)
        .args(["show", "aaaa1111"])
        .assert()
        .stdout(predicate::str::contains("我先更新快照再提交"));
}

#[test]
fn every_connector_extracts_tools_and_errors() {
    let e = env();
    let cases = [
        ("cur-1111", "/work/app/README.md", 0),
        ("cdx-2222", "src/limiter.rs", 1),
        ("gem-3333", "/work/ops/NOTES.md", 1),
        ("qwen-4444", "/work/calc/calc.py", 1),
        ("20261104_060000_hm5555", "", 1),
        ("ses_oc6666", "/work/web/nav.css", 1),
        ("pi-7777", "/work/pi/server.ts", 1),
        // exit-code suffix + denied push = 2 errors; relative edit path resolved against cwd
        (
            "session-6f1c2a9e",
            "/Users/alice/dev/parser/src/lexer.rs",
            2,
        ),
        (
            "session-zstd0001",
            "/Users/alice/dev/parser/src/lexer.rs",
            2,
        ),
    ];
    for (id, file, errors) in cases {
        let h = json(&e, &["handoff", id]);
        if !file.is_empty() {
            assert_eq!(h["files_written"][0][0], file, "{id}: {h}");
        }
        assert_eq!(h["errors"].as_array().unwrap().len(), errors, "{id}: {h}");
        assert!(h["last_reply"].is_string(), "{id} has no reply");
    }
    let s = json(&e, &["sessions", "--agent", "gemini"]);
    assert_eq!(s[0]["project"], "/work/ops");
    let s = json(&e, &["sessions", "--agent", "cursor"]);
    assert_eq!(s[0]["started"], 1793757600000i64);
}

#[test]
fn wiki_export_is_idempotent() {
    let e = env();
    let out = e.home.join("wiki");
    let a = json(&e, &["wiki", "export", out.to_str().unwrap()]);
    assert_eq!(a["sessions"], 10);
    assert!(out.join("index.md").exists());
    assert!(out.join("days/2026-11-04.md").exists());
    let b = json(&e, &["wiki", "export", out.to_str().unwrap()]);
    assert_eq!(b["pages_written"], 0);
}

#[test]
fn mcp_server_answers_tool_calls() {
    let e = env();
    agentstory(&e).arg("index").assert().success();
    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_sessions","arguments":{"query":"navbar"}}}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"handoff","arguments":{"id":"cdx-2222"}}}"#,
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"read_session","arguments":{"id":"nope"}}}"#,
    ]
    .join("\n");
    let out = agentstory(&e)
        .arg("mcp")
        .write_stdin(input)
        .output()
        .unwrap();
    let lines: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 5, "notification must not get a reply");
    assert_eq!(lines[0]["result"]["serverInfo"]["name"], "agentstory");
    assert_eq!(lines[1]["result"]["tools"].as_array().unwrap().len(), 4);
    assert!(
        lines[2]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("opencode:ses_oc6666")
    );
    assert!(
        lines[3]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("rate limiter")
    );
    assert_eq!(lines[4]["result"]["isError"], true);
}

// ---------------------------------------------------------------- agent space

fn run_in(e: &Env, dir: &Path, args: &[&str]) -> std::process::Output {
    agentstory(e).current_dir(dir).args(args).output().unwrap()
}

fn ok(o: &std::process::Output) -> String {
    assert!(
        o.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&o.stderr),
        String::from_utf8_lossy(&o.stdout)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn init_creates_space_and_is_idempotent() {
    let e = env();
    let sp = e.home.join("proj");
    std::fs::create_dir_all(sp.join(".claude")).unwrap();
    std::fs::write(
        sp.join(".claude/settings.json"),
        r#"{"model":"x","hooks":{"Stop":[{"hooks":[]}]}}"#,
    )
    .unwrap();
    std::fs::write(sp.join("CLAUDE.md"), "existing rules").unwrap();
    ok(&run_in(
        &e,
        &sp,
        &["init", "--lang", "zh", "--name", "测试空间"],
    ));
    for f in [
        ".agentstory.toml",
        "AGENTS.md",
        "memory/USER.md",
        "memory/NOW.md",
        "memory/MEMORY.md",
        "wiki/index.md",
        "mistakes/index.md",
        ".cursor/rules/agent-space.mdc",
    ] {
        assert!(sp.join(f).exists(), "missing {f}");
    }
    assert!(
        std::fs::read_to_string(sp.join("AGENTS.md"))
            .unwrap()
            .contains("测试空间")
    );
    assert_eq!(
        std::fs::read_to_string(sp.join("CLAUDE.md")).unwrap(),
        "existing rules",
        "existing file must be kept"
    );
    assert!(
        sp.join("GEMINI.md")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(sp.join(".claude/settings.json")).unwrap())
            .unwrap();
    assert_eq!(settings["model"], "x");
    assert!(settings["hooks"]["Stop"].is_array());
    assert!(
        settings["hooks"]["UserPromptSubmit"]
            .to_string()
            .contains("agentstory hook nudge")
    );
    // Second run: nothing created, hook not duplicated.
    let again: Value = serde_json::from_str(&ok(&run_in(&e, &sp, &["--json", "init"]))).unwrap();
    assert!(again["created"].as_array().unwrap().is_empty(), "{again}");
    let s2 = std::fs::read_to_string(sp.join(".claude/settings.json")).unwrap();
    assert_eq!(s2.matches("agentstory hook nudge").count(), 1);
}

#[test]
fn memory_entries_index_supersede_and_doctor() {
    let e = env();
    let sp = e.home.join("space");
    ok(&run_in(&e, &e.home, &["init", sp.to_str().unwrap()]));
    ok(&run_in(
        &e,
        &sp,
        &[
            "mem",
            "add",
            "feedback",
            "Small batches",
            "-d",
            "paid generation: at most 3 per batch",
            "--body",
            "He asked why 20 at once.",
        ],
    ));
    ok(&run_in(
        &e,
        &sp,
        &[
            "mem",
            "add",
            "project",
            "pregnancy",
            "-d",
            "due date and appointments",
            "--private",
            "--body",
            "-",
        ],
    ));
    let idx = std::fs::read_to_string(sp.join("memory/MEMORY.md")).unwrap();
    assert!(
        idx.contains("[small-batches](feedback/small-batches.md)"),
        "{idx}"
    );
    assert!(idx.contains("pregnancy") && idx.contains("🔒"));
    // Duplicate slug is refused unless --update.
    assert!(
        !run_in(
            &e,
            &sp,
            &["mem", "add", "feedback", "small-batches", "-d", "x"]
        )
        .status
        .success()
    );
    ok(&run_in(
        &e,
        &sp,
        &[
            "mem",
            "add",
            "feedback",
            "batch-of-two",
            "-d",
            "now at most 2 per batch",
        ],
    ));
    ok(&run_in(
        &e,
        &sp,
        &["mem", "supersede", "small-batches", "batch-of-two"],
    ));
    let old = std::fs::read_to_string(sp.join("memory/feedback/small-batches.md")).unwrap();
    assert!(old.contains("status: superseded") && old.contains("superseded_by: batch-of-two"));
    assert!(
        !std::fs::read_to_string(sp.join("memory/MEMORY.md"))
            .unwrap()
            .contains("small-batches")
    );
    let d: Value = serde_json::from_str(&ok(&run_in(&e, &sp, &["--json", "doctor"]))).unwrap();
    assert!(d["problems"].as_array().unwrap().is_empty(), "{d}");
    // Plant problems: a leaked key, a broken link, an oversized NOW.md.
    std::fs::write(
        sp.join("wiki/leak.md"),
        "token=abcdef1234567890 see [[nowhere]]",
    )
    .unwrap();
    std::fs::write(sp.join("memory/NOW.md"), "x".repeat(2500)).unwrap();
    let o = run_in(&e, &sp, &["--json", "doctor"]);
    assert_eq!(o.status.code(), Some(2));
    let d: Value = serde_json::from_slice(&o.stdout).unwrap();
    let all = d.to_string();
    assert!(
        all.contains("leak.md") && all.contains("NOW.md") && all.contains("[[nowhere]]"),
        "{all}"
    );
}

#[test]
fn mistakes_and_wiki_pages() {
    let e = env();
    let sp = e.home.join("space");
    ok(&run_in(&e, &e.home, &["init", sp.to_str().unwrap()]));
    ok(&run_in(
        &e,
        &sp,
        &[
            "mistake",
            "add",
            "Pushed to main",
            "-l",
            "Never push to main; open a branch",
            "-t",
            "git",
        ],
    ));
    ok(&run_in(
        &e,
        &sp,
        &[
            "mistake",
            "add",
            "Title read as death",
            "-l",
            "Write resigned, not gone",
            "-t",
            "#content",
        ],
    ));
    let idx = std::fs::read_to_string(sp.join("mistakes/index.md")).unwrap();
    let a = idx.find("Title read as death").unwrap();
    let b = idx.find("Pushed to main").unwrap();
    assert!(a < b, "newest first:\n{idx}");
    assert!(idx.contains("#git") && idx.contains("#content"));
    ok(&run_in(
        &e,
        &sp,
        &[
            "wiki",
            "add",
            "Release checklist",
            "-s",
            "steps before tagging",
            "--body",
            "1. cargo test",
        ],
    ));
    assert!(sp.join("wiki/release-checklist.md").exists());
    assert!(
        std::fs::read_to_string(sp.join("wiki/index.md"))
            .unwrap()
            .contains("[Release checklist](release-checklist.md)")
    );
}

#[test]
fn search_covers_space_notes_and_sessions() {
    let e = env();
    let sp = e.home.join("space");
    ok(&run_in(&e, &e.home, &["init", sp.to_str().unwrap()]));
    ok(&run_in(
        &e,
        &sp,
        &[
            "mistake",
            "add",
            "按钮颜色返工",
            "-l",
            "改 UI 先截图给用户确认",
            "-t",
            "ui",
        ],
    ));
    let hits: Value =
        serde_json::from_str(&ok(&run_in(&e, &sp, &["--json", "search", "按钮"]))).unwrap();
    let sources: Vec<&str> = hits
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["source"].as_str().unwrap())
        .collect();
    assert!(
        sources.contains(&"note") && sources.contains(&"session"),
        "{hits}"
    );
    let notes: Value = serde_json::from_str(&ok(&run_in(
        &e,
        &sp,
        &["--json", "search", "按钮", "--only", "notes"],
    )))
    .unwrap();
    assert!(
        notes
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["source"] == "note")
    );
    assert_eq!(notes[0]["agent"], "note:mistakes");
}

#[test]
fn corrections_and_digest() {
    let e = env();
    let cs = json(&e, &["corrections"]);
    let said: Vec<&str> = cs
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["said"].as_str().unwrap())
        .collect();
    assert!(said.iter().any(|s| s.contains("别推 main")), "{said:?}");
    assert!(cs[0]["after"].as_str().is_some_and(|a| !a.is_empty()));
    let o = agentstory(&e)
        .args(["digest", "2026-11-04"])
        .output()
        .unwrap();
    let d = String::from_utf8(o.stdout).unwrap();
    for a in [
        "claude-code",
        "codex",
        "gemini",
        "qwen",
        "opencode",
        "hermes",
        "cursor",
        "pi",
    ] {
        assert!(d.contains(&format!("## {a}")), "digest misses {a}");
    }
    assert!(!d.contains("sk-test"));
}

#[test]
fn nudge_hook_fires_every_n_human_prompts() {
    let e = env();
    let state = e.home.join("state");
    let mut fired = 0;
    for i in 0..6 {
        let prompt = if i == 2 {
            "[SYSTEM NOTIFICATION - NOT USER INPUT] done"
        } else {
            "do the thing"
        };
        let o = agentstory(&e)
            .env("XDG_STATE_HOME", &state)
            .env("HOME", &e.home)
            .args(["hook", "nudge", "--every", "2"])
            .write_stdin(format!(r#"{{"session_id":"s-1","prompt":"{prompt}"}}"#))
            .output()
            .unwrap();
        assert!(o.status.success());
        if String::from_utf8_lossy(&o.stdout).contains("additionalContext") {
            fired += 1;
        }
    }
    assert_eq!(fired, 2, "5 human prompts with every=2 → fires at 2 and 4");
}

#[test]
fn dsh_reads_zstd_frames_and_latest_generation() {
    let e = env();
    let hits = json(&e, &["search", "ZSTD_ONLY_MARKER"]);
    assert_eq!(hits[0]["agent"], "dsh", "{hits}");
    let h = json(&e, &["handoff", "session-zstd0001"]);
    assert!(h["latest_summary"].as_str().unwrap().contains("lexer.rs"));
    assert_eq!(h["model"], "deepseek-v4-flash");
    let kinds: Vec<&str> = h["key_actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"push"), "{kinds:?}");
}

#[test]
fn short_ids_skip_generic_prefixes() {
    let e = env();
    // dsh ids start with "session-"; the short form and lookups use what follows.
    let v = json(&e, &["show", "6f1c2a9e", "--last", "1"]);
    assert_eq!(v["id"], "session-6f1c2a9e-3b7d-4e21-9c55-0d8e4f7a1b23");
    let out = String::from_utf8(
        agentstory(&e)
            .args(["sessions", "-a", "dsh"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(
        out.contains("6f1c2a9e") && !out.contains("session-  "),
        "{out}"
    );
}

#[test]
fn search_levels() {
    let e = env();
    // "limiter.rs" appears only as a file the agent wrote (work), "FAILED" only in tool output.
    assert!(
        json(&e, &["search", "limiter.rs", "--level", "chat"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    let work = json(&e, &["search", "limiter.rs", "--level", "work"]);
    assert_eq!(work[0]["matched"], "work", "{work}");
    assert!(work[0]["snippet"].as_str().unwrap().starts_with("[did]"));
    assert!(
        json(&e, &["search", "burst", "FAILED", "--level", "work"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    let full = json(&e, &["search", "burst", "FAILED", "--level", "full"]);
    assert_eq!(full[0]["matched"], "output", "{full}");
    // Default is chat; a chat-level miss escalates to full on its own.
    let auto = json(&e, &["search", "burst", "FAILED"]);
    assert_eq!(auto[0]["matched"], "output");
    // A chat hit stays at chat level and never shows tool output.
    let chat = json(&e, &["search", "rate", "limiter"]);
    assert!(
        chat.as_array()
            .unwrap()
            .iter()
            .all(|h| h["matched"] == "chat"),
        "{chat}"
    );
}
