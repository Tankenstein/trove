use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, ErrorCode, OptionalExtension, params};

use crate::agents::{Agent, Database, Format, Lines, Role};
use crate::discover::{self, Found};
use crate::env::Env;
use crate::parse::{self, Parsed, State};

/// Bump when parsing changes, so existing indexes are rebuilt.
const VERSION: &str = "12";
/// Lines longer than this are skipped: compacted history and tool output, not chat text.
const BLOCK: usize = 8 << 20;
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);
const COMMIT_EVERY: Duration = Duration::from_millis(300);

const SCHEMA: &str = "
CREATE TABLE meta(k TEXT PRIMARY KEY, v TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE sessions(
    id INTEGER PRIMARY KEY,
    file TEXT NOT NULL,
    chat TEXT NOT NULL DEFAULT '',
    agent TEXT NOT NULL,
    archived INTEGER NOT NULL DEFAULT 0,
    sid TEXT NOT NULL DEFAULT '',
    cwd TEXT NOT NULL DEFAULT '',
    title TEXT NOT NULL DEFAULT '',
    auto_title TEXT NOT NULL DEFAULT '',
    first_prompt TEXT NOT NULL DEFAULT '',
    last_prompt TEXT NOT NULL DEFAULT '',
    model TEXT NOT NULL DEFAULT '',
    started INTEGER NOT NULL DEFAULT 0,
    n_user INTEGER NOT NULL DEFAULT 0,
    n_messages INTEGER NOT NULL DEFAULT 0,
    hidden INTEGER NOT NULL DEFAULT 0,
    updated INTEGER NOT NULL DEFAULT 0,
    ino INTEGER NOT NULL DEFAULT 0,
    size INTEGER NOT NULL DEFAULT 0,
    mtime_ns INTEGER NOT NULL DEFAULT 0,
    offset INTEGER NOT NULL DEFAULT 0,
    version INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL DEFAULT '',
    UNIQUE(file, chat)
);
CREATE VIRTUAL TABLE fts USING fts5(text, tokenize = 'unicode61 remove_diacritics 2', prefix = '1 2');
";

pub fn row_session(rowid: i64) -> i64 {
    rowid >> 24
}

pub fn row_role(rowid: i64) -> Option<Role> {
    match rowid & 3 {
        1 => Some(Role::User),
        2 => Some(Role::Assistant),
        _ => None,
    }
}

// Row ids are `session << 24 | message << 2 | kind` (0 metadata, 1 prompt, 2 reply): a
// session's rows are one range, and a row's role is known without loading its text.
fn meta_row(session: i64) -> i64 {
    session << 24
}

pub fn session_rows(session: i64) -> (i64, i64) {
    (meta_row(session), meta_row(session) | 0xFF_FFFF)
}

pub fn row_message(rowid: i64) -> i64 {
    (rowid >> 2) & 0x3F_FFFF
}

fn message_row(session: i64, n: u32, role: Role) -> i64 {
    let kind = match role {
        Role::User => 1,
        Role::Assistant => 2,
    };
    (session << 24) | ((n as i64) << 2) | kind
}

pub fn default_path() -> Result<PathBuf> {
    Ok(dirs::cache_dir()
        .context("can't find a cache directory")?
        .join("trove")
        .join("index.db"))
}

pub fn open(path: &Path) -> Result<Connection> {
    open_with(path, BUSY_TIMEOUT)
}

fn open_with(path: &Path, busy: Duration) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
        restrict(dir, 0o700);
    }
    match open_checked(path, busy) {
        Err(e) if is_corrupt(&e) => {
            {
                // Unlocked before reopening, which takes the lock again.
                let _lock = lock(path)?;
                for suffix in ["", "-wal", "-shm"] {
                    let _ = fs::remove_file(format!("{}{suffix}", path.display()));
                }
            }
            open_checked(path, busy)
        }
        result => result,
    }
}

fn is_corrupt(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<rusqlite::Error>()
            .and_then(rusqlite::Error::sqlite_error_code),
        Some(ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt)
    )
}

