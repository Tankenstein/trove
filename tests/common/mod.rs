#![allow(dead_code)]

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use rusqlite::Connection;
use tempfile::TempDir;
use trove::env::Env;
use trove::index::{self, Progress};
use trove::search::{self, Session};

pub struct Home {
    dir: TempDir,
}

impl Home {
    pub fn new() -> Home {
        let dir = tempfile::Builder::new()
            .prefix("trove-home-")
            .tempdir()
            .unwrap();
        Home { dir }
    }

    pub fn path(&self) -> PathBuf {
        fs::canonicalize(self.dir.path()).unwrap()
    }

    pub fn env(&self) -> Env {
        Env::new(self.path())
    }

    pub fn db(&self) -> PathBuf {
        self.path().join("cache/index.db")
    }

    pub fn project(&self, name: &str) -> String {
        let p = self.path().join("Projects").join(name);
        fs::create_dir_all(&p).unwrap();
        p.to_str().unwrap().to_owned()
    }

    pub fn claude(&self, root: &str, cwd: &str, id: &str, lines: &[String]) -> PathBuf {
        let path = self
            .path()
            .join(root)
            .join("projects")
            .join(encode(cwd))
            .join(format!("{id}.jsonl"));
        write_lines(&path, lines);
        path
    }

    pub fn codex(&self, root: &str, id: &str, lines: &[String]) -> PathBuf {
        let path = self
            .path()
            .join(root)
            .join("sessions/2026/09/01")
            .join(rollout_name(id));
        write_lines(&path, lines);
        path
    }

    pub fn codex_archived(&self, root: &str, id: &str, lines: &[String]) -> PathBuf {
        let path = self
            .path()
            .join(root)
            .join("archived_sessions")
            .join(rollout_name(id));
        write_lines(&path, lines);
        path
    }

    pub fn codex_name(&self, root: &str, id: &str, name: &str) {
        let line = serde_json::json!({"id": id, "thread_name": name, "updated_at": "2026-09-01T10:00:00Z"});
        append(
            &self.path().join(root).join("session_index.jsonl"),
            &[line.to_string()],
        );
    }

    pub fn refresh(&self) -> Connection {
        self.refresh_with(&self.env())
    }

    pub fn refresh_with(&self, env: &Env) -> Connection {
        let db = self.db();
        let conn = index::open(&db).unwrap();
        index::refresh(&conn, &db, env, &Progress::default(), &mut || {}).unwrap();
        conn
    }

    pub fn trove(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_trove"));
        cmd.env_clear()
            .env("HOME", self.path())
            .env("SHELL", "/bin/sh")
            .current_dir(self.path());
        cmd
    }
}

pub fn rollout_name(id: &str) -> String {
    format!("rollout-2026-09-01T10-00-00-{id}.jsonl")
}

pub fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

pub fn encode(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

pub fn write_lines(path: &Path, lines: &[String]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut body = lines.join("\n");
    body.push('\n');
    fs::write(path, body).unwrap();
}

pub fn append(path: &Path, lines: &[String]) {
    for line in lines {
        append_raw(path, &format!("{line}\n"));
    }
}

pub fn append_raw(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}

pub fn set_age(path: &Path, secs_ago: u64) {
    let file = File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(secs_ago))
        .unwrap();
}

pub fn now() -> i64 {
    trove::display::now()
}

pub fn titles(conn: &Connection, query: &str) -> Vec<String> {
    find(conn, query, Path::new("/nonexistent"))
        .into_iter()
        .map(|s| s.title)
        .collect()
}

pub fn find(conn: &Connection, query: &str, here: &Path) -> Vec<Session> {
    let sessions = search::load(conn).unwrap();
    let query = search::query(conn, query).unwrap();
    let hits = search::search(conn, &sessions, &query, here, now()).unwrap();
    hits.into_iter().map(|h| sessions[h.idx].clone()).collect()
}

pub fn messages(conn: &Connection, title: &str) -> Vec<String> {
    let s = search::load(conn)
        .unwrap()
        .into_iter()
        .find(|s| s.title == title)
        .expect("chat exists");
    search::transcript(conn, s.id)
        .unwrap()
        .into_iter()
        .map(|m| m.text)
        .collect()
}

pub mod claude {
    use serde_json::json;

    pub fn user(text: &str, cwd: &str) -> String {
        json!({
            "parentUuid": null, "isSidechain": false, "promptId": "p", "type": "user",
            "message": {"role": "user", "content": text},
            "uuid": "u1", "timestamp": "2026-09-01T10:00:00.000Z", "origin": {"kind": "human"},
            "userType": "external", "entrypoint": "cli", "cwd": cwd, "sessionId": "s", "version": "2.1.283",
        })
        .to_string()
    }

