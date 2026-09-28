use std::borrow::Cow;
use std::path::{Path, PathBuf};

use memchr::memmem::Finder;
use serde::Deserialize;

use super::{
    Agent, Event, Format, Header, Lines, Msg, Role, clean, file_name, finder, is_uuid, lines,
    prompt, timestamp, walk,
};
use crate::env::Env;

pub struct Codex;

impl Agent for Codex {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn color(&self) -> u8 {
        110
    }

    fn roots(&self, env: &Env) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = env
            .var("CODEX_HOME")
            .map(Path::to_owned)
            .into_iter()
            .collect();
        roots.push(env.home.join(".codex"));
        roots
    }

    fn stores(&self, root: &Path) -> Vec<(PathBuf, bool)> {
        [("sessions", false), ("archived_sessions", true)]
            .into_iter()
            .map(|(dir, archived)| (root.join(dir), archived))
            .filter(|(dir, _)| dir.is_dir())
            .collect()
    }

    fn chats(&self, store: &Path, found: &mut dyn FnMut(PathBuf)) {
        walk(store, 3, &mut |path| {
            let name = file_name(&path);
            if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                found(path);
            }
        });
    }

    fn format(&self) -> Format<'_> {
        Format::Lines(self)
    }

    fn title_files(&self, root: &Path) -> Vec<PathBuf> {
        let index = root.join("session_index.jsonl");
        if index.is_file() && !self.stores(root).is_empty() {
            vec![index]
        } else {
            Vec::new()
        }
    }

    fn titles(&self, data: &[u8]) -> Vec<(String, String)> {
        #[derive(Deserialize)]
        struct Entry {
            id: String,
            thread_name: Option<String>,
        }
        lines(data)
            .filter_map(|line| serde_json::from_slice::<Entry>(line).ok())
            .filter_map(|e| Some((e.id, clean(&e.thread_name?)?)))
            .collect()
    }

    fn resume(&self, id: &str, _file: &Path) -> Vec<String> {
        vec!["codex".into(), "resume".into(), id.into()]
    }
}

impl Lines for Codex {
    fn session_id(&self, file: &Path) -> Option<String> {
        let stem = file.file_stem()?.to_str()?;
        let id = stem.get(stem.len().checked_sub(36)?..)?;
        is_uuid(id).then(|| id.to_owned())
    }

    fn header(&self, _file: &Path, data: &[u8]) -> Header {
        lines(data).next().and_then(header).unwrap_or_default()
    }

    fn line(&self, line: &[u8]) -> Option<Event> {
        self::line(line)
    }

    fn repeats_messages(&self) -> bool {
        true
    }
}

/// Codex writes the `type` fields first, so the head is enough to classify a line, and huge
/// lines are skipped unread.
const HEAD: usize = 1024;

