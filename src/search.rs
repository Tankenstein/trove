use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

use crate::agents::{self, Agent, Role};
use crate::index::{row_role, row_session, session_rows};

#[derive(Clone, Debug)]
pub struct Session {
    pub id: i64,
    pub agent: &'static dyn Agent,
    pub sid: String,
    pub cwd: String,
    /// The chat's file, or the database it's in.
    pub file: PathBuf,
    pub title: String,
    pub last_prompt: String,
    pub model: String,
    pub messages: i64,
    pub started: i64,
    pub updated: i64,
    pub archived: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub idx: usize,
    pub rows: Vec<i64>,
}

const MATCHES: usize = 3;

pub struct Excerpt {
    pub row: i64,
    pub role: Role,
    pub text: String,
}

impl Excerpt {
    fn new(row: i64, text: String) -> Excerpt {
        let role = row_role(row).unwrap_or(Role::Assistant);
        Excerpt { row, role, text }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    pub terms: Vec<String>,
    pub tokens: Vec<String>,
}

pub fn load(conn: &Connection) -> Result<Vec<Session>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, agent, sid, cwd, title, auto_title, first_prompt, updated, archived,
                last_prompt, model, n_messages, started, file FROM sessions
         WHERE hidden = 0 AND n_user > 0 ORDER BY updated DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        let title: String = r.get(4)?;
        let auto_title: String = r.get(5)?;
        let first_prompt: String = r.get(6)?;
        let title = [title, auto_title, first_prompt]
            .into_iter()
            .find(|t| !t.is_empty())
            .unwrap_or_default();
        let Some(agent) = agents::by_name(r.get_ref(1)?.as_str()?) else {
            return Ok(None);
        };
        Ok(Some(Session {
            id: r.get(0)?,
            agent,
            sid: r.get(2)?,
            cwd: r.get(3)?,
            title,
            updated: r.get(7)?,
            archived: r.get(8)?,
            last_prompt: r.get(9)?,
            model: r.get(10)?,
            messages: r.get(11)?,
            started: r.get(12)?,
            file: PathBuf::from(r.get::<_, String>(13)?),
        }))
    })?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for s in rows {
        let Some(s) = s? else { continue };
        if plain_id(&s.sid) && seen.insert((s.agent.name(), s.sid.clone())) {
            out.push(s);
        }
    }
    Ok(out)
}

/// Ids come from the agents' files and are passed to their CLIs, so one that could pass for an
/// option isn't resumable.
fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
}

pub fn query(conn: &Connection, text: &str) -> Result<Query> {
    let mut exists = conn.prepare_cached("SELECT 1 FROM fts WHERE fts MATCH ?1 LIMIT 1")?;
    let mut q = Query::default();
    for word in words(text) {
        let forms = forms(&word);
        for (i, form) in forms.iter().enumerate() {
            let tokens: Vec<String> = tokens(form).collect();
            if tokens.is_empty() {
                continue;
            }
            let term = format!("\"{}\"*", tokens.join(" "));
            if i + 1 == forms.len() || exists.exists([&term])? {
                q.terms.push(term);
                q.tokens.extend(tokens);
                break;
            }
        }
    }
    Ok(q)
}

fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for c in text.chars() {
        if c == '"' || c.is_whitespace() && !quoted {
            quoted ^= c == '"';
            if !word.trim().is_empty() {
                out.push(std::mem::take(&mut word));
            }
            word.clear();
        } else {
            word.push(c);
        }
    }
    if !word.trim().is_empty() {
        out.push(word);
    }
    out
}

fn forms(word: &str) -> Vec<String> {
    let url = match word.split_once("://") {
        Some((_, rest)) => rest,
        None => {
            let host = word.split(['/', '?', '#']).next().unwrap_or("");
            if host.len() == word.len() || !host.contains('.') || host.starts_with('.') {
                return vec![word.to_owned()];
            }
            word
        }
    };
    let base = url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    let mut parts: Vec<&str> = base.split('/').collect();
    let mut out = vec![parts.join("/")];
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    while parts.len() > 3 && !parts.last().is_some_and(|p| numeric(p)) {
        parts.pop();
        out.push(parts.join("/"));
    }
    out
}

/// Words in more messages than this only filter: scoring them is slow and says little.
const SCORED_ROWS: usize = 3000;

#[derive(Default)]
struct Acc {
    matched: usize,
    current: f64,
    total: f64,
    top: Vec<(f64, i64)>,
}

impl Acc {
    fn offer(&mut self, score: f64, row: i64) {
        if self.top.iter().any(|&(_, r)| r == row) {
            return;
        }
        if self.top.len() < MATCHES {
            self.top.push((score, row));
        } else if let Some(worst) = self.top.iter_mut().min_by(|a, b| a.0.total_cmp(&b.0))
            && score > worst.0
        {
            *worst = (score, row);
        }
    }
}

