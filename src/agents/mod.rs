use std::borrow::Cow;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::value::RawValue;

use crate::env::Env;

pub mod claude;
pub mod codex;
pub mod copilot;
pub mod gemini;
pub mod opencode;
pub mod pi;

// To support another agent, implement `Agent` for it and list it here.
pub static AGENTS: &[&dyn Agent] = &[
    &claude::Claude,
    &codex::Codex,
    &opencode::OpenCode,
    &copilot::Copilot,
    &pi::Pi,
    &gemini::Gemini,
];

pub fn by_name(name: &str) -> Option<&'static dyn Agent> {
    AGENTS.iter().copied().find(|a| a.name() == name)
}

pub trait Agent: Sync {
    fn name(&self) -> &'static str;

    fn color(&self) -> u8;

    fn roots(&self, env: &Env) -> Vec<PathBuf>;

    /// Called for every candidate root: returns nothing for directories that aren't this agent's.
    fn stores(&self, root: &Path) -> Vec<(PathBuf, bool)>;

    /// Finds the chats' files in a store: a file per chat, or the databases.
    fn chats(&self, store: &Path, found: &mut dyn FnMut(PathBuf));

    fn format(&self) -> Format<'_>;

    /// Files holding chat titles, read with `titles` whenever they change.
    fn title_files(&self, _root: &Path) -> Vec<PathBuf> {
        Vec::new()
    }

    fn titles(&self, _data: &[u8]) -> Vec<(String, String)> {
        Vec::new()
    }

    fn model_label(&self, model: &str) -> String {
        model.to_owned()
    }

    /// Runs in the chat's folder. `file` is the chat's file, or the database it's in.
    fn resume(&self, id: &str, file: &Path) -> Vec<String>;
}

impl fmt::Debug for dyn Agent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.name())
    }
}

