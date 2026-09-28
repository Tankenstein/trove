use std::borrow::Cow;
use std::path::{Path, PathBuf};

use memchr::memmem::Finder;
use serde::Deserialize;
use serde_json::value::RawValue;

use super::{
    Agent, Event, Format, Header, Lines, Msg, Role, clean, file_name, finder, lines, prompt,
    short_model, text_of, timestamp, walk,
};
use crate::env::Env;

finder!(MESSAGE, br#""type":"message""#);
finder!(SESSION_INFO, br#""type":"session_info""#);
finder!(MODEL_CHANGE, br#""type":"model_change""#);

pub struct Pi;

impl Agent for Pi {
    fn name(&self) -> &'static str {
        "pi"
    }

    fn color(&self) -> u8 {
        141
    }

    fn roots(&self, env: &Env) -> Vec<PathBuf> {
        let dir = match env.var("PI_CODING_AGENT_DIR") {
            Some(dir) => match dir.strip_prefix("~") {
                Ok(rest) => env.home.join(rest),
                Err(_) => dir.to_owned(),
            },
            None => env.home.join(".pi/agent"),
        };
        vec![dir]
    }

    fn stores(&self, root: &Path) -> Vec<(PathBuf, bool)> {
        let sessions = root.join("sessions");
        if is_pi_store(&sessions) {
            vec![(sessions, false)]
        } else {
            Vec::new()
        }
    }

    fn chats(&self, store: &Path, found: &mut dyn FnMut(PathBuf)) {
        walk(store, 1, &mut |path| {
            if session_id(&path).is_some() {
                found(path);
            }
        });
    }

    fn format(&self) -> Format<'_> {
        Format::Lines(self)
    }

    fn model_label(&self, model: &str) -> String {
        short_model(model)
    }

    fn resume(&self, _id: &str, file: &Path) -> Vec<String> {
        vec![
            "pi".into(),
            "--session".into(),
            file.to_string_lossy().into_owned(),
        ]
    }
}

impl Lines for Pi {
    fn session_id(&self, file: &Path) -> Option<String> {
        session_id(file)
    }

    fn header(&self, _file: &Path, data: &[u8]) -> Header {
        lines(data)
            .next()
            .and_then(|l| serde_json::from_slice::<Session>(l).ok())
            .filter(|s| s.kind == "session")
            .map_or_else(Header::default, |s| Header {
                cwd: s.cwd,
                started: s.timestamp.as_deref().and_then(timestamp),
                ..Header::default()
            })
    }

    fn line(&self, b: &[u8]) -> Option<Event> {
        let has = |f: &Finder| f.find(b).is_some();
        if !(has(&MESSAGE) || has(&SESSION_INFO) || has(&MODEL_CHANGE)) {
            return None;
        }
        let l: Line = serde_json::from_slice(b).ok()?;
        match &*l.kind {
            "message" => {
                let message = l.message?;
                let role = match &*message.role {
                    "user" => Role::User,
                    "assistant" => Role::Assistant,
                    _ => return None,
                };
                let text = text_of(message.content?)?;
                let text = match role {
                    Role::User => prompt(&typed(&text)?)?,
                    Role::Assistant => clean(&text)?,
                };
                Some(Event::Msg(Msg {
                    role,
                    text,
                    at: l.timestamp.as_deref().and_then(timestamp),
                    model: message.model.map(Cow::into_owned),
                    folder: None,
                }))
            }
            "session_info" => clean(&l.name?).map(Event::Title),
            "model_change" => clean(&l.model_id?).map(Event::Model),
            _ => None,
        }
    }
}

/// A prompt as typed: Pi replaces `/skill:name args` with the skill's text followed by the args,
/// and puts the contents of `@file` references before the prompt.
fn typed(text: &str) -> Option<String> {
    if let Some(rest) = text.strip_prefix("<skill name=\"")
        && let Some((name, _)) = rest.split_once('"')
        && let Some((_, args)) = rest.split_once("\n</skill>")
    {
        let args = args.trim();
        return (!args.is_empty()).then(|| format!("/skill:{name} {args}"));
    }
    let mut words = Vec::new();
    let mut rest = text;
    while let Some(after) = rest.strip_prefix("<file name=\"")
        && let Some((name, _)) = after.split_once('"')
        && let Some((_, next)) = after.split_once("</file>")
    {
        words.push(format!("@{name}"));
        rest = next.trim_start();
    }
    words.push(rest.to_owned());
    Some(words.join(" "))
}

/// Pi's `sessions` holds `--<encoded cwd>--` folders, or session files directly when the folder
/// is set by hand. Other tools have `sessions` folders too.
fn is_pi_store(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        let name = e.file_name();
        let name = name.to_string_lossy();
        name.starts_with("--") && name.ends_with("--") || session_id(Path::new(&*name)).is_some()
    })
}

