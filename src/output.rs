use std::collections::HashSet;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use jiff::tz::TimeZone;
use rusqlite::Connection;
use serde::Serialize;

use crate::agents::Role;
use crate::display::{age, datetime, iso, now, one_line, printable, printable_text, tilde};
use crate::env::Env;
use crate::index::{self, Progress};
use crate::resume;
use crate::search::{self, Excerpt, Session};
use crate::shell::Quoting;

/// Prints the chats matching `words`, best first: a line of tab-separated fields each, or JSON.
pub fn matches(env: &Env, db: &Path, words: &str, json: bool) -> Result<ExitCode> {
    let (conn, sessions) = load(env, db)?;
    let now = now();
    let here = std::env::current_dir().unwrap_or_default();
    let query = search::query(&conn, words)?;
    let hits = search::search(&conn, &sessions, &query, &here, now)?;
    let mut out = io::stdout().lock();
    for hit in &hits {
        let s = &sessions[hit.idx];
        let line = if json {
            let matches: Vec<Excerpt> = hit
                .rows
                .iter()
                .filter_map(|&row| search::snippet(&conn, &query, row).transpose())
                .collect::<Result<_>>()?;
            serde_json::to_string(&Found {
                chat: Summary::of(s),
                matches: matches.iter().map(Message::of).collect(),
            })?
        } else {
            format!(
                "{}\t{}\t{}\t{}\t{}",
                age(now - s.updated),
                s.agent.name(),
                printable(&tilde(&s.cwd, env.home_str())),
                one_line(&s.title),
                printable(&resume::command(s, Quoting::Posix))
            )
        };
        if !write_line(&mut out, &line)? {
            break;
        }
    }
    Ok(exit(!hits.is_empty()))
}

/// Prints the chats with these ids, whole.
pub fn show(env: &Env, db: &Path, ids: &[String], json: bool) -> Result<ExitCode> {
    let (conn, sessions) = load(env, db)?;
    let tz = TimeZone::system();
    let mut out = io::stdout().lock();
    let mut found_all = true;
    let mut seen = HashSet::new();
    for id in ids.iter().filter(|id| seen.insert(id.as_str())) {
        let chats: Vec<&Session> = sessions.iter().filter(|s| s.sid == *id).collect();
        if chats.is_empty() {
            eprintln!("trove: no chat with id {}", printable(id));
            found_all = false;
        }
        for s in chats {
            let transcript = search::transcript(&conn, s.id)?;
            let text = if json {
                serde_json::to_string(&Shown {
                    chat: Summary::of(s),
                    transcript: transcript.iter().map(Message::of).collect(),
                })?
            } else {
                conversation(s, &transcript, &tz)
            };
            if !write_line(&mut out, &text)? {
                return Ok(exit(found_all));
            }
        }
    }
    Ok(exit(found_all))
}

fn load(env: &Env, db: &Path) -> Result<(Connection, Vec<Session>)> {
    let conn = index::open(db)?;
    index::refresh(&conn, db, env, &Progress::default(), &mut || {})?;
    let sessions = search::load(&conn)?;
    Ok((conn, sessions))
}

fn conversation(s: &Session, transcript: &[Excerpt], tz: &TimeZone) -> String {
    let agent = match s.model.as_str() {
        "" => s.agent.name().to_owned(),
        model => format!("{} ({})", s.agent.name(), s.agent.model_label(model)),
    };
    let about = [
        ("agent", agent),
        ("folder", printable(&s.cwd)),
        ("started", datetime(s.started, tz)),
        ("updated", datetime(s.updated, tz)),
        ("id", s.sid.clone()),
        ("resume", printable(&resume::command(s, Quoting::Posix))),
    ];
    let mut out = format!("# {}\n\n", one_line(&s.title));
    for (key, value) in about.iter().filter(|(_, v)| !v.is_empty()) {
        out.push_str(&format!("{key}: {value}\n"));
    }
    for m in transcript {
        let who = match m.role {
            Role::User => "you",
            Role::Assistant => s.agent.name(),
        };
        out.push_str(&format!("\n## {who}\n\n{}\n", printable_text(&m.text)));
    }
    out
}

/// Writes a line, or returns `false` if the reader has gone.
fn write_line(out: &mut impl Write, line: &str) -> io::Result<bool> {
    match writeln!(out, "{line}") {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(false),
        Err(e) => Err(e),
    }
}

fn exit(found: bool) -> ExitCode {
    if found {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[derive(Serialize)]
struct Summary<'a> {
    id: &'a str,
    agent: &'static str,
    model: Option<&'a str>,
    folder: &'a str,
    title: &'a str,
    started: Option<String>,
    updated: Option<String>,
    messages: i64,
    archived: bool,
    file: &'a Path,
    resume: String,
}

impl<'a> Summary<'a> {
    fn of(s: &'a Session) -> Summary<'a> {
        Summary {
            id: &s.sid,
            agent: s.agent.name(),
            model: Some(s.model.as_str()).filter(|m| !m.is_empty()),
            folder: &s.cwd,
            title: &s.title,
            started: iso(s.started),
            updated: iso(s.updated),
            messages: s.messages,
            archived: s.archived,
            file: &s.file,
            resume: resume::command(s, Quoting::Posix),
        }
    }
}

#[derive(Serialize)]
struct Found<'a> {
    #[serde(flatten)]
    chat: Summary<'a>,
    matches: Vec<Message<'a>>,
}

#[derive(Serialize)]
struct Shown<'a> {
    #[serde(flatten)]
    chat: Summary<'a>,
    transcript: Vec<Message<'a>>,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'static str,
    text: &'a str,
}

impl<'a> Message<'a> {
    fn of(e: &'a Excerpt) -> Message<'a> {
        let role = match e.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        Message {
            role,
            text: &e.text,
        }
    }
}