fn lock(db: &Path) -> Result<File> {
    let lock = File::create(db.with_extension("lock"))?;
    lock.lock()?;
    Ok(lock)
}

fn open_checked(path: &Path, busy: Duration) -> Result<Connection> {
    let conn = Connection::open(path)?;
    restrict(path, 0o600);
    conn.busy_timeout(busy)?;
    conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
    conn.execute_batch("PRAGMA synchronous = NORMAL;")?;
    if version(&conn).as_deref() != Some(VERSION) {
        let _lock = lock(path)?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let stale = version(&conn).as_deref() != Some(VERSION);
        if stale {
            conn.execute_batch(
                "DROP TABLE IF EXISTS fts; DROP TABLE IF EXISTS sessions; DROP TABLE IF EXISTS meta;",
            )?;
            conn.execute_batch(SCHEMA)?;
            conn.execute("INSERT INTO meta(k, v) VALUES ('version', ?1)", [VERSION])?;
        }
        conn.execute_batch("COMMIT")?;
        if stale {
            conn.execute_batch("VACUUM")?;
        }
    }
    Ok(conn)
}

fn version(conn: &Connection) -> Option<String> {
    conn.query_row("SELECT v FROM meta WHERE k = 'version'", [], |r| r.get(0))
        .ok()
}

fn restrict(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
}

#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
}

struct Known {
    id: i64,
    archived: bool,
    ino: u64,
    size: u64,
    mtime_ns: i64,
    offset: u64,
    version: i64,
    state: String,
}

/// A chat to read. `file` and `chat` are its key: the chat's file and an empty `chat`, or the
/// database and the chat's id in it.
struct Job {
    id: Option<i64>,
    agent: &'static dyn Agent,
    file: String,
    chat: String,
    source: Source,
    reset: bool,
}

enum Source {
    /// Read from where the last run stopped.
    File {
        lines: &'static dyn Lines,
        found: Found,
        offset: u64,
        state: State,
    },
    /// Read whole.
    Database {
        db: &'static dyn Database,
        version: i64,
    },
}

impl Job {
    fn bytes(&self) -> u64 {
        match &self.source {
            Source::File { found, offset, .. } => found.size.saturating_sub(*offset),
            Source::Database { .. } => 1 << 16,
        }
    }
}

/// What's recorded about a chat's source, to tell next time whether it changed.
#[derive(Default)]
struct Stamp {
    archived: bool,
    updated: i64,
    ino: u64,
    size: u64,
    mtime_ns: i64,
    offset: u64,
    version: i64,
}