/// Session files are named `<time>_<id>.jsonl`.
fn session_id(path: &Path) -> Option<String> {
    let stem = file_name(path).strip_suffix(".jsonl")?;
    let (time, id) = stem.split_once('_')?;
    (time.starts_with(|c: char| c.is_ascii_digit()) && !id.is_empty()).then(|| id.to_owned())
}

#[derive(Deserialize)]
struct Session {
    #[serde(rename = "type")]
    kind: String,
    cwd: Option<String>,
    timestamp: Option<String>,
}

#[derive(Deserialize)]
struct Line<'a> {
    #[serde(rename = "type", borrow)]
    kind: Cow<'a, str>,
    #[serde(borrow)]
    timestamp: Option<Cow<'a, str>>,
    #[serde(borrow)]
    message: Option<Message<'a>>,
    #[serde(borrow)]
    name: Option<Cow<'a, str>>,
    #[serde(rename = "modelId", borrow)]
    model_id: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct Message<'a> {
    #[serde(borrow)]
    role: Cow<'a, str>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(line: &str) -> Option<(Role, String, Option<String>)> {
        match Pi.line(line.as_bytes()) {
            Some(Event::Msg(m)) => Some((m.role, m.text, m.model)),
            _ => None,
        }
    }

    #[test]
    fn messages() {
        let user = r#"{"type":"message","id":"a1","parentId":null,"timestamp":"2026-01-02T03:04:05.000Z","message":{"role":"user","content":"fix it","timestamp":1767323045000}}"#;
        assert_eq!(msg(user), Some((Role::User, "fix it".into(), None)));
        let reply = r#"{"type":"message","id":"b2","parentId":"a1","timestamp":"t","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"Done."},{"type":"toolCall","name":"bash"}],"provider":"anthropic","model":"claude-sonnet-4-5"}}"#;
        assert_eq!(
            msg(reply),
            Some((
                Role::Assistant,
                "Done.".into(),
                Some("claude-sonnet-4-5".into())
            ))
        );
        let tool = r#"{"type":"message","id":"c3","message":{"role":"toolResult","content":[{"type":"text","text":"output"}]}}"#;
        assert_eq!(msg(tool), None);
    }

    #[test]
    fn prompts_as_typed() {
        let skill = "<skill name=\"review\" location=\"/s/review/SKILL.md\">\nReferences are relative to /s.\n\nRead the diff.\n</skill>\n\nthe auth change";
        assert_eq!(
            typed(skill).as_deref(),
            Some("/skill:review the auth change")
        );
        assert_eq!(typed(&skill[..skill.find("\n\nthe").unwrap()]), None);
        let files = "<file name=\"/a/x.rs\">\nfn x() {}\n</file>\n<file name=\"/a/y.png\"></file>\nwhat do these do";
        assert_eq!(
            typed(files).as_deref(),
            Some("@/a/x.rs @/a/y.png what do these do")
        );
        assert_eq!(
            typed("<file name=\"/a/x.rs\">unclosed").as_deref(),
            Some("<file name=\"/a/x.rs\">unclosed")
        );
    }

    #[test]
    fn names_and_models() {
        let named = r#"{"type":"session_info","id":"d4","name":"Refactor auth"}"#;
        assert!(matches!(Pi.line(named.as_bytes()), Some(Event::Title(t)) if t == "Refactor auth"));
        let changed = r#"{"type":"model_change","id":"e5","provider":"openai","modelId":"gpt-6"}"#;
        assert!(matches!(Pi.line(changed.as_bytes()), Some(Event::Model(m)) if m == "gpt-6"));
    }

    #[test]
    fn header_and_file_names() {
        let header = br#"{"type":"session","version":3,"id":"0199abcd","timestamp":"2026-01-02T03:04:05.000Z","cwd":"/home/a/shop"}"#;
        let h = Pi.header(Path::new("/x"), header);
        assert_eq!(
            (h.cwd.as_deref(), h.started),
            (Some("/home/a/shop"), Some(1_767_323_045))
        );
        let path = Path::new(
            "/h/.pi/agent/sessions/--home-a-shop--/2026-01-02T03-04-05-000Z_0199abcd.jsonl",
        );
        assert_eq!(Pi.session_id(path).as_deref(), Some("0199abcd"));
        assert_eq!(Pi.session_id(Path::new("/x/notes.jsonl")), None);
        assert_eq!(Pi.resume("0199abcd", path)[..2], ["pi", "--session"]);
    }
}
