use std::collections::HashMap;
use std::io::{self, Stdout};
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use jiff::tz::TimeZone;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::SetCursorStyle;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use ratatui::crossterm::{execute, terminal};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};
use rusqlite::Connection;
use unicode_width::UnicodeWidthStr;

use crate::agents::{AGENTS, Agent};
use crate::display::{age, day, now, one_line, thousands};
use crate::env::Env;
use crate::index::{self, Progress};
use crate::search::{self, Excerpt, Hit, Query, Session};
use crate::text;

mod preview;

const HEIGHT_PERCENT: u32 = 70;
const MIN_HEIGHT: u16 = 16;
const SIDE_BY_SIDE: u16 = 130;
const SHOW_PROGRESS_AFTER: Duration = Duration::from_millis(300);

pub enum Outcome {
    Picked(Session),
    Quit,
    Interrupted,
}

enum Update {
    Committed,
    Done(Option<String>),
}

pub fn pick(env: &Env, db: &Path, query: &str) -> Result<Outcome> {
    let conn = index::open(db)?;
    let progress = Arc::new(Progress::default());
    let (tx, rx) = mpsc::channel();
    {
        let (env, db, progress) = (env.clone(), db.to_owned(), progress.clone());
        thread::spawn(move || {
            let result = index::open(&db).and_then(|conn| {
                index::refresh(&conn, &db, &env, &progress, &mut || {
                    let _ = tx.send(Update::Committed);
                })
            });
            let _ = tx.send(Update::Done(result.err().map(|e| format!("{e:#}"))));
        });
    }

    let mut app = App::new(&conn, env, query, progress)?;
    let raw = RawMode::enable()?;
    let rows = terminal::size()?.1;
    let height = ((rows as u32 * HEIGHT_PERCENT / 100) as u16)
        .max(MIN_HEIGHT)
        .min(rows.saturating_sub(1))
        .max(3);
    let mut term = Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        TerminalOptions {
            viewport: Viewport::Inline(height),
        },
    )?;
    let outcome = app.run(&mut term, &rx);
    let _ = term.clear();
    let _ = term.show_cursor();
    drop(raw);
    if let Some(err) = &app.error {
        eprintln!("trove: couldn't update the index: {err}");
    }
    outcome
}

struct RawMode;

impl RawMode {
    fn enable() -> io::Result<RawMode> {
        let default_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            RawMode::restore();
            default_hook(info);
        }));
        terminal::enable_raw_mode()?;
        let raw = RawMode;
        // Bracketed paste, so a pasted newline arrives as text, not as Enter.
        execute!(
            io::stdout(),
            EnableBracketedPaste,
            SetCursorStyle::SteadyBar
        )?;
        Ok(raw)
    }

    fn restore() {
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            SetCursorStyle::DefaultUserShape
        );
        let _ = terminal::disable_raw_mode();
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        RawMode::restore();
    }
}

struct Layout {
    query: Rect,
    rule: Rect,
    list: Rect,
    divider: Option<Rect>,
    preview: Option<Rect>,
}

impl Layout {
    fn new(area: Rect) -> Layout {
        let query = Rect {
            y: area.y + 1,
            height: 1,
            ..area
        };
        let rule = Rect {
            y: area.y + 2,
            height: 1,
            ..area
        };
        let body = Rect {
            y: area.y + 3,
            height: area.height.saturating_sub(3),
            ..area
        };
        if area.width >= SIDE_BY_SIDE {
            let list = body.width * 55 / 100;
            Layout {
                query,
                rule,
                list: Rect {
                    width: list - 1,
                    ..body
                },
                divider: Some(Rect {
                    x: body.x + list,
                    width: 1,
                    ..body
                }),
                preview: Some(Rect {
                    x: body.x + list + 2,
                    width: body.width - list - 2,
                    ..body
                }),
            }
        } else if body.height >= 14 {
            let preview = (body.height / 2).clamp(6, 16);
            let list = body.height - preview - 1;
            Layout {
                query,
                rule,
                list: Rect {
                    height: list,
                    ..body
                },
                divider: Some(Rect {
                    y: body.y + list,
                    height: 1,
                    ..body
                }),
                preview: Some(Rect {
                    y: body.y + list + 1,
                    height: preview,
                    ..body
                }),
            }
        } else {
            Layout {
                query,
                rule,
                list: body,
                divider: None,
                preview: None,
            }
        }
    }

    fn per_page(&self) -> usize {
        ((self.list.height as usize + 1) / 3).max(1)
    }

    fn beside(&self) -> bool {
        self.divider.is_some_and(|d| d.width == 1)
    }
}

