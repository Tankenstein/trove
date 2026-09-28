use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};

use memchr::memmem::Finder;
use serde::Deserialize;
use serde_json::value::RawValue;

use super::{
    Agent, Event, Format, Header, Lines, Msg, Role, clean, file_name, finder, lines, prompt,
    short_model, text_of, timestamp, walk,
};
use crate::env::Env;

finder!(USER_MESSAGE, br#""type":"user.message""#);
finder!(ASSISTANT_MESSAGE, br#""type":"assistant.message""#);
finder!(MODEL_CHANGE, br#""type":"session.model_change""#);

pub struct Copilot;

impl Agent for Copilot {
    fn name(&self) -> &'static str {
        "copilot"
    }

    fn color(&self) -> u8 {
        72
    }

    fn roots(&self, env: &Env) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = env
            .var("COPILOT_HOME")
            .map(Path::to_owned)
            .into_iter()
            .collect();
        roots.push(env.home.join(".copilot"));
        roots
    }

    fn stores(&self, root: &Path) -> Vec<(PathBuf, bool)> {
        let sessions = root.join("session-state");
        if sessions.is_dir() {
            vec![(sessions, false)]
        } else {
            Vec::new()
        }
    }

    fn chats(&self, store: &Path, found: &mut dyn FnMut(PathBuf)) {
        walk(store, 1, &mut |path| {
            if file_name(&path) == "events.jsonl" && path.parent().is_some_and(|p| p != store) {
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

    fn resume(&self, id: &str, _file: &Path) -> Vec<String> {
        vec!["copilot".into(), format!("--resume={id}")]
    }
}

impl Lines for Copilot {
    fn session_id(&self, file: &Path) -> Option<String> {
        Some(file_name(file.parent()?).to_owned()).filter(|id| !id.is_empty())
    }

    /// The first event starts the session, with its folder and model. `workspace.yaml` has the
    /// title, and the folder as it is now, which is where Copilot resumes.
    fn header(&self, file: &Path, data: &[u8]) -> Header {
        let mut header = lines(data).next().and_then(start).unwrap_or_default();
        if let Some(dir) = file.parent()
            && let Ok(yaml) = fs::read_to_string(dir.join("workspace.yaml"))
        {
            header.title = yaml_value(&yaml, "name");
            header.auto_title = yaml_value(&yaml, "summary");
            header.cwd = yaml_value(&yaml, "cwd").or(header.cwd);
        }
        header
    }

    fn line(&self, b: &[u8]) -> Option<Event> {
        let has = |f: &Finder| f.find(b).is_some();
        if !(has(&USER_MESSAGE) || has(&ASSISTANT_MESSAGE) || has(&MODEL_CHANGE)) {
            return None;
        }
        let l: Line = serde_json::from_slice(b).ok()?;
        let data = l.data?;
        // Sub-agents log to the same file.
        let from_agent = [&l.agent_id, &data.agent_id, &data.parent_tool_call_id]
            .into_iter()
            .any(|id| id.as_deref().is_some_and(|id| !id.is_empty()));
        if from_agent {
            return None;
        }
        let role = match &*l.kind {
            "user.message" if data.source.as_deref().is_none_or(typed) => Role::User,
            "assistant.message" => Role::Assistant,
            "session.model_change" => return clean(&data.new_model?).map(Event::Model),
            _ => return None,
        };
        let text = text_of(data.content?)?;
        let text = match role {
            Role::User => prompt(&text)?,
            Role::Assistant => clean(&text)?,
        };
        Some(Event::Msg(Msg {
            role,
            text,
            at: l.timestamp.as_deref().and_then(timestamp),
            model: data.model.map(Cow::into_owned),
            folder: None,
        }))
    }
}

fn start(line: &[u8]) -> Option<Header> {
    let start: Start = serde_json::from_slice(line).ok()?;
    (start.kind == "session.start").then(|| Header {
        cwd: start.data.context.and_then(|c| c.cwd),
        started: start.timestamp.as_deref().and_then(timestamp),
        model: start.data.selected_model,
        ..Header::default()
    })
}

/// Whether a user message was typed: Copilot also sends skills and other agents' messages as the
/// user's.
fn typed(source: &str) -> bool {
    source.is_empty()
        || source == "user"
        || ["command-", "schedule-", "autopilot-"]
            .iter()
            .any(|p| source.starts_with(p))
}

/// A top-level `key: value` from simple YAML.
fn yaml_value(yaml: &str, key: &str) -> Option<String> {
    let value = yaml
        .lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))?
        .trim();
    let value = if value.starts_with('"') {
        serde_json::from_str(value).ok()?
    } else if let Some(quoted) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        quoted.replace("''", "'")
    } else if value.starts_with(['|', '>']) {
        return None;
    } else {
        value.to_owned()
    };
    clean(&value)
}

#[derive(Deserialize)]
struct Line<'a> {
    #[serde(rename = "type", borrow)]
    kind: Cow<'a, str>,
    #[serde(borrow)]
    timestamp: Option<Cow<'a, str>>,
    #[serde(rename = "agentId", borrow)]
    agent_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    data: Option<Data<'a>>,
}

#[derive(Deserialize)]
struct Data<'a> {
    #[serde(borrow)]
    content: Option<&'a RawValue>,
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
    #[serde(rename = "newModel", borrow)]
    new_model: Option<Cow<'a, str>>,
    #[serde(borrow)]
    source: Option<Cow<'a, str>>,
    #[serde(rename = "agentId", borrow)]
    agent_id: Option<Cow<'a, str>>,
    #[serde(rename = "parentToolCallId", borrow)]
    parent_tool_call_id: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct Start {
    #[serde(rename = "type")]
    kind: String,
    timestamp: Option<String>,
    data: StartData,
}

#[derive(Deserialize)]
struct StartData {
    #[serde(rename = "selectedModel")]
    selected_model: Option<String>,
    context: Option<Context>,
}

#[derive(Deserialize)]
struct Context {
    cwd: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages() {
        let user = r#"{"type":"user.message","id":"b","parentId":"a","timestamp":"2026-01-02T03:04:05Z","data":{"content":"fix login"}}"#;
        let Some(Event::Msg(m)) = Copilot.line(user.as_bytes()) else {
            panic!()
        };
        assert_eq!(
            (m.role, m.text.as_str(), m.at),
            (Role::User, "fix login", Some(1_767_323_045))
        );
        let reply = r#"{"type":"assistant.message","id":"c","parentId":"b","timestamp":"t","data":{"content":"Done.","model":"gpt-5.4","toolRequests":[]}}"#;
        let Some(Event::Msg(m)) = Copilot.line(reply.as_bytes()) else {
            panic!()
        };
        assert_eq!(
            (m.role, m.text.as_str(), m.model.as_deref()),
            (Role::Assistant, "Done.", Some("gpt-5.4"))
        );
        let tool =
            r#"{"type":"tool.execution_complete","id":"d","data":{"content":"ran user.message"}}"#;
        assert!(Copilot.line(tool.as_bytes()).is_none());
        let changed =
            r#"{"type":"session.model_change","id":"e","data":{"newModel":"claude-sonnet-4.5"}}"#;
        assert!(
            matches!(Copilot.line(changed.as_bytes()), Some(Event::Model(m)) if m == "claude-sonnet-4.5")
        );
    }

    #[test]
    fn only_what_the_user_and_main_agent_said() {
        let text = |line: &str| match Copilot.line(line.as_bytes()) {
            Some(Event::Msg(m)) => Some(m.text),
            _ => None,
        };
        let skipped = [
            r#"{"type":"assistant.message","agentId":"sub-1","data":{"content":"from a sub-agent"}}"#,
            r#"{"type":"assistant.message","data":{"content":"also","parentToolCallId":"call_1"}}"#,
            r#"{"type":"user.message","data":{"content":"skill text","source":"skill-pdf"}}"#,
            r#"{"type":"user.message","data":{"content":"hand-off","source":"agent-2"}}"#,
            r#"{"type":"session.model_change","data":{"newModel":"gpt-5.4-mini","agentId":"sub-1"}}"#,
        ];
        for line in skipped {
            assert!(Copilot.line(line.as_bytes()).is_none(), "{line}");
        }
        let command =
            r#"{"type":"user.message","data":{"content":"/review","source":"command-review"}}"#;
        assert_eq!(text(command).as_deref(), Some("/review"));
        let nested = r#"{"type":"assistant.message","data":{"content":"Sent.","toolRequests":[{"arguments":{"type":"user.message"}}]}}"#;
        assert_eq!(text(nested).as_deref(), Some("Sent."));
    }

    #[test]
    fn yaml_values() {
        let yaml = "name: \"Fix \\\"login\\\"\"\nsummary: 'it''s done'\ncwd: /a/b\nnotes: |\n  x\n";
        assert_eq!(yaml_value(yaml, "name").as_deref(), Some("Fix \"login\""));
        assert_eq!(yaml_value(yaml, "summary").as_deref(), Some("it's done"));
        assert_eq!(yaml_value(yaml, "cwd").as_deref(), Some("/a/b"));
        assert_eq!(yaml_value(yaml, "notes"), None);
        assert_eq!(yaml_value(yaml, "missing"), None);
    }

    #[test]
    fn session_start_and_title() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        fs::write(
            dir.path().join("workspace.yaml"),
            "id: 3f2a\nname: \"Login fix\"\nsummary: Fixes the login form\ncwd: /home/a/app/web\n",
        )
        .unwrap();
        let start = br#"{"type":"session.start","id":"a","parentId":null,"timestamp":"2026-01-02T03:04:05Z","data":{"sessionId":"3f2a","selectedModel":"gpt-5.4","context":{"cwd":"/home/a/app"}}}"#;
        let h = Copilot.header(&path, start);
        assert_eq!(h.cwd.as_deref(), Some("/home/a/app/web"));
        assert_eq!(h.model.as_deref(), Some("gpt-5.4"));
        assert_eq!(h.started, Some(1_767_323_045));
        assert_eq!(h.title.as_deref(), Some("Login fix"));
        assert_eq!(h.auto_title.as_deref(), Some("Fixes the login form"));
        let later = br#"{"type":"user.message","data":{"content":"more"}}"#;
        assert_eq!(Copilot.header(&path, later).started, None);
        assert_eq!(Copilot.resume("3f2a", &path), ["copilot", "--resume=3f2a"]);
    }
}