finder!(SESSION_META, br#""type":"session_meta""#);
finder!(EVENT_MSG, br#""type":"event_msg""#);
finder!(RESPONSE_ITEM, br#""type":"response_item""#);
finder!(TURN_CONTEXT, br#""type":"turn_context""#);
finder!(USER_MESSAGE, br#""type":"user_message""#);
finder!(AGENT_MESSAGE, br#""type":"agent_message""#);
finder!(ITEM_COMPLETED, br#""type":"item_completed""#);
finder!(MESSAGE, br#""type":"message""#);
finder!(USER_ITEM, br#""item":{"type":"UserMessage""#);
finder!(AGENT_ITEM, br#""item":{"type":"AgentMessage""#);
finder!(ROLE_USER, br#""role":"user""#);
finder!(ROLE_ASSISTANT, br#""role":"assistant""#);

#[derive(Deserialize)]
struct MetaLine {
    payload: MetaPayload,
}

#[derive(Deserialize)]
struct MetaPayload {
    #[serde(default)]
    id: String,
    #[serde(default)]
    cwd: String,
    source: Option<serde_json::Value>,
    thread_source: Option<String>,
    timestamp: Option<String>,
}

#[derive(Deserialize)]
struct TurnLine {
    payload: Turn,
}

#[derive(Deserialize)]
struct Turn {
    model: Option<String>,
}

#[derive(Deserialize)]
struct EventLine<'a> {
    #[serde(borrow)]
    payload: EventPayload<'a>,
}

#[derive(Deserialize)]
struct EventPayload<'a> {
    #[serde(borrow)]
    message: Option<Cow<'a, str>>,
    #[serde(borrow)]
    item: Option<Parts<'a>>,
}

#[derive(Deserialize)]
struct ResponseLine<'a> {
    #[serde(borrow)]
    payload: Parts<'a>,
}

#[derive(Deserialize)]
struct Parts<'a> {
    #[serde(borrow, default)]
    content: Vec<Part<'a>>,
}

#[derive(Deserialize)]
struct Part<'a> {
    #[serde(borrow)]
    text: Option<Cow<'a, str>>,
}

fn line(b: &[u8]) -> Option<Event> {
    let head = &b[..b.len().min(HEAD)];
    let has = |f: &Finder| f.find(head).is_some();
    if has(&SESSION_META) {
        None // read by `header`
    } else if has(&TURN_CONTEXT) {
        let TurnLine { payload } = serde_json::from_slice(b).ok()?;
        payload.model.filter(|m| !m.is_empty()).map(Event::Model)
    } else if has(&EVENT_MSG) {
        if has(&USER_MESSAGE) {
            event(b, Role::User)
        } else if has(&AGENT_MESSAGE) {
            event(b, Role::Assistant)
        } else if has(&ITEM_COMPLETED) && has(&USER_ITEM) {
            item(b, Role::User)
        } else if has(&ITEM_COMPLETED) && has(&AGENT_ITEM) {
            item(b, Role::Assistant)
        } else {
            None
        }
    } else if has(&RESPONSE_ITEM) && has(&MESSAGE) {
        if has(&ROLE_USER) {
            response(b, Role::User)
        } else if has(&ROLE_ASSISTANT) {
            response(b, Role::Assistant)
        } else {
            None
        }
    } else {
        None
    }
}

fn header(b: &[u8]) -> Option<Header> {
    SESSION_META.find(&b[..b.len().min(HEAD)])?;
    let MetaLine { payload: p } = serde_json::from_slice(b).ok()?;
    let hidden = p.source.as_ref().is_some_and(|s| s.is_object())
        || matches!(
            p.thread_source.as_deref(),
            Some("subagent" | "guardian_review")
        );
    Some(Header {
        id: Some(p.id).filter(|id| !id.is_empty()),
        cwd: Some(p.cwd).filter(|cwd| !cwd.is_empty()),
        started: p.timestamp.as_deref().and_then(timestamp),
        hidden,
        ..Header::default()
    })
}

fn event(b: &[u8], role: Role) -> Option<Event> {
    let l: EventLine = serde_json::from_slice(b).ok()?;
    message(role, &l.payload.message?)
}

fn item(b: &[u8], role: Role) -> Option<Event> {
    let l: EventLine = serde_json::from_slice(b).ok()?;
    message(role, &join(&l.payload.item?))
}

fn response(b: &[u8], role: Role) -> Option<Event> {
    let l: ResponseLine = serde_json::from_slice(b).ok()?;
    message(role, &join(&l.payload))
}

fn join(parts: &Parts) -> String {
    let texts: Vec<&str> = parts
        .content
        .iter()
        .filter_map(|p| p.text.as_deref())
        .collect();
    texts.join("\n")
}

fn message(role: Role, text: &str) -> Option<Event> {
    let text = match role {
        // Codex sends the project's AGENTS.md as a prompt.
        Role::User
            if text
                .trim_start()
                .starts_with("# AGENTS.md instructions for ") =>
        {
            return None;
        }
        Role::User => prompt(text)?,
        Role::Assistant => clean(text)?,
    };
    Some(Event::Msg(Msg::new(role, text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(line: &str) -> Option<(Role, String)> {
        match super::line(line.as_bytes()) {
            Some(Event::Msg(m)) => Some((m.role, m.text)),
            _ => None,
        }
    }

    #[test]
    fn model_per_turn() {
        let turn = r#"{"timestamp":"t","type":"turn_context","payload":{"turn_id":"x","cwd":"/x","model":"gpt-6-astra","effort":"xhigh"}}"#;
        assert!(
            matches!(super::line(turn.as_bytes()), Some(Event::Model(m)) if m == "gpt-6-astra")
        );
    }

    #[test]
    fn all_message_shapes() {
        let cases = [
            (
                r#"{"timestamp":"t","type":"event_msg","payload":{"type":"user_message","message":"hi there","images":[]}}"#,
                Role::User,
                "hi there",
            ),
            (
                r#"{"timestamp":"t","type":"event_msg","payload":{"type":"agent_message","message":"hello"}}"#,
                Role::Assistant,
                "hello",
            ),
            (
                r#"{"timestamp":"t","ordinal":9,"type":"event_msg","payload":{"type":"item_completed","thread_id":"x","turn_id":"y","item":{"type":"UserMessage","id":"i","content":[{"type":"text","text":"new style","text_elements":[]}]}}}"#,
                Role::User,
                "new style",
            ),
            (
                r#"{"timestamp":"t","ordinal":10,"type":"event_msg","payload":{"type":"item_completed","thread_id":"x","turn_id":"y","item":{"type":"AgentMessage","id":"i","content":[{"type":"Text","text":"reply"}]}}}"#,
                Role::Assistant,
                "reply",
            ),
            (
                r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"old style"}]}}"#,
                Role::User,
                "old style",
            ),
            (
                r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","id":"m","role":"assistant","content":[{"type":"output_text","text":"old reply"}]}}"#,
                Role::Assistant,
                "old reply",
            ),
        ];
        for (line, role, text) in cases {
            assert_eq!(msg(line), Some((role, text.to_owned())), "{line}");
        }
    }

    #[test]
    fn skips_noise() {
        let lines = [
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions>"}]}}"#,
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n<cwd>/x</cwd>\n</environment_context>"}]}}"#,
            r##"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /x\n\n<INSTRUCTIONS>"}]}}"##,
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{}"}}"#,
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","output":"lots"}}}"#,
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"token_count","info":{}}}"#,
            r#"{"timestamp":"t","type":"compacted","payload":{"message":"","replacement_history":[{"type":"message","role":"user","content":[{"type":"input_text","text":"repeat"}]}]}}"#,
            r#"{"timestamp":"t","type":"turn_context","payload":{"cwd":"/x"}}"#,
        ];
        for line in lines {
            assert!(super::line(line.as_bytes()).is_none(), "{line}");
        }
    }

    #[test]
    fn session_header() {
        let visible = r#"{"timestamp":"t","type":"session_meta","payload":{"id":"abc","cwd":"/Users/a/shop","originator":"codex_cli_rs","source":"cli","base_instructions":{"text":"long"}}}"#;
        let h = header(visible.as_bytes()).unwrap();
        assert_eq!(
            (h.id.as_deref(), h.cwd.as_deref(), h.hidden),
            (Some("abc"), Some("/Users/a/shop"), false)
        );
        assert!(
            super::line(visible.as_bytes()).is_none(),
            "headers are read by `header`"
        );

        let subagent = r#"{"timestamp":"t","type":"session_meta","payload":{"id":"s","cwd":"/x","source":{"subagent":{"other":"guardian"}},"thread_source":"guardian_review"}}"#;
        assert!(header(subagent.as_bytes()).unwrap().hidden);

        let old = r#"{"timestamp":"t","type":"session_meta","payload":{"id":"o","timestamp":"t","cwd":"/x","originator":"codex_cli_rs","cli_version":"0.47.0","instructions":"…"}}"#;
        assert!(!header(old.as_bytes()).unwrap().hidden);
        assert!(header(br#"{"timestamp":"t","type":"event_msg","payload":{}}"#).is_none());
    }

    #[test]
    fn ids_come_from_file_names() {
        let p =
            Path::new("/x/rollout-2026-01-02T03-04-05-0192a3b4-c5d6-7e8f-9a0b-1c2d3e4f5a6b.jsonl");
        assert_eq!(
            Codex.session_id(p).as_deref(),
            Some("0192a3b4-c5d6-7e8f-9a0b-1c2d3e4f5a6b")
        );
        assert_eq!(Codex.session_id(Path::new("/x/rollout-bad.jsonl")), None);
    }

    #[test]
    fn instructions_are_not_prompts() {
        let agents = r##"{"timestamp":"t","type":"event_msg","payload":{"type":"user_message","message":"# AGENTS.md instructions for /x\n\n<INSTRUCTIONS>…"}}"##;
        assert!(super::line(agents.as_bytes()).is_none());
    }

    #[test]
    fn thread_names() {
        let data = b"{\"id\":\"a\",\"thread_name\":\"First\"}\n{\"id\":\"b\"}\n{\"id\":\"a\",\"thread_name\":\"Renamed\"}\n";
        assert_eq!(
            Codex.titles(data),
            [("a".into(), "First".into()), ("a".into(), "Renamed".into())]
        );
    }
}