struct App<'c> {
    conn: &'c Connection,
    home: String,
    here: PathBuf,
    now: i64,
    tz: TimeZone,
    sessions: Vec<Session>,
    hits: Vec<Hit>,
    text: String,
    query: Query,
    stale: bool,
    selected: usize,
    offset: usize,
    snippets: HashMap<i64, Option<Excerpt>>,
    preview: Option<(i64, Vec<Excerpt>)>,
    gone: HashMap<i64, bool>,
    indexing: bool,
    started: Instant,
    progress: Arc<Progress>,
    error: Option<String>,
}

impl<'c> App<'c> {
    fn new(conn: &'c Connection, env: &Env, text: &str, progress: Arc<Progress>) -> Result<Self> {
        let mut app = App {
            conn,
            home: env.home_str().to_owned(),
            here: std::env::current_dir().unwrap_or_default(),
            now: now(),
            tz: TimeZone::system(),
            sessions: search::load(conn)?,
            hits: Vec::new(),
            text: text.to_owned(),
            query: Query::default(),
            stale: false,
            selected: 0,
            offset: 0,
            snippets: HashMap::new(),
            preview: None,
            gone: HashMap::new(),
            indexing: true,
            started: Instant::now(),
            progress,
            error: None,
        };
        app.search()?;
        Ok(app)
    }

