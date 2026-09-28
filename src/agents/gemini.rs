use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};

use memchr::memmem::Finder;
use serde::Deserialize;
use serde_json::value::RawValue;

use super::{
    Agent, Event, Format, Header, Lines, Msg, Role, clean, file_name, finder, lines, prompt,
    text_of, timestamp, walk,
};
use crate::env::Env;

finder!(USER, br#""type":"user""#);
finder!(GEMINI, br#""type":"gemini""#);
finder!(SET, br#""$set""#);

pub struct Gemini;

impl Agent for Gemini {
    fn name(&self) -> &'static str {
        "gemini"
    }

    fn color(&self) -> u8 {
        69
    }

    fn roots(&self, env: &Env) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = env
            .var("GEMINI_CLI_HOME")
            .map(|home| home.join(".gemini"))
            .into_iter()
            .collect();
        roots.push(env.home.join(".gemini"));
        // Where chats go under the macOS sandbox.
        roots.push(env.home.join(".cache/.gemini"));
        roots
    }

    fn stores(&self, root: &Path) -> Vec<(PathBuf, bool)> {
        let tmp = root.join("tmp");
        if tmp.is_dir() && file_name(root) == ".gemini" {
            vec![(tmp, false)]
        } else {
            Vec::new()
        }
    }

    /// `<project>/chats/session-*.jsonl`. Subagents' chats are a level deeper.
    fn chats(&self, store: &Path, found: &mut dyn FnMut(PathBuf)) {
        walk(store, 2, &mut |path| {
            let name = file_name(&path);
            let in_chats = path.parent().is_some_and(|p| file_name(p) == "chats");
            if in_chats && name.starts_with("session-") && name.ends_with(".jsonl") {
                found(path);
            }
        });
    }

    fn format(&self) -> Format<'_> {
        Format::Lines(self)
    }

    fn resume(&self, id: &str, _file: &Path) -> Vec<String> {
        vec!["gemini".into(), "--resume".into(), id.into()]
    }
}

impl Lines for Gemini {
    /// File names hold only the start of the id; the header has all of it.
    fn session_id(&self, _file: &Path) -> Option<String> {
        None
    }

    /// The header line has the session's id and start; the project's folder is recorded next
    /// to its `chats` folder.
    fn header(&self, file: &Path, data: &[u8]) -> Header {
        let Some(meta) = lines(data)
            .next()
            .and_then(|l| serde_json::from_slice::<Meta>(l).ok())
        else {
            return Header::default();
        };
        let cwd = file
            .parent()
            .and_then(Path::parent)
            .and_then(|project| fs::read_to_string(project.join(".project_root")).ok());
        Header {
            id: Some(meta.session_id),
            cwd: cwd.map(|root| root.trim().to_owned()),
            started: meta.start_time.as_deref().and_then(timestamp),
            hidden: meta.kind.as_deref() == Some("subagent"),
            ..Header::default()
        }
    }

    fn line(&self, b: &[u8]) -> Option<Event> {
        let has = |f: &Finder| f.find(b).is_some();
        if has(&SET) {
            let l: Set = serde_json::from_slice(b).ok()?;
            return clean(&l.set?.summary?).map(Event::AutoTitle);
        }
        if !(has(&USER) || has(&GEMINI)) {
            return None;
        }
        let l: Record = serde_json::from_slice(b).ok()?;
        let role = match &*l.kind {
            "user" => Role::User,
            "gemini" => Role::Assistant,
            _ => return None,
        };
        // A prompt as typed, before `@file` references were replaced by the files.
        let typed = match role {
            Role::User => l.display_content.and_then(text_of),
            Role::Assistant => None,
        };
        let text = typed.or_else(|| l.content.and_then(text_of))?;
        let text = match role {
            Role::User if text.starts_with(['/', '?']) => return None,
            Role::User => prompt(&text)?,
            Role::Assistant => clean(&text)?,
        };
        Some(Event::Msg(Msg {
            role,
            text,
            at: l.timestamp.as_deref().and_then(timestamp),
            model: l.model.map(Cow::into_owned),
            folder: None,
        }))
    }

