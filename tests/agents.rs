//! Pi, Copilot CLI, Gemini CLI and OpenCode, in fake homes built from their documented formats.
//! None of these has been tried against a real install.

mod common;

use std::fs;
use std::path::Path;

use common::{Home, append, messages, set_age, titles, write_lines};
use rusqlite::{Connection, params};
use serde_json::json;
use trove::shell::Quoting;
use trove::{resume, search};

fn resume_command(conn: &Connection, title: &str) -> String {
    let s = search::load(conn)
        .unwrap()
        .into_iter()
        .find(|s| s.title == title)
        .expect("chat exists");
    resume::command(&s, Quoting::Posix)
}

#[test]
fn pi() {
    let home = Home::new();
    let shop = home.project("shop");
    let path = home
        .path()
        .join(".pi/agent/sessions/--shop--/2026-01-02T03-04-05-000Z_0199abcd.jsonl");
    write_lines(
        &path,
        &[
            json!({"type": "session", "version": 3, "id": "0199abcd", "timestamp": "2026-01-02T03:04:05.000Z", "cwd": shop}).to_string(),
            json!({"type": "message", "id": "a1", "parentId": null, "timestamp": "2026-01-02T03:04:06.000Z",
                   "message": {"role": "user", "content": "the login form double-submits"}}).to_string(),
            json!({"type": "message", "id": "b2", "parentId": "a1", "timestamp": "2026-01-02T03:04:07.000Z",
                   "message": {"role": "assistant", "provider": "anthropic", "model": "claude-sonnet-4-5",
                               "content": [{"type": "thinking", "thinking": "hmm"}, {"type": "text", "text": "Debounce the handler."},
                                           {"type": "toolCall", "name": "edit"}]}}).to_string(),
            json!({"type": "message", "id": "c3", "parentId": "b2", "message": {"role": "toolResult", "content": "edited"}}).to_string(),
            json!({"type": "session_info", "id": "d4", "name": "Login double submit"}).to_string(),
        ],
    );
    let conn = home.refresh();
    assert_eq!(titles(&conn, "debounce"), ["Login double submit"]);
    assert_eq!(
        messages(&conn, "Login double submit"),
        ["the login form double-submits", "Debounce the handler."]
    );
    let s = &search::load(&conn).unwrap()[0];
    assert_eq!(
        (s.agent.name(), s.cwd.as_str(), s.model.as_str()),
        ("pi", shop.as_str(), "claude-sonnet-4-5")
    );
    // Pi resumes by file, which works from anywhere.
    assert_eq!(
        resume_command(&conn, "Login double submit"),
        format!("cd {shop} && pi --session {}", path.display())
    );

    append(&path, &[json!({"type": "message", "id": "e5", "message": {"role": "user", "content": "now add a test"}}).to_string()]);
    assert_eq!(messages(&home.refresh(), "Login double submit").len(), 3);
}

#[test]
fn copilot() {
    let home = Home::new();
    let app = home.project("app");
    let id = "3f2a9b1c-0000-4000-8000-000000000001";
    let dir = home.path().join(".copilot/session-state").join(id);
    write_lines(
        &dir.join("events.jsonl"),
        &[
            json!({"type": "session.start", "id": "a", "parentId": null, "timestamp": "2026-01-02T03:04:05Z",
                   "data": {"sessionId": id, "selectedModel": "gpt-5.4", "context": {"cwd": app}}}).to_string(),
            json!({"type": "user.message", "id": "b", "parentId": "a", "timestamp": "2026-01-02T03:04:06Z",
                   "data": {"content": "why is checkout slow"}}).to_string(),
            json!({"type": "tool.execution_start", "id": "c", "parentId": "b", "data": {"toolName": "bash"}}).to_string(),
            json!({"type": "assistant.message", "id": "d", "parentId": "c", "timestamp": "2026-01-02T03:04:09Z",
                   "data": {"content": "An N+1 query in the cart.", "model": "gpt-5.4", "toolRequests": []}}).to_string(),
        ],
    );
    fs::write(
        dir.join("workspace.yaml"),
        format!("id: {id}\nname: Checkout speed\ncwd: {app}\n"),
    )
    .unwrap();
    let conn = home.refresh();
    assert_eq!(titles(&conn, "cart"), ["Checkout speed"]);
    assert_eq!(
        messages(&conn, "Checkout speed"),
        ["why is checkout slow", "An N+1 query in the cart."]
    );
    let s = &search::load(&conn).unwrap()[0];
    assert_eq!(
        (s.agent.name(), s.sid.as_str(), s.cwd.as_str()),
        ("copilot", id, app.as_str())
    );
    assert_eq!(
        resume_command(&conn, "Checkout speed"),
        format!("cd {app} && copilot --resume={id}")
    );
}

