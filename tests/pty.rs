mod common;

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use common::{Home, claude, codex, id, set_age};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

const ESC: &str = "\x1b";
const ENTER: &str = "\r";
const DOWN: &str = "\x1b[B";
const CTRL_C: &str = "\x03";
const CTRL_U: &str = "\x15";

const TIMEOUT: Duration = Duration::from_secs(20);
const ROWS: usize = 30;
const COLS: usize = 120;

struct Term {
    rows: usize,
    cols: usize,
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    output: Arc<Mutex<Vec<u8>>>,
}

impl Term {
    fn spawn(cmd: CommandBuilder) -> Term {
        Term::sized(cmd, ROWS, COLS)
    }

    fn sized(cmd: CommandBuilder, rows: usize, cols: usize) -> Term {
        let size = PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = native_pty_system().openpty(size).unwrap();
        let child = pair.slave.spawn_command(cmd).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let writer = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
        let output = Arc::new(Mutex::new(Vec::new()));
        let (out, answer) = (output.clone(), writer.clone());
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let mut scanned = 0;
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut out = out.lock().unwrap();
                out.extend_from_slice(&buf[..n]);
                while let Some(i) = out[scanned..].windows(4).position(|w| w == b"\x1b[6n") {
                    scanned += i + 4;
                    let _ = answer.lock().unwrap().write_all(b"\x1b[1;1R");
                }
            }
        });
        Term {
            rows,
            cols,
            _master: pair.master,
            child,
            writer,
            output,
        }
    }

    fn screen(&self) -> String {
        let mut screen = Screen::new(self.rows, self.cols);
        screen.feed(&String::from_utf8_lossy(&self.output.lock().unwrap()));
        screen.text()
    }

    fn expect(&self, needle: &str) {
        let start = Instant::now();
        while !self.screen().contains(needle) {
            assert!(
                start.elapsed() < TIMEOUT,
                "timed out waiting for {needle:?}. Screen:\n{}",
                self.screen()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn expect_query(&self, text: &str) {
        let start = Instant::now();
        let query = |screen: String| {
            let line = screen.lines().find(|l| !l.trim().is_empty())?;
            Some(line.trim_start().to_owned())
        };
        while query(self.screen()).is_none_or(|l| !l.starts_with(text)) {
            assert!(
                start.elapsed() < TIMEOUT,
                "timed out waiting for query {text:?}. Screen:\n{}",
                self.screen()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn send(&self, keys: &str) {
        let mut w = self.writer.lock().unwrap();
        w.write_all(keys.as_bytes()).unwrap();
        w.flush().unwrap();
    }

    fn paste(&self, text: &str) {
        let enabled = self
            .output
            .lock()
            .unwrap()
            .windows(8)
            .any(|w| w == b"\x1b[?2004h");
        let text = text.replace('\n', "\r");
        if enabled {
            self.send(&format!("\x1b[200~{text}\x1b[201~"));
        } else {
            self.send(&text);
        }
    }

    fn wait(&mut self) -> u32 {
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.exit_code();
            }
            if start.elapsed() > TIMEOUT {
                let _ = self.child.kill();
                panic!("process didn't exit. Screen:\n{}", self.screen());
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

struct Screen {
    cells: Vec<Vec<char>>,
    rows: usize,
    cols: usize,
    row: usize,
    col: usize,
}

impl Screen {
    fn new(rows: usize, cols: usize) -> Screen {
        Screen {
            cells: vec![vec![' '; cols]; rows],
            rows,
            cols,
            row: 0,
            col: 0,
        }
    }

    fn feed(&mut self, s: &str) {
        let mut chars = s.chars();
        while let Some(ch) = chars.next() {
            match ch {
                '\x1b' => match chars.next() {
                    Some('[') => {
                        let mut params = String::new();
                        let mut last = ' ';
                        for c in chars.by_ref() {
                            if ('\x40'..='\x7e').contains(&c) {
                                last = c;
                                break;
                            }
                            params.push(c);
                        }
                        self.csi(&params, last);
                    }
                    Some(']') => {
                        while let Some(c) = chars.next() {
                            if c == '\x07' || c == '\x1b' && chars.next().is_some() {
                                break;
                            }
                        }
                    }
                    _ => {}
                },
                '\r' => self.col = 0,
                '\n' => self.newline(),
                c if c.is_control() => {}
                c => {
                    if self.col >= self.cols {
                        self.col = 0;
                        self.newline();
                    }
                    self.cells[self.row][self.col] = c;
                    self.col += 1;
                }
            }
        }
    }

    fn csi(&mut self, params: &str, last: char) {
        let nums: Vec<usize> = params
            .trim_start_matches('?')
            .split(';')
            .map(|p| p.parse().unwrap_or(0))
            .collect();
        let n = |i: usize| nums.get(i).copied().filter(|&v| v > 0).unwrap_or(1);
        match last {
            'H' | 'f' => {
                self.row = (n(0) - 1).min(self.rows - 1);
                self.col = (n(1) - 1).min(self.cols - 1);
            }
            'A' => self.row = self.row.saturating_sub(n(0)),
            'B' => self.row = (self.row + n(0)).min(self.rows - 1),
            'C' => self.col = (self.col + n(0)).min(self.cols - 1),
            'D' => self.col = self.col.saturating_sub(n(0)),
            'G' => self.col = (n(0) - 1).min(self.cols - 1),
            'S' => (0..n(0)).for_each(|_| self.scroll()),
            'J' => {
                let (row, col) = (self.row, self.col);
                match nums.first().copied().unwrap_or(0) {
                    0 => {
                        self.cells[row][col..].fill(' ');
                        self.cells[row + 1..].iter_mut().for_each(|r| r.fill(' '));
                    }
                    _ => self.cells.iter_mut().for_each(|r| r.fill(' ')),
                }
            }
            'K' => {
                let (row, col) = (self.row, self.col);
                match nums.first().copied().unwrap_or(0) {
                    0 => self.cells[row][col..].fill(' '),
                    _ => self.cells[row].fill(' '),
                }
            }
            _ => {}
        }
    }

    fn newline(&mut self) {
        if self.row + 1 >= self.rows {
            self.scroll()
        } else {
            self.row += 1
        }
    }

    fn scroll(&mut self) {
        self.cells.remove(0);
        self.cells.push(vec![' '; self.cols]);
    }

    fn text(&self) -> String {
        let lines: Vec<String> = self
            .cells
            .iter()
            .map(|r| r.iter().collect::<String>().trim_end().to_owned())
            .collect();
        lines.join("\n").trim_end().to_owned()
    }
}

fn fake_agents(home: &Home) {
    let bin = home.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    for name in ["claude", "codex"] {
        let path = bin.join(name);
        let script = format!(
            "#!/bin/sh\nprintf '%s|%s|%s|%s\\n' {name} \"$PWD\" \"$(pwd -P)\" \"$*\" >> \"$AGENT_LOG\"\necho '{name} started'\n"
        );
        fs::write(&path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn agent_log(home: &Home) -> String {
    fs::read_to_string(home.path().join("agent.log")).unwrap_or_default()
}

fn command(home: &Home, program: &str, args: &[&str], shell: &str) -> CommandBuilder {
    let trove_dir = Path::new(env!("CARGO_BIN_EXE_trove"))
        .parent()
        .unwrap()
        .to_owned();
    let tmp = home.path().join("tmp");
    fs::create_dir_all(&tmp).unwrap();
    let mut cmd = CommandBuilder::new(program);
    cmd.args(args);
    cmd.env_clear();
    cmd.env("HOME", home.path());
    cmd.env("SHELL", shell);
    cmd.env("TERM", "xterm-256color");
    cmd.env("TZ", "UTC");
    cmd.env("TMPDIR", &tmp);
    cmd.env("AGENT_LOG", home.path().join("agent.log"));
    cmd.env(
        "PATH",
        format!(
            "{}:{}:/usr/bin:/bin",
            home.path().join("bin").display(),
            trove_dir.display()
        ),
    );
    cmd.cwd(home.path());
    cmd
}

fn trove(home: &Home, args: &[&str]) -> CommandBuilder {
    command(home, env!("CARGO_BIN_EXE_trove"), args, "/bin/sh")
}

fn home_with_chats() -> (Home, String, String) {
    home_with_chats_in("zoo")
}

fn home_with_chats_in(folder: &str) -> (Home, String, String) {
    let home = Home::new();
    fake_agents(&home);
    let zoo = home.project(folder);
    let other = home.project("other");
    home.claude(
        ".claude",
        &zoo,
        &id(1),
        &[
            claude::user("zebra stripes", &zoo),
            claude::assistant("They help with flies.", &zoo),
        ],
    );
    let codex_chat = home.codex(
        ".codex",
        &id(2),
        &codex::session(&id(2), &other, "otter habitats", "Rivers and coasts."),
    );
    set_age(&codex_chat, 3600);
    (home, zoo, other)
}

fn has(program: &str) -> bool {
    Path::new(program).exists()
}

#[test]
fn enter_resumes_the_chat_in_its_folder() {
    let (home, zoo, _) = home_with_chats();
    let mut term = Term::spawn(trove(&home, &["zebra"]));
    term.expect("zebra stripes");
    term.expect("zoo  zebra stripes");
    term.expect("1 of 2");
    term.send(ENTER);
    term.expect("claude started");
    assert_eq!(term.wait(), 0);
    assert_eq!(
        agent_log(&home),
        format!("claude|{zoo}|{zoo}|--resume {}\n", id(1))
    );
}

#[test]
fn typing_filters_and_arrows_move() {
    let (home, _, other) = home_with_chats();
    let mut term = Term::spawn(trove(&home, &[]));
    term.expect("2 chats");
    term.expect("otter habitats");
    term.send("otter");
    term.expect("1 of 2");
    term.send(CTRL_U);
    term.expect("2 chats");
    term.send(DOWN);
    term.send(ENTER);
    term.expect("codex started");
    assert_eq!(term.wait(), 0);
    assert_eq!(
        agent_log(&home),
        format!("codex|{other}|{other}|resume {}\n", id(2))
    );
}

#[test]
fn enter_right_after_typing_picks_what_was_typed() {
    let (home, _, other) = home_with_chats();
    let mut term = Term::spawn(trove(&home, &[]));
    term.expect("2 chats");
    term.send(&format!("otter{ENTER}"));
    term.expect("codex started");
    assert_eq!(term.wait(), 0);
    assert_eq!(
        agent_log(&home),
        format!("codex|{other}|{other}|resume {}\n", id(2))
    );
}

fn lines_of(screen: &str, needles: &[&str]) -> Vec<Option<usize>> {
    needles
        .iter()
        .map(|n| screen.lines().position(|l| l.contains(n)))
        .collect()
}

#[test]
fn moving_the_selection_moves_nothing_else() {
    let (home, _, _) = home_with_chats();
    let mut term = Term::spawn(trove(&home, &[]));
    term.expect("They help with flies.");
    let before = term.screen();
    term.send(DOWN);
    term.expect("Rivers and coasts.");
    let after = term.screen();
    let rows = [
        "zebra stripes",
        "otter habitats",
        "zoo  zebra stripes",
        "other  otter habitats",
    ];
    assert_eq!(
        lines_of(&before, &rows),
        lines_of(&after, &rows),
        "before:\n{before}\nafter:\n{after}"
    );
    assert!(lines_of(&after, &rows).iter().all(Option::is_some));
    term.send(ESC);
    term.wait();
}

#[test]
fn every_row_shows_its_folder_and_an_excerpt() {
    let (home, _, _) = home_with_chats();
    let mut term = Term::spawn(trove(&home, &[]));
    term.expect("2 chats");
    let screen = term.screen();
    let lines: Vec<&str> = screen.lines().collect();
    for (title, second) in [
        ("zebra stripes", "zoo  zebra stripes"),
        ("otter habitats", "other  otter habitats"),
    ] {
        let at = lines
            .iter()
            .position(|l| l.contains(title) && l.contains(" · "))
            .unwrap();
        assert!(lines[at + 1].contains(second), "{screen}");
    }
    term.send(ESC);
    term.wait();
}

#[test]
fn rows_show_agent_model_size_and_times() {
    let (home, _, _) = home_with_chats();
    let mut term = Term::spawn(trove(&home, &[]));
    term.expect("2 chats");
    let screen = term.screen();
    let row = |title: &str| {
        screen
            .lines()
            .find(|l| l.contains(title) && l.contains(" · "))
            .unwrap()
            .to_owned()
    };
    assert!(
        row("zebra stripes").contains("claude · opus 5.5 · 2 msgs · Sep 1 · "),
        "{screen}"
    );
    assert!(
        row("otter habitats").contains("codex · gpt-6-astra · 2 msgs · Sep 1 · "),
        "{screen}"
    );
    term.send(ESC);
    term.wait();
}

fn home_with_a_long_chat() -> Home {
    let home = Home::new();
    fake_agents(&home);
    let shop = home.project("shop");
    let texts = [
        "alpha opening question",
        "first reply",
        "bravo filler",
        "the needle is here",
        "charlie filler",
        "delta filler",
        "omega closing words",
    ];
    let lines: Vec<String> = texts
        .iter()
        .enumerate()
        .map(|(i, t)| {
            if i % 2 == 0 {
                claude::user(t, &shop)
            } else {
                claude::assistant(t, &shop)
            }
        })
        .collect();
    home.claude(".claude", &shop, &id(1), &lines);
    home
}

#[test]
fn the_preview_shows_how_a_chat_started_and_ended() {
    let home = home_with_a_long_chat();
    let mut term = Term::spawn(trove(&home, &[]));
    term.expect("omega closing words");
    let screen = term.screen();
    for text in [
        "alpha opening question",
        "first reply",
        "⋯",
        "delta filler",
        "omega closing words",
    ] {
        assert!(screen.contains(text), "{text:?} missing:\n{screen}");
    }
    assert!(!screen.contains("needle"), "{screen}");
    term.send(ESC);
    term.wait();
}

#[test]
fn while_searching_the_preview_centres_on_the_matches() {
    let home = home_with_a_long_chat();
    let mut term = Term::spawn(trove(&home, &["needle"]));
    term.expect("the needle is here");
    let screen = term.screen();
    let preview: Vec<&str> = screen
        .lines()
        .skip_while(|l| !l.starts_with("────"))
        .collect();
    let at = |text: &str| preview.iter().position(|l| l.contains(text));
    let (before, hit, after) = (
        at("bravo filler"),
        at("needle is here"),
        at("charlie filler"),
    );
    assert!(before.is_some() && before < hit && hit < after, "{screen}");
    term.send(ESC);
    term.wait();
}

#[test]
fn wide_terminals_put_the_preview_beside_the_list() {
    let (home, _, _) = home_with_chats();
    let mut term = Term::sized(trove(&home, &[]), 30, 150);
    term.expect("They help with flies.");
    let screen = term.screen();
    assert!(
        screen
            .lines()
            .any(|l| l.matches("zebra stripes").count() == 2 && l.contains('│')),
        "{screen}"
    );
    term.send(ESC);
    term.wait();

    let mut term = Term::sized(trove(&home, &[]), 30, 100);
    term.expect("They help with flies.");
    let screen = term.screen();
    assert!(screen.lines().any(|l| l.starts_with("────")), "{screen}");
    assert!(!screen.contains('│'), "{screen}");
    term.send(ESC);
    term.wait();
}

#[test]
fn the_input_has_a_placeholder_a_rule_and_a_bar_cursor() {
    let (home, _, _) = home_with_chats();
    let mut term = Term::spawn(trove(&home, &[]));
    term.expect("2 chats");
    let screen = term.screen();
    let mut lines = screen.lines();
    assert_eq!(lines.next().unwrap(), "", "{screen}");
    assert!(
        lines.next().unwrap().starts_with("  Search chats"),
        "{screen}"
    );
    assert!(lines.next().unwrap().starts_with(" ────"), "{screen}");
    term.send("zeb");
    term.expect_query("zeb");
    assert!(!term.screen().contains("Search chats"));
    term.send(ESC);
    term.wait();
    let raw = String::from_utf8_lossy(&term.output.lock().unwrap()).into_owned();
    let (bar, back) = (raw.find("\x1b[6 q"), raw.rfind("\x1b[0 q"));
    assert!(
        bar.is_some() && back > bar,
        "the cursor isn't a bar while picking, or isn't restored"
    );
}

#[test]
fn a_multi_line_paste_is_searched_not_run() {
    let (home, _, _) = home_with_chats();
    let mut term = Term::spawn(trove(&home, &[]));
    term.expect("2 chats");
    term.paste("otter\nhabitats");
    term.expect_query("otter habitats");
    term.expect("1 of 2");
    term.send(ESC);
    assert_eq!(term.wait(), 0);
    assert_eq!(agent_log(&home), "");
}

#[test]
fn json_is_printed_even_in_a_terminal() {
    let (home, _, _) = home_with_chats();
    let mut term = Term::spawn(trove(&home, &["--json", "zebra"]));
    assert_eq!(term.wait(), 0);
    let start = Instant::now();
    let printed = || String::from_utf8_lossy(&term.output.lock().unwrap()).into_owned();
    while !printed().contains(r#""title":"zebra stripes""#) {
        assert!(start.elapsed() < TIMEOUT, "no JSON:\n{}", printed());
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!printed().contains("\x1b[?2004h"), "the picker opened");
}

#[test]
fn an_idle_picker_draws_nothing() {
    let (home, _, _) = home_with_chats();
    let mut term = Term::spawn(trove(&home, &[]));
    term.expect("2 chats");
    thread::sleep(Duration::from_millis(500));
    let before = term.output.lock().unwrap().len();
    thread::sleep(Duration::from_millis(1500));
    let after = term.output.lock().unwrap().len();
    term.send(ESC);
    assert_eq!(term.wait(), 0);
    assert_eq!(after, before, "wrote {} bytes while idle", after - before);
}

#[test]
fn esc_leaves_the_screen_as_it_was() {
    let (home, _, _) = home_with_chats();
    let mut term = Term::spawn(trove(&home, &[]));
    term.expect("zebra stripes");
    term.send(ESC);
    assert_eq!(term.wait(), 0);
    assert!(
        !term.screen().contains("zebra"),
        "picker left behind:\n{}",
        term.screen()
    );
    assert_eq!(agent_log(&home), "");

    let mut term = Term::spawn(trove(&home, &[]));
    term.expect("zebra stripes");
    term.send(CTRL_C);
    assert_eq!(term.wait(), 130);
}

#[test]
fn a_chat_whose_folder_is_gone_prints_the_command() {
    let (home, zoo, _) = home_with_chats();
    fs::remove_dir_all(&zoo).unwrap();
    let mut term = Term::spawn(trove(&home, &["zebra"]));
    term.expect("zebra stripes");
    term.send(ENTER);
    term.expect("no longer exists");
    term.expect("Projects/zoo && claude --resume");
    assert_eq!(term.wait(), 1);
    assert_eq!(agent_log(&home), "");
}

fn first_resume(home: &Home, shell: &str, answer: &str) -> String {
    let mut term = Term::spawn(command(
        home,
        env!("CARGO_BIN_EXE_trove"),
        &["zebra"],
        shell,
    ));
    term.expect("zebra stripes");
    term.send(ENTER);
    term.expect("Stay in the chat's folder after it ends?");
    term.send(answer);
    term.expect("claude started");
    assert_eq!(term.wait(), 0);
    term.screen()
}

#[test]
fn first_resume_offers_the_shell_hook_once() {
    if !has("/bin/zsh") {
        return;
    }
    let (home, _, _) = home_with_chats();
    let screen = first_resume(&home, "/bin/zsh", "\r");
    assert!(
        screen.contains("This adds one line to ~/.zshrc. [Y/n]"),
        "{screen}"
    );
    assert!(screen.contains("Done."), "{screen}");
    let rc = fs::read_to_string(home.path().join(".zshrc")).unwrap();
    assert!(rc.contains(".config/trove/hook.zsh"), "{rc}");
    let hook = fs::read_to_string(home.path().join(".config/trove/hook.zsh")).unwrap();
    assert!(hook.contains("TROVE_CMD_FILE"));

    let mut term = Term::spawn(command(
        &home,
        env!("CARGO_BIN_EXE_trove"),
        &["zebra"],
        "/bin/zsh",
    ));
    term.expect("zebra stripes");
    term.send(ENTER);
    term.expect("claude started");
    assert_eq!(term.wait(), 0);
    assert!(!term.screen().contains("Stay in"));
}

#[test]
fn declining_the_hook_is_remembered() {
    if !has("/bin/zsh") {
        return;
    }
    let (home, _, _) = home_with_chats();
    first_resume(&home, "/bin/zsh", "n\r");
    assert!(!home.path().join(".zshrc").exists());
    assert!(home.path().join(".config/trove/declined").exists());

    let mut term = Term::spawn(command(
        &home,
        env!("CARGO_BIN_EXE_trove"),
        &["zebra"],
        "/bin/zsh",
    ));
    term.expect("zebra stripes");
    term.send(ENTER);
    term.expect("claude started");
    assert_eq!(term.wait(), 0);
    assert!(!term.screen().contains("Stay in"));
}

const NASTY: &str = r#"it's \' "q" $(touch pwned) `touch pwned`; touch pwned"#;

fn hook_leaves_the_shell_in_the_folder(shell: &str, rc: &str, folder: &str) {
    let (home, zoo, _) = home_with_chats_in(folder);
    first_resume(&home, shell, "y\r");
    fs::remove_file(home.path().join("agent.log")).unwrap();
    assert!(home.path().join(rc).exists(), "{rc} not written");

    let script =
        format!(r#"source ~/{rc}; trove zebra; printf '%s' "$PWD" > ~/shell-pwd; echo finished"#);
    let mut term = Term::spawn(command(&home, shell, &["-c", &script], shell));
    term.expect("zebra stripes");
    term.send(ENTER);
    term.expect("claude started");
    term.expect("finished");
    assert_eq!(term.wait(), 0);
    assert_eq!(
        fs::read_to_string(home.path().join("shell-pwd")).unwrap(),
        zoo
    );
    assert_eq!(
        agent_log(&home),
        format!("claude|{zoo}|{zoo}|--resume {}\n", id(1))
    );
    assert!(
        !home.path().join("pwned").exists(),
        "the folder name ran a command"
    );
}

fn bash_rc() -> &'static str {
    if cfg!(target_os = "macos") {
        ".bash_profile"
    } else {
        ".bashrc"
    }
}

#[test]
fn zsh_hook_leaves_the_shell_in_the_folder() {
    if has("/bin/zsh") {
        hook_leaves_the_shell_in_the_folder("/bin/zsh", ".zshrc", "zoo");
        hook_leaves_the_shell_in_the_folder("/bin/zsh", ".zshrc", NASTY);
    }
}

#[test]
fn bash_hook_leaves_the_shell_in_the_folder() {
    if has("/bin/bash") {
        hook_leaves_the_shell_in_the_folder("/bin/bash", bash_rc(), "zoo");
        hook_leaves_the_shell_in_the_folder("/bin/bash", bash_rc(), NASTY);
    }
}

#[test]
fn fish_hook_leaves_the_shell_in_the_folder() {
    let Some(fish) = [
        "/opt/homebrew/bin/fish",
        "/usr/local/bin/fish",
        "/usr/bin/fish",
    ]
    .into_iter()
    .find(|p| has(p)) else {
        return;
    };
    let (home, zoo, _) = home_with_chats();
    first_resume(&home, fish, "y\r");
    let function: PathBuf = home.path().join(".config/fish/functions/trove.fish");
    assert!(function.exists());
    fs::remove_file(home.path().join("agent.log")).unwrap();

    let script = r#"trove zebra; echo "after: $PWD""#;
    let mut term = Term::spawn(command(&home, fish, &["-c", script], fish));
    term.expect("zebra stripes");
    term.send(ENTER);
    term.expect("claude started");
    term.expect(&format!("after: {zoo}"));
    assert_eq!(term.wait(), 0);
}
