use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Deserialize;

use super::{Agent, Chat, Database, Format, Role, file_name, short_model};
use crate::env::Env;

pub struct OpenCode;

impl Agent for OpenCode {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn color(&self) -> u8 {
        138
    }

    fn roots(&self, env: &Env) -> Vec<PathBuf> {
        let data = env
            .var("XDG_DATA_HOME")
            .map_or_else(|| env.home.join(".local/share"), Path::to_owned);
        vec![data.join("opencode")]
    }

    fn stores(&self, root: &Path) -> Vec<(PathBuf, bool)> {
        if file_name(root) == "opencode" && root.is_dir() {
            vec![(root.to_owned(), false)]
        } else {
            Vec::new()
        }
    }

    /// `opencode.db`, and the databases of other release channels (`opencode-dev.db`).
    fn chats(&self, store: &Path, found: &mut dyn FnMut(PathBuf)) {
        let Ok(entries) = std::fs::read_dir(store) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = file_name(&path);
            if name.starts_with("opencode") && name.ends_with(".db") {
                found(path);
            }
        }
    }

    fn format(&self) -> Format<'_> {
        Format::Database(self)
    }

    fn model_label(&self, model: &str) -> String {
        short_model(model)
    }

    fn resume(&self, id: &str, _file: &Path) -> Vec<String> {
        vec!["opencode".into(), "--session".into(), id.into()]
    }
}

impl Database for OpenCode {
    /// Each session with its last update and, where the database has one, the sequence number
    /// of its latest event, which also moves while a reply streams in.
    fn versions(&self, db: &Path) -> Option<Vec<(String, i64)>> {
        let conn = open(db)?;
        let queries = [
            "SELECT s.id, s.time_updated, COALESCE(MAX(e.seq), 0) FROM session s
             LEFT JOIN event_sequence e ON e.aggregate_id = s.id GROUP BY s.id",
            "SELECT s.id, s.time_updated,
                    COALESCE((SELECT MAX(p.time_updated) FROM part p WHERE p.session_id = s.id), 0)
             FROM session s",
            "SELECT id, time_updated, 0 FROM session",
        ];
        queries.iter().find_map(|sql| {
            let mut stmt = conn.prepare(sql).ok()?;
            let rows = stmt.query_map([], |r| {
                let (id, updated, seq): (String, i64, i64) = (r.get(0)?, r.get(1)?, r.get(2)?);
                Ok((id, updated.wrapping_mul(1_000_003) ^ seq))
            });
            rows.ok()?.collect::<rusqlite::Result<Vec<_>>>().ok()
        })
    }

    fn read(&self, db: &Path, id: &str) -> Option<Chat> {
        let conn = open(db)?;
        let session = conn
            .query_row(
                "SELECT directory, title, time_created, time_updated, time_archived, parent_id, model
                 FROM session WHERE id = ?1",
                [id],
                |r| {
                    Ok(Session {
                        directory: r.get(0)?,
                        title: r.get(1)?,
                        created: r.get(2)?,
                        updated: r.get(3)?,
                        archived: r.get(4)?,
                        parent: r.get(5)?,
                        model: r.get(6)?,
                    })
                },
            )
            .optional()
            .ok()??;
        let mut stmt = conn
            .prepare(
                "SELECT m.id, m.data, p.data FROM message m JOIN part p ON p.message_id = m.id
                 WHERE m.session_id = ?1 ORDER BY m.time_created, m.id, p.id",
            )
            .ok()?;
        let rows = stmt
            .query_map([id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .ok()?;

        let mut chat = Chat {
            cwd: session.directory.unwrap_or_default(),
            title: session.title.filter(|t| !t.starts_with("New session - ")),
            model: session.model.as_deref().and_then(model_of),
            started: session.created.map(|ms| ms / 1000),
            updated: session.updated.unwrap_or(0) / 1000,
            archived: session.archived.is_some(),
            hidden: session.parent.is_some(),
            messages: Vec::new(),
        };
        let mut current: Option<(String, Role, Vec<String>)> = None;
        for row in rows {
            let (message_id, message, part) = row.ok()?;
            let Ok(message) = serde_json::from_str::<Message>(&message) else {
                continue;
            };
            let role = match message.role.as_str() {
                "user" => Role::User,
                // A summary is what compaction wrote in place of the messages before it.
                "assistant" if message.summary != Some(serde_json::Value::Bool(true)) => {
                    Role::Assistant
                }
                _ => continue,
            };
            if role == Role::Assistant && message.model_id.is_some() {
                chat.model = message.model_id;
            }
            if current.as_ref().is_none_or(|(m, _, _)| *m != message_id) {
                chat.messages.extend(
                    current
                        .take()
                        .map(|(_, role, texts)| (role, texts.join("\n"))),
                );
                current = Some((message_id, role, Vec::new()));
            }
            // Synthetic text is what opencode adds, not what anyone typed or replied.
            if let Ok(part) = serde_json::from_str::<Part>(&part)
                && part.kind == "text"
                && !part.synthetic.unwrap_or(false)
                && !part.ignored.unwrap_or(false)
                && let (Some(text), Some((_, _, texts))) = (part.text, current.as_mut())
            {
                texts.push(text);
            }
        }
        chat.messages
            .extend(current.map(|(_, role, texts)| (role, texts.join("\n"))));
        chat.messages.retain(|(_, text)| !text.trim().is_empty());
        Some(chat)
    }
}

/// Read-only, since opencode may be writing to it.
fn open(db: &Path) -> Option<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(db, flags).ok()?;
    conn.busy_timeout(Duration::from_secs(2)).ok()?;
    Some(conn)
}

/// The model's id from a `{"id": …, "providerID": …}` column.
fn model_of(json: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Model {
        id: Option<String>,
        #[serde(rename = "modelID")]
        model_id: Option<String>,
    }
    let model = serde_json::from_str::<Model>(json).ok()?;
    model.id.or(model.model_id)
}

struct Session {
    directory: Option<String>,
    title: Option<String>,
    created: Option<i64>,
    updated: Option<i64>,
    archived: Option<i64>,
    parent: Option<String>,
    model: Option<String>,
}

#[derive(Deserialize)]
struct Message {
    role: String,
    #[serde(rename = "modelID")]
    model_id: Option<String>,
    summary: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Part {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    synthetic: Option<bool>,
    ignored: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models() {
        assert_eq!(
            model_of(r#"{"id":"claude-opus-5","providerID":"anthropic"}"#).as_deref(),
            Some("claude-opus-5")
        );
        assert_eq!(model_of("not json"), None);
        assert_eq!(
            OpenCode.resume("ses_1", Path::new("/x")),
            ["opencode", "--session", "ses_1"]
        );
    }
}