#[test]
fn gemini() {
    let home = Home::new();
    let shop = home.project("shop");
    let id = "3f2a9b1c-1111-4000-8000-000000000002";
    let project = home.path().join(".gemini/tmp/shop");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join(".project_root"), format!("{shop}\n")).unwrap();
    let reply = json!({"id": "m2", "timestamp": "2026-01-02T03:04:07Z", "type": "gemini", "content": "Pin the toolchain.", "model": "gemini-3-pro"});
    let mut updated = reply.clone();
    updated["tokens"] = json!({"input": 10});
    write_lines(
        &project.join("chats/session-2026-01-02T03-04-3f2a9b1c.jsonl"),
        &[
            json!({"sessionId": id, "projectHash": "ab12", "startTime": "2026-01-02T03:04:05Z", "lastUpdated": "t", "kind": "main"}).to_string(),
            json!({"id": "m1", "timestamp": "2026-01-02T03:04:06Z", "type": "user", "content": [{"text": "the build breaks on CI"}]}).to_string(),
            reply.to_string(),
            // The same message again, with its token counts.
            updated.to_string(),
            json!({"id": "m3", "type": "user", "content": [{"functionResponse": {"name": "shell"}}]}).to_string(),
            json!({"$set": {"summary": "CI build failure"}}).to_string(),
        ],
    );
    write_lines(
        &project.join("chats/parent-id/session-sub.jsonl"),
        &[json!({"sessionId": "sub", "kind": "subagent"}).to_string()],
    );
    write_lines(
        &project.join("chats/session-2026-01-03T00-00-yolo.jsonl"),
        &[
            json!({"sessionId": "--yolo", "kind": "main"}).to_string(),
            json!({"id": "m1", "type": "user", "content": [{"text": "an id that reads as an option"}]}).to_string(),
        ],
    );
    let conn = home.refresh();
    assert_eq!(titles(&conn, "toolchain"), ["CI build failure"]);
    assert_eq!(
        messages(&conn, "CI build failure"),
        ["the build breaks on CI", "Pin the toolchain."]
    );
    let s = &search::load(&conn).unwrap()[0];
    assert_eq!(
        (s.agent.name(), s.sid.as_str(), s.cwd.as_str()),
        ("gemini", id, shop.as_str())
    );
    assert_eq!(search::load(&conn).unwrap().len(), 1);
    assert_eq!(
        resume_command(&conn, "CI build failure"),
        format!("cd {shop} && gemini --resume {id}")
    );
}