pub fn search(
    conn: &Connection,
    sessions: &[Session],
    query: &Query,
    here: &Path,
    now: i64,
) -> Result<Vec<Hit>> {
    let here = normalize(&here.to_string_lossy());
    let near = |s: &Session| Path::new(&normalize(&s.cwd)).starts_with(&here);
    let terms = &query.terms;
    if terms.is_empty() {
        let mut hits: Vec<Hit> = (0..sessions.len())
            .map(|idx| Hit {
                idx,
                rows: Vec::new(),
            })
            .collect();
        hits.sort_by_key(|h| !near(&sessions[h.idx]));
        return Ok(hits);
    }

    let pos: HashMap<i64, usize> = sessions
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id, i))
        .collect();
    let mut acc: HashMap<usize, Acc> = HashMap::new();
    let mut matching = conn.prepare_cached("SELECT rowid FROM fts WHERE fts MATCH ?1")?;
    let mut ranked = conn.prepare_cached("SELECT rowid, bm25(fts) FROM fts WHERE fts MATCH ?1")?;
    for (word, term) in terms.iter().enumerate() {
        let rows: Vec<i64> = matching
            .query_map([term], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let rows: Vec<(i64, f64)> = if rows.len() > SCORED_ROWS {
            rows.into_iter().map(|row| (row, 0.0)).collect()
        } else {
            ranked
                .query_map([term], |r| Ok((r.get(0)?, -r.get::<_, f64>(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (rowid, rank) in rows {
            let Some(&idx) = pos.get(&row_session(rowid)) else {
                continue;
            };
            let role = row_role(rowid);
            let score = rank * weight(role);
            let a = if word == 0 {
                acc.entry(idx).or_default()
            } else {
                match acc.get_mut(&idx) {
                    Some(a) => a,
                    None => continue,
                }
            };
            if a.matched == word {
                a.matched = word + 1;
                a.current = score;
            } else {
                a.current = a.current.max(score);
            }
            if role.is_some() {
                a.offer(score, rowid);
            }
        }
        acc.retain(|_, a| a.matched == word + 1);
        for a in acc.values_mut() {
            a.total += a.current;
        }
    }

    let top = acc
        .values()
        .map(|a| a.total)
        .fold(f64::MIN_POSITIVE, f64::max);
    let longest = acc
        .keys()
        .map(|&idx| sessions[idx].messages)
        .max()
        .unwrap_or(0);
    let size = |s: &Session| ((1 + s.messages) as f64).ln() / ((1 + longest) as f64).ln().max(1.0);
    let mut scored: Vec<(f64, Hit)> = acc
        .into_iter()
        .map(|(idx, mut a)| {
            let s = &sessions[idx];
            let days = (now - s.updated).max(0) as f64 / 86_400.0;
            let score = a.total / top
                + 0.3 * (-days / 30.0).exp()
                + 0.1 * size(s)
                + if near(s) { 0.2 } else { 0.0 };
            a.top
                .sort_by(|x, y| y.0.total_cmp(&x.0).then(x.1.cmp(&y.1)));
            let rows = a.top.into_iter().map(|(_, row)| row).collect();
            (score, Hit { idx, rows })
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.total_cmp(&a.0).then(
            sessions[a.1.idx]
                .updated
                .cmp(&sessions[b.1.idx].updated)
                .reverse(),
        )
    });
    Ok(scored.into_iter().map(|(_, h)| h).collect())
}

fn weight(role: Option<Role>) -> f64 {
    match role {
        None => 3.0,
        Some(Role::User) => 1.5,
        Some(Role::Assistant) => 1.0,
    }
}

pub fn snippet(conn: &Connection, query: &Query, row: i64) -> Result<Option<Excerpt>> {
    let text = text(conn, row)?;
    Ok(text.map(|text| Excerpt::new(row, excerpt(&text, &query.tokens, 300))))
}

fn text(conn: &Connection, row: i64) -> Result<Option<String>> {
    Ok(conn
        // By rowid alone: adding MATCH here makes FTS5 walk every occurrence of a common word.
        .prepare_cached("SELECT text FROM fts WHERE rowid = ?1")?
        .query_row([row], |r| r.get(0))
        .optional()?)
}

fn excerpt(text: &str, tokens: &[String], len: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = chars
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    let at = (0..lower.len())
        .find(|&i| {
            (i == 0 || !lower[i - 1].is_alphanumeric())
                && tokens.iter().any(|t| {
                    lower[i..]
                        .iter()
                        .copied()
                        .take(t.chars().count())
                        .eq(t.chars())
                })
        })
        .unwrap_or(0);
    let mut start = at.saturating_sub((len / 3).min(40));
    if start > 0 {
        start = (start..at)
            .find(|&i| chars[i].is_whitespace())
            .map_or(at, |i| i + 1);
    }
    let end = (start + len).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out
}

pub fn preview(
    conn: &Connection,
    query: &Query,
    session: i64,
    matches: &[i64],
) -> Result<Vec<Excerpt>> {
    let (meta, last) = session_rows(session);
    let range = (meta + 1, last);
    let mut out = BTreeMap::new();
    for sql in [
        "SELECT rowid, text FROM fts WHERE rowid BETWEEN ?1 AND ?2 ORDER BY rowid LIMIT 2",
        "SELECT rowid, text FROM fts WHERE rowid BETWEEN ?1 AND ?2 ORDER BY rowid DESC LIMIT 6",
    ] {
        let mut stmt = conn.prepare_cached(sql)?;
        for row in stmt.query_map(range, |r| Ok((r.get(0)?, r.get(1)?)))? {
            let (row, text) = row?;
            out.insert(row, Excerpt::new(row, text));
        }
    }
    let mut around = conn.prepare_cached(
        "SELECT rowid, text FROM fts WHERE rowid BETWEEN ?1 AND ?2 AND rowid & 3 <> 0",
    )?;
    for &row in matches {
        let (from, to) = ((row & !3) - 4, (row & !3) + 7);
        for neighbour in around.query_map([from.max(range.0), to.min(range.1)], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })? {
            let (n, text): (i64, String) = neighbour?;
            let text = if n == row {
                excerpt(&text, &query.tokens, 1200)
            } else {
                text
            };
            out.insert(n, Excerpt::new(n, text));
        }
    }
    Ok(out.into_values().collect())
}

/// Every prompt and reply of a chat, in order.
pub fn transcript(conn: &Connection, session: i64) -> Result<Vec<Excerpt>> {
    let (meta, last) = session_rows(session);
    let mut stmt = conn.prepare_cached(
        "SELECT rowid, text FROM fts WHERE rowid BETWEEN ?1 AND ?2 ORDER BY rowid",
    )?;
    let rows = stmt.query_map([meta + 1, last], |r| Ok(Excerpt::new(r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn normalize(path: &str) -> String {
    if cfg!(target_os = "macos") {
        path.to_lowercase()
    } else {
        path.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpts_start_near_the_match() {
        let text = format!(
            "{} the needle is here and more follows",
            "filler words ".repeat(20)
        );
        let e = excerpt(&text, &["needle".into()], 30);
        assert!(
            e.starts_with('…') && e.contains("needle") && e.ends_with('…'),
            "{e}"
        );
        assert!(e.find("needle").unwrap() < 45, "{e}");
        assert_eq!(
            excerpt("Needle first", &["needle".into()], 100),
            "Needle first"
        );
        assert_eq!(excerpt("no match here", &["zzz".into()], 5), "no ma…");
    }

    #[test]
    fn ids() {
        assert!(plain_id("0f1e2d3c-4b5a-4968-8778-a6b5c4d3e2f1"));
        assert!(plain_id("ses_3f2aB"));
        assert!(!plain_id(""));
        assert!(!plain_id("--yolo"));
        assert!(!plain_id("a b"));
        assert!(!plain_id("a;b"));
    }

    #[test]
    fn query_words() {
        assert_eq!(words("stripe  Webhook"), ["stripe", "Webhook"]);
        assert_eq!(
            words(r#"fix "retry path" now"#),
            ["fix", "retry path", "now"]
        );
        assert_eq!(words(r#""unfinished quote"#), ["unfinished quote"]);
        assert_eq!(words(r#"" ""#), Vec::<String>::new());
    }

    #[test]
    fn url_forms() {
        assert_eq!(forms("pull/501"), ["pull/501"]);
        assert_eq!(forms("src/main.rs"), ["src/main.rs"]);
        assert_eq!(forms("main.rs"), ["main.rs"]);
        assert_eq!(
            forms("https://github.com/acme/shop/pull/501/files?w=1#diff-9"),
            [
                "github.com/acme/shop/pull/501/files",
                "github.com/acme/shop/pull/501"
            ]
        );
        assert_eq!(
            forms("https://github.com/acme/shop/pull/999"),
            ["github.com/acme/shop/pull/999"]
        );
        assert_eq!(
            forms("https://github.com/acme/shop/blob/main/src/x.rs#L10"),
            [
                "github.com/acme/shop/blob/main/src/x.rs",
                "github.com/acme/shop/blob/main/src",
                "github.com/acme/shop/blob/main",
                "github.com/acme/shop/blob",
                "github.com/acme/shop",
            ]
        );
        assert_eq!(forms("github.com/acme/shop/"), ["github.com/acme/shop"]);
        assert_eq!(forms("http://localhost:3000/api"), ["localhost:3000/api"]);
    }
}
