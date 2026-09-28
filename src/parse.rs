use std::path::Path;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::agents::{Chat, Event, Lines, Msg, Role, clean, lines, prompt, truncate};

const MAX_MSGS: u32 = (1 << 22) - 1;
const CHUNK: usize = 2 << 20;
const RECENT: usize = 64;

/// What's kept between reads of a chat's file.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct State {
    pub next_msg: u32,
    pub cwd: String,
    pub hidden: bool,
    pub recent: Vec<u64>,
}

#[derive(Default, Debug)]
pub struct Parsed {
    pub messages: Vec<(u32, Role, String)>,
    pub sid: Option<String>,
    pub title: Option<String>,
    pub auto_title: Option<String>,
    pub first_prompt: Option<String>,
    pub n_user: i64,
    pub model: Option<String>,
    pub started: Option<i64>,
    pub last_prompt: Option<String>,
}

pub fn parse(
    format: &dyn Lines,
    file: &Path,
    data: &[u8],
    state: &mut State,
    out: &mut Parsed,
) -> usize {
    let end = complete_len(data);
    let body = &data[..end];
    let header = format.header(file, body);
    if out.sid.is_none() {
        out.sid = format.session_id(file).or(header.id);
    }
    if let Some(cwd) = header.cwd {
        state.cwd = cwd;
    }
    state.hidden |= header.hidden;
    out.started = out.started.or(header.started);
    out.model = header.model.or(out.model.take());
    out.title = header.title.or(out.title.take());
    out.auto_title = header.auto_title.or(out.auto_title.take());
    if state.hidden {
        return end;
    }

    let events: Vec<Event> = if body.len() > CHUNK {
        let parts: Vec<Vec<Event>> = chunks(body, CHUNK)
            .par_iter()
            .map(|c| scan(format, c))
            .collect();
        parts.into_iter().flatten().collect()
    } else {
        scan(format, body)
    };

    for event in events {
        match event {
            Event::Msg(Msg {
                role,
                text,
                at,
                model,
                folder,
            }) => {
                if format.repeats_messages() {
                    let h = hash(role, &text);
                    if state.recent.contains(&h) {
                        continue;
                    }
                    if state.recent.len() == RECENT {
                        state.recent.remove(0);
                    }
                    state.recent.push(h);
                }
                if state.cwd.is_empty()
                    && let Some(folder) = folder
                {
                    state.cwd = folder;
                }
                out.started = out.started.or(at);
                if model.is_some() {
                    out.model = model;
                }
                record(out, state, role, text);
            }
            Event::Title(t) => out.title = Some(t),
            Event::AutoTitle(t) => out.auto_title = Some(t),
            Event::Model(m) => out.model = Some(m),
        }
    }
    end
}

fn scan(format: &dyn Lines, body: &[u8]) -> Vec<Event> {
    lines(body).filter_map(|line| format.line(line)).collect()
}

fn record(out: &mut Parsed, state: &mut State, role: Role, text: String) {
    if role == Role::User {
        out.n_user += 1;
        let line = first_line(&text);
        out.first_prompt.get_or_insert_with(|| line.clone());
        out.last_prompt = Some(line);
    }
    if state.next_msg < MAX_MSGS {
        state.next_msg += 1;
        out.messages.push((state.next_msg, role, text));
    }
}

/// A chat read whole from a database, as `parse` would have found it reading a file from the start.
pub fn whole(id: &str, chat: Chat) -> (Parsed, State) {
    let mut state = State {
        cwd: chat.cwd,
        hidden: chat.hidden,
        ..State::default()
    };
    let mut out = Parsed {
        sid: Some(id.to_owned()),
        title: chat.title,
        model: chat.model,
        started: chat.started,
        ..Parsed::default()
    };
    if chat.hidden {
        return (out, state);
    }
    for (role, text) in chat.messages {
        let text = match role {
            Role::User => prompt(&text),
            Role::Assistant => clean(&text),
        };
        if let Some(text) = text {
            record(&mut out, &mut state, role, text);
        }
    }
    (out, state)
}

/// A final line without a newline counts only if it's valid JSON; otherwise it's still being
/// written.
fn complete_len(data: &[u8]) -> usize {
    let start = match memchr::memrchr(b'\n', data) {
        Some(i) if i + 1 == data.len() => return data.len(),
        Some(i) => i + 1,
        None => 0,
    };
    let tail = &data[start..];
    let whole = !tail.iter().all(u8::is_ascii_whitespace)
        && serde_json::from_slice::<serde::de::IgnoredAny>(tail).is_ok();
    if whole { data.len() } else { start }
}

fn chunks(body: &[u8], size: usize) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < body.len() {
        let mut end = (start + size).min(body.len());
        if end < body.len() {
            end = memchr::memchr(b'\n', &body[end..]).map_or(body.len(), |i| end + i + 1);
        }
        out.push(&body[start..end]);
        start = end;
    }
    out
}

/// FNV-1a: stable across Rust versions, because the hashes are stored.
fn hash(role: Role, text: &str) -> u64 {
    let tag: &[u8] = match role {
        Role::User => b"u",
        Role::Assistant => b"a",
    };
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in tag.iter().chain(text.as_bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    truncate(line, 300).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_last_line_is_left_for_later() {
        assert_eq!(complete_len(b"{\"a\":1}\n{\"b\":"), 8);
        assert_eq!(complete_len(b"{\"a\":1}\n{\"b\":2}"), 15);
        assert_eq!(complete_len(b"{\"a\":1}\n"), 8);
        assert_eq!(complete_len(b""), 0);
        assert_eq!(complete_len(b"{\"a\""), 0);
    }

    #[test]
    fn chunks_end_on_newlines() {
        let body = b"aaaa\nbb\ncccccc\nd\n";
        let parts = chunks(body, 3);
        assert_eq!(parts.concat(), body);
        assert!(parts.iter().all(|p| p.ends_with(b"\n")));
    }
}