    fn run(
        &mut self,
        term: &mut Terminal<CrosstermBackend<Stdout>>,
        rx: &Receiver<Update>,
    ) -> Result<Outcome> {
        let mut dirty = true;
        loop {
            if dirty {
                let area = term.get_frame().area();
                let layout = Layout::new(area);
                self.prepare(&layout)?;
                term.draw(|f| self.draw(f, &layout))?;
            }
            let (timeout, progress) = if self.indexing {
                (
                    Duration::from_millis(100),
                    self.started.elapsed() > SHOW_PROGRESS_AFTER,
                )
            } else {
                (Duration::from_secs(60), false)
            };
            dirty = progress;
            if event::poll(timeout)? {
                loop {
                    match event::read()? {
                        Event::Key(key) if key.kind != KeyEventKind::Release => {
                            let page = Layout::new(term.get_frame().area()).per_page();
                            if let Some(outcome) = self.key(key, page)? {
                                return Ok(outcome);
                            }
                        }
                        Event::Paste(text) => self.edit(|q| q.push_str(&one_line(&text))),
                        _ => {}
                    }
                    dirty = true;
                    if !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
            }
            if self.stale {
                self.search()?;
            }
            let mut reload = false;
            while let Ok(update) = rx.try_recv() {
                reload = true;
                if let Update::Done(err) = update {
                    self.indexing = false;
                    self.error = err;
                }
            }
            if reload {
                self.reload()?;
                dirty = true;
            }
        }
    }

    fn key(&mut self, key: KeyEvent, page: usize) -> Result<Option<Outcome>> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let navigates = matches!(
            key.code,
            KeyCode::Enter | KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
        ) || ctrl && matches!(key.code, KeyCode::Char('p' | 'n'));
        // Enter or arrows right after typing act on what was typed.
        if navigates && self.stale {
            self.search()?;
        }
        let page = page as isize;
        match key.code {
            KeyCode::Esc => return Ok(Some(Outcome::Quit)),
            KeyCode::Char('c') if ctrl => return Ok(Some(Outcome::Interrupted)),
            KeyCode::Enter => {
                if let Some(hit) = self.hits.get(self.selected) {
                    return Ok(Some(Outcome::Picked(self.sessions[hit.idx].clone())));
                }
            }
            KeyCode::Up => self.step(-1),
            KeyCode::Down => self.step(1),
            KeyCode::Char('p') if ctrl => self.step(-1),
            KeyCode::Char('n') if ctrl => self.step(1),
            KeyCode::PageUp => self.step(-page),
            KeyCode::PageDown => self.step(page),
            KeyCode::Backspace => self.edit(|q| {
                q.pop();
            }),
            KeyCode::Char('h') if ctrl => self.edit(|q| {
                q.pop();
            }),
            KeyCode::Char('u') if ctrl => self.edit(String::clear),
            KeyCode::Char('w') if ctrl => self.edit(|q| {
                q.truncate(q.trim_end().len());
                q.truncate(q.rfind(char::is_whitespace).map_or(0, |i| i + 1));
            }),
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.edit(|q| q.push(c))
            }
            _ => {}
        }
        Ok(None)
    }

    fn step(&mut self, by: isize) {
        let last = self.hits.len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + by).clamp(0, last) as usize;
    }

    fn edit(&mut self, f: impl FnOnce(&mut String)) {
        let before = self.text.clone();
        f(&mut self.text);
        self.stale |= self.text != before;
    }

    fn search(&mut self) -> Result<()> {
        self.stale = false;
        self.query = search::query(self.conn, &self.text)?;
        self.hits = search::search(self.conn, &self.sessions, &self.query, &self.here, self.now)?;
        self.snippets.clear();
        self.preview = None;
        self.selected = 0;
        self.offset = 0;
        Ok(())
    }

    fn reload(&mut self) -> Result<()> {
        let keep = self
            .hits
            .get(self.selected)
            .map(|h| self.sessions[h.idx].id);
        self.sessions = search::load(self.conn)?;
        self.search()?;
        if let Some(id) = keep {
            self.selected = self
                .hits
                .iter()
                .position(|h| self.sessions[h.idx].id == id)
                .unwrap_or(0);
        }
        Ok(())
    }

    fn selected_session(&self) -> Option<&Session> {
        self.hits.get(self.selected).map(|h| &self.sessions[h.idx])
    }

    fn prepare(&mut self, layout: &Layout) -> Result<()> {
        let page = layout.per_page();
        if self.selected < self.offset {
            self.offset = self.selected;
        }
        if self.selected >= self.offset + page {
            self.offset = self.selected + 1 - page;
        }
        for i in self.offset..(self.offset + page).min(self.hits.len()) {
            let hit = &self.hits[i];
            let s = &self.sessions[hit.idx];
            self.gone
                .entry(s.id)
                .or_insert_with(|| !Path::new(&s.cwd).is_dir());
            if let Some(&row) = hit.rows.first()
                && !self.snippets.contains_key(&row)
            {
                let snippet = search::snippet(self.conn, &self.query, row)?;
                self.snippets.insert(row, snippet);
            }
        }
        if layout.preview.is_some()
            && let Some(hit) = self.hits.get(self.selected)
            && let id = self.sessions[hit.idx].id
            && self.preview.as_ref().is_none_or(|(p, _)| *p != id)
        {
            let excerpts = search::preview(self.conn, &self.query, id, &hit.rows)?;
            self.preview = Some((id, excerpts));
        }
        Ok(())
    }

    fn draw(&self, f: &mut Frame, layout: &Layout) {
        let status = self.status();
        let width = layout.query.width as usize;
        let (text, style) = if self.text.is_empty() {
            ("Search chats".to_owned(), dim())
        } else {
            (self.text.clone(), Style::new())
        };
        let pad = width.saturating_sub(2 + text.width() + status.width() + 1);
        let query = Line::from(vec![
            Span::raw("  "),
            Span::styled(text, style),
            Span::raw(" ".repeat(pad)),
            Span::styled(status, dim()),
        ]);
        f.render_widget(Paragraph::new(query), layout.query);
        let rule = format!(" {}", "─".repeat(width.saturating_sub(1)));
        f.render_widget(Paragraph::new(Line::styled(rule, dim())), layout.rule);

        let list = layout.list;
        let mut lines = Vec::new();
        if self.hits.is_empty() {
            lines.push(Line::styled(format!("  {}", self.empty_message()), dim()));
        }
        let shown = self
            .hits
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(layout.per_page());
        for (n, (i, hit)) in shown.enumerate() {
            if n > 0 {
                lines.push(Line::raw(""));
            }
            lines.extend(self.row(hit, i == self.selected, list.width as usize));
        }
        f.render_widget(Paragraph::new(lines), list);

        if let Some(divider) = layout.divider {
            f.render_widget(
                Paragraph::new(divider_lines(divider, layout.beside())),
                divider,
            );
        }
        if let Some(area) = layout.preview
            && let Some(s) = self.selected_session()
            && let Some((_, excerpts)) = &self.preview
        {
            let matches = &self.hits[self.selected].rows;
            let lines = self.preview_lines(
                s,
                excerpts,
                matches,
                area.width as usize,
                area.height as usize,
                layout.beside(),
            );
            f.render_widget(Paragraph::new(lines), area);
        }

        let x = (2 + self.text.width()).min(width.saturating_sub(1)) as u16;
        f.set_cursor_position(Position::new(layout.query.x + x, layout.query.y));
    }

    fn model(&self, s: &Session) -> Option<String> {
        (!s.model.is_empty()).then(|| text::cut_str(&s.agent.model_label(&s.model), 14))
    }

    fn row(&self, hit: &Hit, selected: bool, width: usize) -> [Line<'static>; 2] {
        let s = &self.sessions[hit.idx];
        let bar = || {
            if selected {
                Span::styled("▌ ", accent())
            } else {
                Span::raw("  ")
            }
        };

        let msgs = format!(
            "{} msg{}",
            s.messages,
            if s.messages == 1 { "" } else { "s" }
        );
        // The agent (the harness), then the model it ran.
        let agent = s.agent.name();
        let model = if width >= 60 { self.model(s) } else { None };
        let mut details = vec![String::new()];
        details.extend(model);
        details.extend([
            msgs,
            day(s.started, self.now, &self.tz),
            age(self.now - s.updated),
        ]);
        details.retain(|d| !d.is_empty());
        let details = format!(" · {}", details.join(" · "));
        let right = agent.width() + details.width();
        let title_width = width.saturating_sub(2 + 2 + right).max(8);
        let mut title_style = Style::new();
        if selected {
            title_style = title_style.add_modifier(Modifier::BOLD);
        }
        if s.archived {
            title_style = title_style.add_modifier(Modifier::DIM);
        }
        let title = text::cut(&text::marked(&s.title, &self.query.tokens), title_width);
        let pad = width.saturating_sub(2 + text::width(&title) + right);
        let mut first = vec![bar()];
        first.extend(text::spans(&title, title_style));
        first.push(Span::raw(" ".repeat(pad)));
        first.push(Span::styled(agent, agent_style(s.agent)));
        first.push(Span::styled(details, dim()));

        let gone = self.gone.get(&s.id).copied().unwrap_or(false);
        let folder = text::cut_str(&folder_name(&s.cwd, &self.home), (width / 4).max(12));
        let folder_style = if gone {
            Style::new().fg(Color::Red).add_modifier(Modifier::DIM)
        } else {
            dim()
        };
        let excerpt = hit
            .rows
            .first()
            .and_then(|row| self.snippets.get(row))
            .and_then(Option::as_ref)
            .map_or(s.last_prompt.as_str(), |e| e.text.as_str());
        let budget = width.saturating_sub(2 + folder.width() + 2);
        let mut second = vec![bar(), Span::styled(folder, folder_style), Span::raw("  ")];
        let excerpt = text::cut(&text::marked(excerpt, &self.query.tokens), budget);
        second.extend(text::spans(&excerpt, dim()));
        [Line::from(first), Line::from(second)]
    }

    fn status(&self) -> String {
        if self.error.is_some() {
            return "index error".into();
        }
        if self.indexing && self.started.elapsed() > SHOW_PROGRESS_AFTER {
            let done = self.progress.done.load(Ordering::Relaxed);
            let total = self.progress.total.load(Ordering::Relaxed);
            return (done * 100)
                .checked_div(total)
                .map_or("indexing".into(), |pct| format!("indexing {pct}%"));
        }
        let total = self.sessions.len();
        if self.text.trim().is_empty() {
            format!(
                "{} chat{}",
                thousands(total),
                if total == 1 { "" } else { "s" }
            )
        } else {
            format!("{} of {}", thousands(self.hits.len()), thousands(total))
        }
    }

    fn empty_message(&self) -> String {
        match (self.sessions.is_empty(), self.indexing) {
            (true, true) => "Looking for chats…".into(),
            (true, false) => {
                let names: Vec<&str> = AGENTS.iter().map(|a| a.name()).collect();
                format!("No chats found ({}).", names.join(", "))
            }
            (false, _) => "No matches.".into(),
        }
    }
}

fn divider_lines(area: Rect, beside: bool) -> Vec<Line<'static>> {
    if beside {
        vec![Line::styled("│", dim()); area.height as usize]
    } else {
        vec![Line::styled("─".repeat(area.width as usize), dim())]
    }
}

fn folder_name(cwd: &str, home: &str) -> String {
    match cwd.rsplit('/').find(|p| !p.is_empty()) {
        _ if cwd == home => "~".into(),
        Some(name) => name.to_owned(),
        None => cwd.to_owned(),
    }
}

fn accent() -> Style {
    Style::new().fg(Color::Indexed(173))
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

fn agent_style(agent: &dyn Agent) -> Style {
    Style::new().fg(Color::Indexed(agent.color()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names() {
        assert_eq!(folder_name("/home/a/code/shop", "/home/a"), "shop");
        assert_eq!(folder_name("/home/a/code/shop/", "/home/a"), "shop");
        assert_eq!(folder_name("/home/a", "/home/a"), "~");
        assert_eq!(folder_name("/", "/home/a"), "/");
    }
}