    fn repeats_messages(&self) -> bool {
        true
    }
}

#[derive(Deserialize)]
struct Meta {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "startTime")]
    start_time: Option<String>,
    kind: Option<String>,
}

#[derive(Deserialize)]
struct Record<'a> {
    #[serde(rename = "type", borrow)]
    kind: Cow<'a, str>,
    #[serde(borrow)]
    timestamp: Option<Cow<'a, str>>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
    #[serde(rename = "displayContent", borrow)]
    display_content: Option<&'a RawValue>,
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct Set {
    #[serde(rename = "$set")]
    set: Option<Summary>,
}

#[derive(Deserialize)]
struct Summary {
    summary: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(line: &str) -> Option<(Role, String)> {
        match Gemini.line(line.as_bytes()) {
            Some(Event::Msg(m)) => Some((m.role, m.text)),
            _ => None,
        }
    }

    #[test]
    fn records() {
        let user = r#"{"id":"m1","timestamp":"2026-01-02T03:04:05Z","type":"user","content":[{"text":"fix build"}]}"#;
        assert_eq!(msg(user), Some((Role::User, "fix build".into())));
        let reply = r#"{"id":"m2","timestamp":"t","type":"gemini","content":"Fixed.","model":"gemini-3-pro","thoughts":[],"toolCalls":[]}"#;
        assert_eq!(msg(reply), Some((Role::Assistant, "Fixed.".into())));
        let tool_result =
            r#"{"id":"m3","type":"user","content":[{"functionResponse":{"name":"shell"}}]}"#;
        assert_eq!(msg(tool_result), None);
        let command = r#"{"id":"m4","type":"user","content":[{"text":"/chat save"}]}"#;
        assert_eq!(msg(command), None);
        let context = r#"{"id":"m5","type":"user","content":[{"text":"<session_context>cwd</session_context>"}]}"#;
        assert_eq!(msg(context), None);
        let file = r#"{"id":"m6","type":"user","content":[{"text":"explain @a.rs"},{"text":"--- Content from referenced files ---"}],"displayContent":[{"text":"explain @a.rs"}]}"#;
        assert_eq!(msg(file), Some((Role::User, "explain @a.rs".into())));
        let nested = r#"{"id":"m7","type":"gemini","content":"Ran it.","toolCalls":[{"result":[{"type":"user"}]}]}"#;
        assert_eq!(msg(nested), Some((Role::Assistant, "Ran it.".into())));
    }

    #[test]
    fn summaries_and_history_rewrites() {
        let summary = r#"{"$set":{"summary":"Fix build"}}"#;
        assert!(
            matches!(Gemini.line(summary.as_bytes()), Some(Event::AutoTitle(t)) if t == "Fix build")
        );
        let rewrite =
            r#"{"$set":{"messages":[{"id":"m1","type":"user","content":[{"text":"again"}]}]}}"#;
        assert!(Gemini.line(rewrite.as_bytes()).is_none());
        assert!(Gemini.line(br#"{"$rewindTo":"m1"}"#).is_none());
    }

    #[test]
    fn header_and_project_folder() {
        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("shop/chats");
        fs::create_dir_all(&chats).unwrap();
        fs::write(dir.path().join("shop/.project_root"), "/home/a/shop\n").unwrap();
        let path = chats.join("session-2026-01-02T03-04-3f2a9b1c.jsonl");
        let header = br#"{"sessionId":"3f2a9b1c-0000-4000-8000-000000000000","projectHash":"ab","startTime":"2026-01-02T03:04:05Z","lastUpdated":"t","kind":"main"}"#;
        let h = Gemini.header(&path, header);
        assert_eq!(h.cwd.as_deref(), Some("/home/a/shop"));
        assert_eq!(
            h.id.as_deref(),
            Some("3f2a9b1c-0000-4000-8000-000000000000")
        );
        assert_eq!((h.hidden, h.started), (false, Some(1_767_323_045)));
    }
}
