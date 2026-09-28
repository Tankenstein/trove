use std::borrow::Cow;
use std::path::{Path, PathBuf};

use memchr::memmem::Finder;
use serde::Deserialize;
use serde_json::value::RawValue;

use super::{
    Agent, Event, Format, Header, Lines, Msg, Role, clean, file_name, finder, is_uuid, lines,
    prompt, short_model, text_of, timestamp, walk,
};
use crate::env::Env;

pub struct Claude;

impl Agent for Claude {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn color(&self) -> u8 {
        173
    }

    fn roots(&self, env: &Env) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = env
            .var("CLAUDE_CONFIG_DIR")
            .map(Path::to_owned)
            .into_iter()
            .collect();
        roots.push(env.home.join(".claude"));
        roots.push(env.config_dir().join("claude"));
        roots
    }

    fn stores(&self, root: &Path) -> Vec<(PathBuf, bool)> {
        let projects = root.join("projects");
        if projects.is_dir() {
            vec![(projects, false)]
        } else {
            Vec::new()
        }
    }

    fn chats(&self, store: &Path, found: &mut dyn FnMut(PathBuf)) {
        walk(store, 1, &mut |path| {
            let in_project = path.parent().is_some_and(|p| p != store);
            if in_project && file_name(&path).strip_suffix(".jsonl").is_some_and(is_uuid) {
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
        vec!["claude".into(), "--resume".into(), id.into()]
    }
}

impl Lines for Claude {
    fn session_id(&self, file: &Path) -> Option<String> {
        Some(file.file_stem()?.to_str()?.to_owned())
    }

    fn header(&self, file: &Path, data: &[u8]) -> Header {
        Header {
            cwd: launch_folder(file, data),
            ..Header::default()
        }
    }

    fn line(&self, line: &[u8]) -> Option<Event> {
        self::line(line)
    }
}

finder!(USER, br#""type":"user""#);
finder!(ASSISTANT, br#""type":"assistant""#);
finder!(TEXT, br#""type":"text""#);
finder!(TOOL_RESULT, br#""tool_use_id""#);
finder!(CUSTOM_TITLE, br#""type":"custom-title""#);
finder!(AI_TITLE, br#""type":"ai-title""#);
finder!(SUMMARY, br#""type":"summary""#);
finder!(CWD, br#""cwd":""#);

#[derive(Deserialize)]
struct Line<'a> {
    #[serde(rename = "type", borrow)]
    kind: Cow<'a, str>,
    #[serde(borrow)]
    message: Option<Message<'a>>,
    #[serde(rename = "isMeta")]
    is_meta: Option<bool>,
    #[serde(rename = "isSidechain")]
    is_sidechain: Option<bool>,
    #[serde(rename = "isCompactSummary")]
    is_compact_summary: Option<bool>,
    #[serde(borrow)]
    origin: Option<&'a RawValue>,
    #[serde(rename = "customTitle", borrow)]
    custom_title: Option<&'a RawValue>,
    #[serde(rename = "aiTitle", borrow)]
    ai_title: Option<&'a RawValue>,
    #[serde(borrow)]
    summary: Option<&'a RawValue>,
    #[serde(borrow)]
    timestamp: Option<Cow<'a, str>>,
    #[serde(borrow)]
    cwd: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct Message<'a> {
    #[serde(borrow)]
    content: Option<&'a RawValue>,
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct Origin<'a> {
    #[serde(borrow)]
    kind: Option<Cow<'a, str>>,
}

fn line(b: &[u8]) -> Option<Event> {
    let has = |f: &Finder| f.find(b).is_some();
    let user = has(&USER);
    let assistant = has(&ASSISTANT);
    if !user && !assistant && !(has(&CUSTOM_TITLE) || has(&AI_TITLE) || has(&SUMMARY)) {
        return None;
    }
    if user && !assistant && has(&TOOL_RESULT) && !has(&TEXT) || assistant && !user && !has(&TEXT) {
        return None;
    }

    let l: Line = serde_json::from_slice(b).ok()?;
    match &*l.kind {
        "user" => message(l, Role::User),
        "assistant" => message(l, Role::Assistant),
        "custom-title" => string(l.custom_title).map(Event::Title),
        "ai-title" => string(l.ai_title).map(Event::AutoTitle),
        "summary" => string(l.summary).map(Event::AutoTitle),
        _ => None,
    }
}

fn message(l: Line, role: Role) -> Option<Event> {
    if l.is_meta == Some(true) || l.is_sidechain == Some(true) || l.is_compact_summary == Some(true)
    {
        return None;
    }
    if role == Role::User
        && let Some(origin) = l.origin
        && let Ok(Origin { kind: Some(kind) }) = serde_json::from_str::<Origin>(origin.get())
        && kind != "human"
    {
        return None;
    }
    let message = l.message?;
    let text = text_of(message.content?)?;
    let text = match role {
        Role::User => prompt(&typed(&text)?)?,
        Role::Assistant => clean(&text)?,
    };
    Some(Event::Msg(Msg {
        role,
        text,
        at: l.timestamp.as_deref().and_then(timestamp),
        // `<synthetic>` marks replies Claude Code wrote itself.
        model: message
            .model
            .filter(|m| !m.starts_with('<'))
            .map(Cow::into_owned),
        folder: l.cwd.map(Cow::into_owned),
    }))
}

/// A prompt as typed: Claude Code adds system reminders to it, and wraps commands and shell
/// input in tags.
fn typed(text: &str) -> Option<String> {
    let text = strip_blocks(text, "system-reminder");
    let t = text.trim();
    if t.starts_with("<command-") {
        let name = inner(t, "command-name").unwrap_or("");
        let args = inner(t, "command-args").unwrap_or("").trim();
        return (!args.is_empty()).then(|| format!("{name} {args}"));
    }
    if t.starts_with("<bash-input>") {
        return inner(t, "bash-input").map(str::to_owned);
    }
    if t.starts_with("[Request interrupted by user") {
        return None;
    }
    Some(t.to_owned())
}

fn strip_blocks<'a>(text: &'a str, tag: &str) -> Cow<'a, str> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    if !text.contains(&open) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        out.push_str(&rest[..start]);
        rest = match rest[start..].find(&close) {
            Some(end) => &rest[start + end + close.len()..],
            None => "",
        };
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn inner<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&format!("</{tag}>"))?;
    Some(&text[start..start + end])
}

fn string(raw: Option<&RawValue>) -> Option<String> {
    let s: String = serde_json::from_str(raw?.get()).ok()?;
    clean(&s)
}

/// Claude Code resumes a chat only in the folder its project folder is named after, which the
/// chat may have moved on from.
fn launch_folder(file: &Path, data: &[u8]) -> Option<String> {
    let dir = file.parent().map(file_name)?;
    lines(data).find_map(|line| {
        let at = CWD.find(line)?;
        let cwd = json_string_at(line, at + br#""cwd":"#.len())?;
        project_dir_matches(dir, &cwd).then_some(cwd)
    })
}

fn json_string_at(line: &[u8], start: usize) -> Option<String> {
    let mut i = start + 1;
    while i < line.len() {
        match line[i] {
            b'\\' => i += 2,
            b'"' => return serde_json::from_slice(&line[start..=i]).ok(),
            _ => i += 1,
        }
    }
    None
}

// Claude Code names a project folder after its path: characters outside `[a-zA-Z0-9]` become
// `-` (one per UTF-16 unit), and names over 200 characters are cut there and get a hash suffix.
fn project_dir_matches(dir_name: &str, cwd: &str) -> bool {
    let mut enc = String::with_capacity(cwd.len());
    for c in cwd.chars() {
        if c.is_ascii_alphanumeric() {
            enc.push(c);
        } else {
            (0..c.len_utf16()).for_each(|_| enc.push('-'));
        }
    }
    if enc.len() <= 200 {
        dir_name == enc
    } else {
        dir_name.len() > 201
            && dir_name.starts_with(&enc[..200])
            && dir_name.as_bytes()[200] == b'-'
    }
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
    fn when_and_which_model() {
        let reply = r#"{"message":{"model":"claude-opus-5-5","role":"assistant","content":[{"type":"text","text":"hi"}]},"type":"assistant","timestamp":"2026-01-02T03:04:05.678Z"}"#;
        let Some(Event::Msg(m)) = super::line(reply.as_bytes()) else {
            panic!()
        };
        assert_eq!(m.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(m.at, Some(1_767_323_045));
        let user = r#"{"type":"user","message":{"content":"hi"},"cwd":"/p/api"}"#;
        let Some(Event::Msg(m)) = super::line(user.as_bytes()) else {
            panic!()
        };
        assert_eq!(m.folder.as_deref(), Some("/p/api"));
    }

    #[test]
    fn prompts_as_typed() {
        let typed = |text: &str| typed(text).and_then(|t| prompt(&t));
        assert_eq!(
            typed("<system-reminder>ctx</system-reminder>hello").as_deref(),
            Some("hello")
        );
        assert_eq!(typed("<system-reminder>only</system-reminder>"), None);
        assert_eq!(typed("[Request interrupted by user for tool use]"), None);
        assert_eq!(
            typed("<command-message>review</command-message>\n<command-name>/review</command-name>\n<command-args>501</command-args>").as_deref(),
            Some("/review 501")
        );
        assert_eq!(
            typed("<command-name>/model</command-name>\n<command-args></command-args>"),
            None
        );
        assert_eq!(
            typed("<bash-input>git status</bash-input>").as_deref(),
            Some("git status")
        );
    }

    #[test]
    fn prompts_and_replies() {
        let user = r#"{"parentUuid":null,"type":"user","message":{"role":"user","content":"find the flaky test"},"cwd":"/p"}"#;
        assert_eq!(msg(user), Some((Role::User, "find the flaky test".into())));

        let blocks = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"look at this"},{"type":"image","source":{}}]}}"#;
        assert_eq!(msg(blocks), Some((Role::User, "look at this".into())));

        let reply = r#"{"message":{"role":"assistant","content":[{"type":"text","text":"It's a race."}]},"type":"assistant"}"#;
        assert_eq!(msg(reply), Some((Role::Assistant, "It's a race.".into())));
    }

    #[test]
    fn text_next_to_a_tool_result_is_kept() {
        let line = r#"{"type":"user","message":{"content":[{"tool_use_id":"t1","type":"tool_result","content":"ok"},{"type":"text","text":"actually use postgres"}]}}"#;
        assert_eq!(
            msg(line),
            Some((Role::User, "actually use postgres".into()))
        );
        let nested = r#"{"type":"user","message":{"content":[{"tool_use_id":"t1","type":"tool_result","content":[{"type":"text","text":"tool says hi"}]}]}}"#;
        assert_eq!(msg(nested), None);
    }

    #[test]
    fn skips_noise() {
        let tool_result = r#"{"type":"user","message":{"content":[{"tool_use_id":"t1","type":"tool_result","content":"ok"}]}}"#;
        let thinking =
            r#"{"message":{"content":[{"type":"thinking","thinking":"hmm"}]},"type":"assistant"}"#;
        let tool_use = r#"{"message":{"content":[{"type":"tool_use","name":"Bash","input":{}}]},"type":"assistant"}"#;
        let meta = r#"{"type":"user","isMeta":true,"message":{"content":"Caveat: …"}}"#;
        let sidechain =
            r#"{"type":"user","isSidechain":true,"message":{"content":"subagent task"}}"#;
        let compact = r#"{"type":"user","isCompactSummary":true,"message":{"content":"This session is being continued…"}}"#;
        let notification = r#"{"type":"user","origin":{"kind":"task-notification"},"message":{"content":"<task-notification>…"}}"#;
        let other = r#"{"type":"file-history-snapshot","snapshot":{}}"#;
        for line in [
            tool_result,
            thinking,
            tool_use,
            meta,
            sidechain,
            compact,
            notification,
            other,
        ] {
            assert!(super::line(line.as_bytes()).is_none(), "{line}");
        }
        let human =
            r#"{"type":"user","origin":{"kind":"human"},"message":{"content":"real prompt"}}"#;
        assert!(msg(human).is_some());
    }

    #[test]
    fn titles() {
        let custom = r#"{"type":"custom-title","customTitle":"Billing refactor","sessionId":"x"}"#;
        assert!(
            matches!(super::line(custom.as_bytes()), Some(Event::Title(t)) if t == "Billing refactor")
        );
        let ai = r#"{"type":"ai-title","aiTitle":"Fix webhook retries","sessionId":"x"}"#;
        assert!(
            matches!(super::line(ai.as_bytes()), Some(Event::AutoTitle(t)) if t == "Fix webhook retries")
        );
        let summary = r#"{"type":"summary","summary":"Old summary","leafUuid":"u"}"#;
        assert!(
            matches!(super::line(summary.as_bytes()), Some(Event::AutoTitle(t)) if t == "Old summary")
        );
    }

    #[test]
    fn project_folder_encoding() {
        assert!(project_dir_matches(
            "-Users-a-Projects-shop",
            "/Users/a/Projects/shop"
        ));
        assert!(project_dir_matches(
            "-Users-a-my-app--claude-worktrees-x",
            "/Users/a/my_app/.claude/worktrees/x"
        ));
        assert!(project_dir_matches("-Users-a-caf-", "/Users/a/café"));
        assert!(project_dir_matches("-Users-a-x--", "/Users/a/x😀"));
        assert!(!project_dir_matches(
            "-Users-a-Projects-shop",
            "/Users/a/Projects/shop/api"
        ));

        let long = format!("/Users/a/{}", "d".repeat(250));
        let mut enc = format!("-Users-a-{}", "d".repeat(250))[..200].to_string();
        enc.push_str("-1x2y3z");
        assert!(project_dir_matches(&enc, &long));
    }

    #[test]
    fn launch_folder_is_the_encoded_one() {
        let path = Path::new(
            "/h/.claude/projects/-Users-a-shop/7c6b5a49-3827-4165-8e0d-9f8a7b6c5d4e.jsonl",
        );
        let body = b"{\"type\":\"user\",\"cwd\":\"/Users/a/shop/api\"}\n{\"type\":\"user\",\"cwd\":\"/Users/a/shop\"}\n";
        assert_eq!(launch_folder(path, body).as_deref(), Some("/Users/a/shop"));
        assert_eq!(launch_folder(path, b"{\"cwd\":\"/elsewhere\"}\n"), None);

        let path =
            Path::new("/h/projects/-Users-a-q-dir/7c6b5a49-3827-4165-8e0d-9f8a7b6c5d4e.jsonl");
        assert_eq!(
            launch_folder(path, br#"{"cwd":"/Users/a/q\"dir"}"#).as_deref(),
            Some("/Users/a/q\"dir")
        );
    }
}