/// An OpenCode database with the tables and columns trove reads.
fn opencode_db(path: &Path) -> Connection {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE session(id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT, directory TEXT, title TEXT,
                              version TEXT, model TEXT, time_created INTEGER, time_updated INTEGER, time_archived INTEGER);
         CREATE TABLE message(id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
         CREATE TABLE part(id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
         CREATE TABLE event_sequence(aggregate_id TEXT PRIMARY KEY, seq INTEGER);",
    )
    .unwrap();
    conn
}

fn opencode_session(
    conn: &Connection,
    id: &str,
    dir: &str,
    title: &str,
    parent: Option<&str>,
    archived: bool,
) {
    conn.execute(
        "INSERT INTO session(id, parent_id, directory, title, model, time_created, time_updated, time_archived)
         VALUES (?1, ?2, ?3, ?4, ?5, 1767323045000, 1767323050000, ?6)",
        params![id, parent, dir, title, r#"{"id":"claude-opus-5","providerID":"anthropic"}"#, archived.then_some(1767323060000i64)],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO event_sequence(aggregate_id, seq) VALUES (?1, 1)",
        [id],
    )
    .unwrap();
}

fn opencode_message(
    conn: &Connection,
    session: &str,
    id: &str,
    at: i64,
    role: &str,
    parts: &[serde_json::Value],
) {
    let data = if role == "assistant" {
        json!({"role": role, "time": {"created": at}, "modelID": "claude-opus-5"})
    } else {
        json!({"role": role, "time": {"created": at}, "summary": {"diffs": []}})
    };
    conn.execute(
        "INSERT INTO message(id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?3, ?4)",
        params![id, session, at, data.to_string()],
    )
    .unwrap();
    for (n, part) in parts.iter().enumerate() {
        conn.execute(
            "INSERT INTO part(id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            params![format!("{id}_{n}"), id, session, at, part.to_string()],
        )
        .unwrap();
    }
}

#[test]
fn opencode() {
    let home = Home::new();
    let api = home.project("api");
    let path = home.path().join(".local/share/opencode/opencode.db");
    let conn = opencode_db(&path);
    opencode_session(&conn, "ses_1", &api, "Rate limiter", None, false);
    opencode_message(
        &conn,
        "ses_1",
        "msg_1",
        1767323046000,
        "user",
        &[
            json!({"type": "text", "text": "add a rate limiter to the login route"}),
            json!({"type": "text", "text": "<context from opencode>", "synthetic": true}),
            json!({"type": "text", "text": "left out of the context", "ignored": true}),
        ],
    );
    opencode_message(
        &conn,
        "ses_1",
        "msg_2",
        1767323047000,
        "assistant",
        &[
            json!({"type": "reasoning", "text": "thinking about buckets"}),
            json!({"type": "tool", "tool": "edit"}),
            json!({"type": "text", "text": "Added a token bucket."}),
        ],
    );
    opencode_message(
        &conn,
        "ses_1",
        "msg_c",
        1767323047500,
        "assistant",
        &[json!({"type": "text", "text": "Summary of the conversation so far"})],
    );
    conn.execute(
        "UPDATE message SET data = json_set(data, '$.summary', json('true')) WHERE id = 'msg_c'",
        [],
    )
    .unwrap();
    opencode_session(
        &conn,
        "ses_2",
        &api,
        "Explore the codebase (@explore subagent)",
        Some("ses_1"),
        false,
    );
    opencode_message(
        &conn,
        "ses_2",
        "msg_3",
        1767323048000,
        "user",
        &[json!({"type": "text", "text": "subagent task"})],
    );
    opencode_session(
        &conn,
        "ses_3",
        &api,
        "New session - 2026-01-02T03:04:05.000Z",
        None,
        true,
    );
    opencode_message(
        &conn,
        "ses_3",
        "msg_4",
        1767323049000,
        "user",
        &[json!({"type": "text", "text": "an old question"})],
    );

    let index = home.refresh();
    assert_eq!(titles(&index, "bucket"), ["Rate limiter"]);
    assert_eq!(
        messages(&index, "Rate limiter"),
        [
            "add a rate limiter to the login route",
            "Added a token bucket."
        ]
    );
    assert!(
        titles(&index, "subagent").is_empty(),
        "subagent sessions are hidden"
    );
    let all = search::load(&index).unwrap();
    let old = all.iter().find(|s| s.sid == "ses_3").unwrap();
    assert_eq!(
        (old.title.as_str(), old.archived, old.model.as_str()),
        ("an old question", true, "claude-opus-5")
    );
    let s = all.iter().find(|s| s.sid == "ses_1").unwrap();
    assert_eq!(
        (s.agent.name(), s.cwd.as_str(), s.model.as_str()),
        ("opencode", api.as_str(), "claude-opus-5")
    );
    assert_eq!(
        resume_command(&index, "Rate limiter"),
        format!("cd {api} && opencode --session ses_1")
    );

    // A reply arrives: the chat's events move on, and it's read again.
    opencode_message(
        &conn,
        "ses_1",
        "msg_5",
        1767323051000,
        "assistant",
        &[json!({"type": "text", "text": "Also added tests."})],
    );
    conn.execute(
        "UPDATE event_sequence SET seq = 2 WHERE aggregate_id = 'ses_1'",
        [],
    )
    .unwrap();
    let index = home.refresh();
    assert_eq!(messages(&index, "Rate limiter").len(), 3);

    // A deleted session leaves the index.
    conn.execute("DELETE FROM session WHERE id = 'ses_3'", [])
        .unwrap();
    set_age(&path, 0);
    let index = home.refresh();
    assert!(titles(&index, "old question").is_empty());
}

#[test]
fn an_unreadable_opencode_database_is_skipped() {
    let home = Home::new();
    let shop = home.project("shop");
    home.claude(
        ".claude",
        &shop,
        &common::id(1),
        &[common::claude::user("hello", &shop)],
    );
    let path = home.path().join(".local/share/opencode/opencode.db");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "not a database").unwrap();
    let conn = home.refresh();
    assert_eq!(search::load(&conn).unwrap().len(), 1);
}
