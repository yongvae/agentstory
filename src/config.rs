//! Optional user configuration, for transcripts that pass through a chat bridge or other harness
//! that wraps messages in its own markup. Nothing here is needed for plain agent sessions.
//!
//! Location: `$AGENTSTORY_CONFIG`, else `~/.config/agentstory/config.toml`.
//!
//! ```toml
//! [transcript]
//! # Elements whose whole content is context, not the user's words. A trailing * matches a prefix.
//! drop_tags = ["router_*", "roster"]
//! # If a message contains <tag>…</tag>, only that part is what the person said.
//! message_tags = ["chat_message"]
//! # Element naming the speaker, e.g. <from name="Alice"/>.
//! speaker_tag = "from"
//! speaker_attr = "name"
//! # Regexes whose first group is a speaker name (e.g. a request relayed by another bot).
//! speaker_patterns = ['\[relayed by (\S+)\]']
//! # Regexes removed from the user's text (notes the bridge prepends).
//! strip_patterns = ['\[note from the bridge[^\]]*\]']
//! # Shell commands whose heredoc or quoted argument is the agent's reply to a chat.
//! relay_commands = ["chatctl send"]
//! # Assistant texts that mean "nothing to say".
//! silent_replies = ["NO_REPLY"]
//!
//! [index]
//! # Skip tool output for a much smaller index (search levels chat and work only).
//! store_output = true
//!
//! [handoff.actions]
//! # Extra side-effect patterns for handoff cards: kind = regex with one capture group.
//! message = '(\bchatctl\s+send\b[^\n;&|]{0,120})'
//! ```

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::LazyLock;

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub transcript: Transcript,
    pub handoff: Handoff,
    pub index: IndexOptions,
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(default)]
pub struct IndexOptions {
    /// Index what tools printed (default true). `false` gives a much smaller index that only
    /// supports `--level chat` and `--level work`.
    pub store_output: Option<bool>,
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(default)]
pub struct Transcript {
    pub drop_tags: Vec<String>,
    pub message_tags: Vec<String>,
    pub speaker_tag: Option<String>,
    pub speaker_attr: Option<String>,
    pub speaker_patterns: Vec<String>,
    pub strip_patterns: Vec<String>,
    pub relay_commands: Vec<String>,
    pub silent_replies: Vec<String>,
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(default)]
pub struct Handoff {
    pub actions: BTreeMap<String, String>,
}

pub fn path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("AGENTSTORY_CONFIG") {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir().map(|h| h.join(".config/agentstory/config.toml"))
}

pub fn load() -> Config {
    let Some(p) = path() else {
        return Config::default();
    };
    let Ok(text) = std::fs::read_to_string(&p) else {
        return Config::default();
    };
    match toml::from_str(&text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("agentstory: ignoring {}: {e}", p.display());
            Config::default()
        }
    }
}

pub static CONFIG: LazyLock<Config> = LazyLock::new(load);