    pub fn assistant(text: &str, cwd: &str) -> String {
        json!({
            "parentUuid": "u1", "isSidechain": false,
            "message": {"model": "claude-opus-5-5", "id": "msg", "type": "message", "role": "assistant",
                        "content": [{"type": "text", "text": text}]},
            "type": "assistant", "uuid": "a1", "timestamp": "2026-09-01T10:00:01.000Z", "cwd": cwd,
        })
        .to_string()
    }

    pub fn tool_call(cwd: &str) -> String {
        json!({
            "parentUuid": "a1", "isSidechain": false,
            "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "ls"}}]},
            "type": "assistant", "cwd": cwd,
        })
        .to_string()
    }

    pub fn tool_result(output: &str, cwd: &str) -> String {
        json!({
            "parentUuid": "a2", "isSidechain": false, "type": "user",
            "message": {"role": "user", "content": [{"tool_use_id": "toolu_1", "type": "tool_result", "content": output}]},
            "cwd": cwd,
        })
        .to_string()
    }

    pub fn ai_title(title: &str) -> String {
        json!({"type": "ai-title", "aiTitle": title, "sessionId": "s"}).to_string()
    }

    pub fn custom_title(title: &str) -> String {
        json!({"type": "custom-title", "customTitle": title, "sessionId": "s"}).to_string()
    }
}

pub mod codex {
    use serde_json::json;

    pub fn meta(id: &str, cwd: &str) -> String {
        json!({
            "timestamp": "2026-09-01T10:00:00.000Z", "ordinal": 0, "type": "session_meta",
            "payload": {"session_id": id, "id": id, "timestamp": "2026-09-01T10:00:00.000Z", "cwd": cwd,
                        "originator": "codex-tui", "cli_version": "0.157.1", "source": "cli",
                        "base_instructions": {"text": "You are Codex."}},
        })
        .to_string()
    }

    pub fn review_meta(id: &str, cwd: &str) -> String {
        json!({
            "timestamp": "2026-09-01T10:00:00.000Z", "type": "session_meta",
            "payload": {"id": id, "cwd": cwd, "source": {"subagent": {"other": "guardian"}}, "thread_source": "guardian_review"},
        })
        .to_string()
    }

    pub fn user(text: &str) -> Vec<String> {
        vec![
            json!({"timestamp": "t", "type": "response_item",
                   "payload": {"type": "message", "id": "m1", "role": "user", "content": [{"type": "input_text", "text": text}]}})
            .to_string(),
            json!({"timestamp": "t", "type": "event_msg",
                   "payload": {"type": "user_message", "message": text, "images": [], "text_elements": []}})
            .to_string(),
            json!({"timestamp": "t", "type": "event_msg",
                   "payload": {"type": "item_completed", "thread_id": "x", "turn_id": "y",
                               "item": {"type": "UserMessage", "id": "i", "content": [{"type": "text", "text": text, "text_elements": []}]}}})
            .to_string(),
        ]
    }

    pub fn assistant(text: &str) -> Vec<String> {
        vec![
            json!({"timestamp": "t", "type": "event_msg",
                   "payload": {"type": "item_completed", "thread_id": "x", "turn_id": "y",
                               "item": {"type": "AgentMessage", "id": "i", "content": [{"type": "Text", "text": text}]}}})
            .to_string(),
            json!({"timestamp": "t", "type": "response_item",
                   "payload": {"type": "message", "id": "m2", "role": "assistant", "content": [{"type": "output_text", "text": text}]}})
            .to_string(),
            json!({"timestamp": "t", "type": "event_msg", "payload": {"type": "agent_message", "message": text}}).to_string(),
        ]
    }

    pub fn injected(cwd: &str) -> String {
        let text = format!("<environment_context>\n  <cwd>{cwd}</cwd>\n</environment_context>");
        json!({"timestamp": "t", "type": "response_item",
               "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}})
        .to_string()
    }

    pub fn compacted(text: &str) -> String {
        json!({"timestamp": "t", "type": "compacted",
               "payload": {"message": "", "replacement_history": [
                   {"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}]}})
        .to_string()
    }

    pub fn filler(size: usize) -> String {
        json!({"timestamp": "t", "type": "event_msg",
               "payload": {"type": "exec_command_end", "stdout": "x".repeat(size)}})
        .to_string()
    }

    pub fn turn(model: &str, cwd: &str) -> String {
        json!({"timestamp": "t", "type": "turn_context",
               "payload": {"turn_id": "x", "cwd": cwd, "approval_policy": "on-request", "model": model, "effort": "xhigh"}})
        .to_string()
    }

    pub fn session(id: &str, cwd: &str, prompt: &str, reply: &str) -> Vec<String> {
        let mut lines = vec![meta(id, cwd), injected(cwd), turn("gpt-6-astra", cwd)];
        lines.extend(user(prompt));
        lines.extend(assistant(reply));
        lines
    }
}