pub fn refresh(
    conn: &Connection,
    db_path: &Path,
    env: &Env,
    progress: &Progress,
    on_commit: &mut dyn FnMut(),
) -> Result<()> {
    let _lock = lock(db_path)?;
    let mut disc = discover::discover(env);
    disc.files.sort_by_key(|f| std::cmp::Reverse(f.mtime_ns));
    let mut known = known(conn)?;
    let mut jobs = Vec::new();
    let mut databases = Vec::new();
    for found in disc.files {
        let lines = match found.agent.format() {
            Format::Lines(lines) => lines,
            Format::Database(db) => {
                databases.push((db, found));
                continue;
            }
        };
        let Some(file) = found.path.to_str().map(str::to_owned) else {
            continue;
        };
        let (id, offset, state, reset) = match known.remove(&(file.clone(), String::new())) {
            None => (None, 0, State::default(), false),
            Some(k) => {
                let same_file = k.ino == found.ino && k.archived == found.archived;
                if same_file && k.size == found.size && k.mtime_ns == found.mtime_ns {
                    continue;
                }
                match serde_json::from_str::<State>(&k.state) {
                    Ok(state) if same_file && found.size >= k.offset => {
                        (Some(k.id), k.offset, state, false)
                    }
                    _ => (Some(k.id), 0, State::default(), true),
                }
            }
        };
        jobs.push(Job {
            id,
            agent: found.agent,
            file,
            chat: String::new(),
            source: Source::File {
                lines,
                found,
                offset,
                state,
            },
            reset,
        });
    }

    // A database is listed again only when it or its write-ahead log changed.
    let mut prints = Vec::new();
    for (db, found) in databases {
        let Some(file) = found.path.to_str().map(str::to_owned) else {
            continue;
        };
        let print = fingerprint(&[found.path.clone(), PathBuf::from(format!("{file}-wal"))]);
        let stored: Option<String> = conn
            .query_row(
                "SELECT v FROM meta WHERE k = ?1",
                [format!("db:{file}")],
                |r| r.get(0),
            )
            .optional()?;
        let listed = if stored.as_deref() == Some(&print) {
            None
        } else {
            db.versions(&found.path)
        };
        let Some(listed) = listed else {
            known.retain(|(f, _), _| *f != file);
            continue;
        };
        for (chat, version) in listed {
            let k = known.remove(&(file.clone(), chat.clone()));
            if k.as_ref().is_some_and(|k| k.version == version) {
                continue;
            }
            jobs.push(Job {
                id: k.as_ref().map(|k| k.id),
                agent: found.agent,
                file: file.clone(),
                chat,
                source: Source::Database { db, version },
                reset: k.is_some(),
            });
        }
        prints.push((file, print));
    }
    progress
        .total
        .store(jobs.iter().map(Job::bytes).sum(), Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);

    let mut w = Writer::new(conn, env.home_str());
    w.begin()?;
    for k in known.values() {
        w.delete(k.id)?;
    }

    let next = AtomicUsize::new(0);
    let workers = thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(jobs.len())
        .max(1);
    thread::scope(|scope| -> Result<()> {
        let (tx, rx) = mpsc::sync_channel(64);
        for _ in 0..workers {
            let (tx, jobs, next) = (tx.clone(), &jobs, &next);
            scope.spawn(move || {
                while let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let result = run(job);
                    progress.done.fetch_add(job.bytes(), Ordering::Relaxed);
                    if tx.send((job, result)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        for (job, result) in rx {
            let Ok((parsed, state, stamp)) = result else {
                // Retried next run. A database with an unreadable chat is listed again then.
                if let Source::Database { .. } = job.source {
                    prints.retain(|(file, _)| *file != job.file);
                }
                continue;
            };
            w.apply(job, parsed, state, stamp)?;
            if w.since.elapsed() > COMMIT_EVERY {
                w.commit()?;
                on_commit();
                w.begin()?;
            }
        }
        Ok(())
    })?;

    let files: Vec<PathBuf> = disc.title_files.iter().map(|(_, f)| f.clone()).collect();
    let fingerprint = fingerprint(&files);
    let reindexed = jobs.iter().any(|j| j.id.is_none() || j.reset);
    let stored: Option<String> = conn
        .query_row("SELECT v FROM meta WHERE k = 'title_files'", [], |r| {
            r.get(0)
        })
        .optional()?;
    if reindexed || stored.as_deref() != Some(&fingerprint) {
        w.apply_titles(&disc.title_files)?;
        conn.execute(
            "INSERT OR REPLACE INTO meta(k, v) VALUES ('title_files', ?1)",
            [&fingerprint],
        )?;
    }
    for (file, print) in prints {
        conn.execute(
            "INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)",
            params![format!("db:{file}"), print],
        )?;
    }
    w.commit()?;
    on_commit();
    Ok(())
}

fn known(conn: &Connection) -> Result<HashMap<(String, String), Known>> {
    let mut stmt = conn.prepare(
        "SELECT file, chat, id, archived, ino, size, mtime_ns, offset, version, state FROM sessions",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            (r.get(0)?, r.get(1)?),
            Known {
                id: r.get(2)?,
                archived: r.get(3)?,
                ino: r.get::<_, i64>(4)? as u64,
                size: r.get::<_, i64>(5)? as u64,
                mtime_ns: r.get(6)?,
                offset: r.get::<_, i64>(7)? as u64,
                version: r.get(8)?,
                state: r.get(9)?,
            },
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn run(job: &Job) -> io::Result<(Parsed, State, Stamp)> {
    match &job.source {
        Source::File {
            lines,
            found,
            offset,
            state,
        } => read_file(*lines, found, *offset, state),
        Source::Database { db, version } => {
            let chat = db.read(Path::new(&job.file), &job.chat);
            let chat = chat.ok_or_else(|| io::Error::other("the chat couldn't be read"))?;
            let stamp = Stamp {
                archived: chat.archived,
                updated: chat.updated,
                version: *version,
                ..Stamp::default()
            };
            let (parsed, state) = parse::whole(&job.chat, chat);
            Ok((parsed, state, stamp))
        }
    }
}

fn read_file(
    lines: &dyn Lines,
    found: &Found,
    offset: u64,
    state: &State,
) -> io::Result<(Parsed, State, Stamp)> {
    let mut file = File::open(&found.path)?;
    let len = file.metadata()?.len();
    let offset = offset.min(len);
    let mut state = state.clone();
    let mut parsed = Parsed::default();
    let mut stamp = Stamp {
        archived: found.archived,
        updated: found.mtime_ns / 1_000_000_000,
        ino: found.ino,
        size: found.size,
        mtime_ns: found.mtime_ns,
        offset: len,
        version: 0,
    };
    if state.hidden {
        return Ok((parsed, state, stamp));
    }
    file.seek(SeekFrom::Start(offset))?;
    let block = BLOCK.min((len - offset) as usize).max(1);
    let consumed = read_blocks(file.take(len - offset), block, |data| {
        parse::parse(lines, &found.path, data, &mut state, &mut parsed)
    })?;
    stamp.offset = offset + consumed;
    Ok((parsed, state, stamp))
}

fn read_blocks(
    mut reader: impl Read,
    block: usize,
    mut parse: impl FnMut(&[u8]) -> usize,
) -> io::Result<u64> {
    let mut buf = vec![0; block];
    // A line too long for the block is skipped, but only counts as read once its end is seen.
    let (mut filled, mut total, mut skipped) = (0, 0u64, None);
    loop {
        filled += fill(&mut reader, &mut buf[filled..])?;
        let eof = filled < block;
        let mut start = 0;
        if let Some(len) = skipped {
            match memchr::memchr(b'\n', &buf[..filled]) {
                Some(i) => {
                    total += len;
                    start = i + 1;
                    skipped = None;
                }
                None if eof => return Ok(total),
                None => {
                    skipped = Some(len + filled as u64);
                    filled = 0;
                    continue;
                }
            }
        }
        let used = start + parse(&buf[start..filled]);
        total += used as u64;
        if eof {
            return Ok(total);
        }
        if used == 0 {
            skipped = Some(filled as u64);
            filled = 0;
            continue;
        }
        buf.copy_within(used..filled, 0);
        filled -= used;
    }
}

fn fill(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match reader.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

fn fingerprint(files: &[PathBuf]) -> String {
    let mut out = String::new();
    for f in files {
        if let Ok(m) = fs::metadata(f) {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            out.push_str(&format!("{}:{}:{mtime};", f.display(), m.len()));
        }
    }
    out
}

struct Writer<'c> {
    conn: &'c Connection,
    home: String,
    touched: Vec<i64>,
    since: Instant,
}

impl<'c> Writer<'c> {
    fn new(conn: &'c Connection, home: &str) -> Self {
        Writer {
            conn,
            home: home.to_owned(),
            touched: Vec::new(),
            since: Instant::now(),
        }
    }

    fn begin(&mut self) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        self.since = Instant::now();
        Ok(())
    }

    fn commit(&mut self) -> Result<()> {
        self.touched.sort_unstable();
        self.touched.dedup();
        for id in std::mem::take(&mut self.touched) {
            self.write_meta(id)?;
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    fn clear_rows(&self, id: i64) -> Result<()> {
        self.conn
            .prepare_cached("DELETE FROM fts WHERE rowid BETWEEN ?1 AND ?2")?
            .execute([meta_row(id), meta_row(id) | 0xFF_FFFF])?;
        Ok(())
    }

    fn delete(&mut self, id: i64) -> Result<()> {
        self.clear_rows(id)?;
        self.conn
            .prepare_cached("DELETE FROM sessions WHERE id = ?1")?
            .execute([id])?;
        Ok(())
    }

    fn apply(&mut self, job: &Job, p: Parsed, state: State, stamp: Stamp) -> Result<()> {
        let id = match job.id {
            Some(id) => {
                if job.reset {
                    self.clear_rows(id)?;
                    self.conn
                        .prepare_cached(
                            "UPDATE sessions SET sid = '', cwd = '', title = '', auto_title = '',
                             first_prompt = '', last_prompt = '', model = '', started = 0,
                             n_user = 0, n_messages = 0, hidden = 0 WHERE id = ?1",
                        )?
                        .execute([id])?;
                }
                id
            }
            None => self
                .conn
                .prepare_cached(
                    "INSERT INTO sessions(file, chat, agent) VALUES (?1, ?2, ?3) RETURNING id",
                )?
                .query_row(params![job.file, job.chat, job.agent.name()], |r| r.get(0))?,
        };
        self.conn
            .prepare_cached(
                "UPDATE sessions SET
                    archived = ?2,
                    sid = CASE WHEN ?3 <> '' THEN ?3 ELSE sid END,
                    cwd = ?4,
                    title = CASE WHEN ?5 <> '' THEN ?5 ELSE title END,
                    auto_title = CASE WHEN ?6 <> '' THEN ?6 ELSE auto_title END,
                    first_prompt = CASE WHEN first_prompt = '' THEN ?7 ELSE first_prompt END,
                    n_user = n_user + ?8,
                    hidden = ?9,
                    updated = ?10, ino = ?11, size = ?12, mtime_ns = ?13, offset = ?14, state = ?15,
                    last_prompt = CASE WHEN ?16 <> '' THEN ?16 ELSE last_prompt END,
                    model = CASE WHEN ?17 <> '' THEN ?17 ELSE model END,
                    started = CASE WHEN started = 0 THEN ?18 ELSE started END,
                    n_messages = n_messages + ?19,
                    version = ?20
                 WHERE id = ?1",
            )?
            .execute(params![
                id,
                stamp.archived,
                p.sid.unwrap_or_default(),
                state.cwd,
                p.title.unwrap_or_default(),
                p.auto_title.unwrap_or_default(),
                p.first_prompt.unwrap_or_default(),
                p.n_user,
                state.hidden,
                stamp.updated,
                stamp.ino as i64,
                stamp.size as i64,
                stamp.mtime_ns,
                stamp.offset as i64,
                serde_json::to_string(&state)?,
                p.last_prompt.unwrap_or_default(),
                p.model.unwrap_or_default(),
                p.started.unwrap_or(0),
                p.messages.len() as i64,
                stamp.version,
            ])?;
        let mut insert = self
            .conn
            .prepare_cached("INSERT INTO fts(rowid, text) VALUES (?1, ?2)")?;
        for (n, role, text) in &p.messages {
            insert.execute(params![message_row(id, *n, *role), text])?;
        }
        self.touched.push(id);
        Ok(())
    }

    fn write_meta(&self, id: i64) -> Result<()> {
        let row: Option<(String, String, String, String, String)> = self
            .conn
            .prepare_cached(
                "SELECT agent, title, auto_title, cwd, model FROM sessions WHERE id = ?1",
            )?
            .query_row([id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .optional()?;
        let Some((agent, title, auto_title, cwd, model)) = row else {
            return Ok(());
        };
        let folder = cwd.strip_prefix(&self.home).unwrap_or(&cwd);
        let text = format!("{title}\n{auto_title}\n{folder}\n{agent} {model}");
        let rowid = meta_row(id);
        let old: Option<String> = self
            .conn
            .prepare_cached("SELECT text FROM fts WHERE rowid = ?1")?
            .query_row([rowid], |r| r.get(0))
            .optional()?;
        if old.as_deref() == Some(text.as_str()) {
            return Ok(());
        }
        self.conn
            .prepare_cached("DELETE FROM fts WHERE rowid = ?1")?
            .execute([rowid])?;
        self.conn
            .prepare_cached("INSERT INTO fts(rowid, text) VALUES (?1, ?2)")?
            .execute(params![rowid, text])?;
        Ok(())
    }

    fn apply_titles(&mut self, files: &[(&'static dyn Agent, PathBuf)]) -> Result<()> {
        let mut titles = HashMap::new();
        for (agent, file) in files {
            let Ok(data) = fs::read(file) else { continue };
            for (sid, title) in agent.titles(&data) {
                titles.insert((agent.name(), sid), title);
            }
        }
        let mut update = self.conn.prepare_cached(
            "UPDATE sessions SET title = ?1 WHERE agent = ?2 AND sid = ?3 AND title <> ?1 RETURNING id",
        )?;
        for ((agent, sid), title) in &titles {
            let ids = update.query_map(params![title, agent, sid], |r| r.get::<_, i64>(0))?;
            for id in ids {
                self.touched.push(id?);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(data: &[u8], block: usize) -> (Vec<String>, u64) {
        let mut seen = Vec::new();
        let total = read_blocks(data, block, |buf| {
            let end = memchr::memrchr(b'\n', buf).map_or(0, |i| i + 1);
            seen.extend(
                crate::agents::lines(&buf[..end]).map(|l| String::from_utf8_lossy(l).into_owned()),
            );
            end
        })
        .unwrap();
        (seen, total)
    }

    #[test]
    fn lines_split_across_blocks_arrive_whole() {
        let data = b"one\ntwo two\nthree three three\nfour\n";
        for block in [18, 19, 23, 29, 64] {
            let (seen, total) = blocks(data, block);
            assert_eq!(
                seen,
                ["one", "two two", "three three three", "four"],
                "block {block}"
            );
            assert_eq!(total, data.len() as u64);
        }
    }

    #[test]
    fn a_line_longer_than_a_block_is_skipped() {
        let data = format!("short\n{}\nafter\n", "x".repeat(50));
        let (seen, total) = blocks(data.as_bytes(), 16);
        assert_eq!(seen, ["short", "after"]);
        assert_eq!(total, data.len() as u64);
    }

    #[test]
    fn an_unfinished_last_line_is_left_for_later() {
        for block in [5, 9, 4096] {
            assert_eq!(blocks(b"done\nhalf", block), (vec!["done".into()], 5));
        }
        assert_eq!(blocks(b"half", 4), (Vec::new(), 0));
        let long = format!("done\n{}", "x".repeat(50));
        assert_eq!(blocks(long.as_bytes(), 16), (vec!["done".into()], 5));
    }

    #[test]
    fn reading_ends_cleanly_before_a_block_is_full() {
        let (seen, total) = blocks(b"a\nb\n", 1 << 20);
        assert_eq!((seen.len(), total), (2, 4));
    }

    #[test]
    fn a_busy_index_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.db");
        let conn = open(&path).unwrap();
        conn.execute(
            "INSERT INTO sessions(file, agent) VALUES ('marker', 'claude')",
            [],
        )
        .unwrap();
        conn.execute("UPDATE meta SET v = 'old' WHERE k = 'version'", [])
            .unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();

        let err = open_with(&path, Duration::from_millis(50)).unwrap_err();
        assert!(!is_corrupt(&err), "{err:#}");
        conn.execute_batch("COMMIT").unwrap();
        let marker: i64 = conn
            .query_row(
                "SELECT count(*) FROM sessions WHERE file = 'marker'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(marker, 1, "the index was thrown away");
    }
}