pub enum Format<'a> {
    /// A file of JSON lines per chat, only ever appended to.
    Lines(&'a dyn Lines),
    /// All chats in one SQLite database.
    Database(&'a dyn Database),
}

pub trait Lines: Sync {
    /// The chat's id, if its file name has it.
    fn session_id(&self, file: &Path) -> Option<String>;

    /// Reads what the file says about the chat as a whole, like a header line. Sees each new
    /// block of the file before its lines, starting with the file's start.
    fn header(&self, _file: &Path, _data: &[u8]) -> Header {
        Header::default()
    }

    /// Lines are parsed in parallel, so this can't depend on other lines; see `header`.
    fn line(&self, line: &[u8]) -> Option<Event>;

    /// Whether each message is logged more than once, so repeats should be dropped.
    fn repeats_messages(&self) -> bool {
        false
    }
}

pub trait Database: Sync {
    /// Every chat in a database, with a version that changes whenever the chat does. `None` if
    /// the database can't be read now.
    fn versions(&self, db: &Path) -> Option<Vec<(String, i64)>>;

    fn read(&self, db: &Path, id: &str) -> Option<Chat>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

pub enum Event {
    Msg(Msg),
    Title(String),
    AutoTitle(String),
    Model(String),
}

pub struct Msg {
    pub role: Role,
    pub text: String,
    pub at: Option<i64>,
    pub model: Option<String>,
    /// The folder it was written in, if the chat's header doesn't say.
    pub folder: Option<String>,
}

impl Msg {
    pub fn new(role: Role, text: String) -> Msg {
        Msg {
            role,
            text,
            at: None,
            model: None,
            folder: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct Header {
    pub id: Option<String>,
    pub cwd: Option<String>,
    pub started: Option<i64>,
    pub model: Option<String>,
    pub title: Option<String>,
    pub auto_title: Option<String>,
    /// Chats run by other chats, like subagents' and reviews', aren't listed.
    pub hidden: bool,
}

/// A chat read whole, from a database.
#[derive(Debug, Default)]
pub struct Chat {
    pub cwd: String,
    pub title: Option<String>,
    pub model: Option<String>,
    pub started: Option<i64>,
    pub updated: i64,
    pub archived: bool,
    pub hidden: bool,
    pub messages: Vec<(Role, String)>,
}

/// The most text kept of a message.
pub const MAX_TEXT: usize = 16 * 1024;

macro_rules! finder {
    ($name:ident, $needle:literal) => {
        static $name: std::sync::LazyLock<memchr::memmem::Finder<'static>> =
            std::sync::LazyLock::new(|| memchr::memmem::Finder::new($needle));
    };
}
pub(crate) use finder;

pub fn walk(dir: &Path, depth: usize, found: &mut dyn FnMut(PathBuf)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_dir = match entry.file_type() {
            Ok(t) if t.is_symlink() => path.is_dir(),
            Ok(t) => t.is_dir(),
            Err(_) => false,
        };
        if !is_dir {
            found(path);
        } else if depth > 0 {
            walk(&path, depth - 1, found);
        }
    }
}

pub fn lines(body: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = body;
    std::iter::from_fn(move || {
        while !rest.is_empty() {
            let (line, next) = match memchr::memchr(b'\n', rest) {
                Some(i) => (&rest[..i], &rest[i + 1..]),
                None => (rest, &rest[rest.len()..]),
            };
            rest = next;
            if !line.is_empty() {
                return Some(line);
            }
        }
        None
    })
}

pub fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

pub fn file_name(path: &Path) -> &str {
    path.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

pub fn timestamp(s: &str) -> Option<i64> {
    s.parse::<jiff::Timestamp>().ok().map(|t| t.as_second())
}

#[derive(Deserialize)]
struct Block<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    text: Option<Cow<'a, str>>,
}

/// Message content that's either a string or a list of blocks, as text: the blocks that are
/// text, joined. `None` if there's no text.
pub fn text_of(content: &RawValue) -> Option<String> {
    let raw = content.get();
    if raw.starts_with('"') {
        return serde_json::from_str(raw).ok();
    }
    let blocks: Vec<Block> = serde_json::from_str(raw).ok()?;
    let texts: Vec<&str> = blocks
        .iter()
        .filter(|b| b.kind.as_deref().is_none_or(|k| k == "text"))
        .filter_map(|b| b.text.as_deref())
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

/// Text trimmed and cut to `MAX_TEXT`, or `None` if there's none.
pub fn clean(text: &str) -> Option<String> {
    let t = text.trim();
    (!t.is_empty()).then(|| truncate(t, MAX_TEXT).to_owned())
}

/// A prompt's text, or `None` if the agent wrote it: a message that's only a wrapper tag, like
/// `<environment_context>…</environment_context>`.
pub fn prompt(text: &str) -> Option<String> {
    let t = text.trim();
    if wrapper_tag(t) {
        return None;
    }
    clean(t)
}

fn wrapper_tag(t: &str) -> bool {
    let Some(rest) = t.strip_prefix('<') else {
        return false;
    };
    let Some(end) = rest.find(['>', ' ']) else {
        return false;
    };
    let tag = &rest[..end];
    tag.contains(['_', '-'])
        && tag
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

pub fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// `claude-opus-5-5` → `opus 5.5`, `claude-3-5-sonnet-20241022` → `sonnet 3.5`; other models as
/// they are.
pub fn short_model(model: &str) -> String {
    let model = model.split('[').next().unwrap_or(model);
    let Some(rest) = model.strip_prefix("claude-") else {
        return model.to_owned();
    };
    let (mut names, mut version) = (Vec::new(), Vec::new());
    for part in rest.split(['-', '.']) {
        if !part.chars().all(|c| c.is_ascii_digit()) {
            names.push(part);
        } else if part.len() < 8 {
            version.push(part);
        }
    }
    format!("{} {}", names.join(" "), version.join("."))
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids() {
        assert!(is_uuid("0f1e2d3c-4b5a-4968-8778-a6b5c4d3e2f1"));
        assert!(!is_uuid("0f1e2d3c-4b5a-4968-8778-a6b5c4d3e2f"));
        assert!(!is_uuid("agent-a0123456789abcdef"));
        assert!(!is_uuid("0f1e2d3cx4b5a-4968-8778-a6b5c4d3e2f1"));
    }

    #[test]
    fn names_are_unique_and_found() {
        for agent in AGENTS {
            assert_eq!(by_name(agent.name()).map(|a| a.name()), Some(agent.name()));
        }
        assert!(by_name("nope").is_none());
    }

    #[test]
    fn content_text() {
        let raw = |s: &str| serde_json::from_str::<Box<RawValue>>(s).unwrap();
        assert_eq!(text_of(&raw(r#""plain""#)).as_deref(), Some("plain"));
        let blocks = r#"[{"type":"text","text":"a"},{"type":"tool_use","name":"x"},{"text":"b"},{"type":"thinking","thinking":"no"}]"#;
        assert_eq!(text_of(&raw(blocks)).as_deref(), Some("a\nb"));
        assert_eq!(text_of(&raw(r#"[{"functionResponse":{}}]"#)), None);
    }

    #[test]
    fn prompts() {
        assert_eq!(prompt("  fix the bug \n").as_deref(), Some("fix the bug"));
        assert_eq!(
            prompt("<environment_context>\n<cwd>/x</cwd>\n</environment_context>"),
            None
        );
        assert_eq!(
            prompt("<local-command-stdout>ok</local-command-stdout>"),
            None
        );
        assert_eq!(prompt("<task-notification> <task-id>1</task-id>"), None);
        assert_eq!(
            prompt("<div>html is fine</div>").as_deref(),
            Some("<div>html is fine</div>")
        );
        assert_eq!(prompt(" \n "), None);
    }

    #[test]
    fn caps_long_text_on_char_boundary() {
        let text = "é".repeat(MAX_TEXT);
        let out = clean(&text).unwrap();
        assert!(out.len() <= MAX_TEXT);
        assert!(out.chars().all(|c| c == 'é'));
    }

    #[test]
    fn lines_skip_empty() {
        let got: Vec<&[u8]> = lines(b"a\n\nb\nc").collect();
        assert_eq!(got, [b"a" as &[u8], b"b", b"c"]);
    }

    #[test]
    fn model_names() {
        assert_eq!(short_model("claude-opus-5-5"), "opus 5.5");
        assert_eq!(short_model("claude-fable-5-1"), "fable 5.1");
        assert_eq!(short_model("claude-opus-5"), "opus 5");
        assert_eq!(short_model("claude-haiku-4-5-20251001"), "haiku 4.5");
        assert_eq!(short_model("claude-3-5-sonnet-20241022"), "sonnet 3.5");
        assert_eq!(short_model("claude-sonnet-4.5"), "sonnet 4.5");
        assert_eq!(short_model("claude-opus-4-7[1m]"), "opus 4.7");
        assert_eq!(short_model("gpt-6-astra"), "gpt-6-astra");
    }
}
