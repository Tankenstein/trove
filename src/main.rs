use std::io::{self, IsTerminal};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use trove::env::Env;
use trove::ui::{self, Outcome};
use trove::{index, output, resume};

const HELP: &str = "\
trove: find and resume coding agent chats, from Claude Code, Codex, OpenCode,
Copilot CLI, Pi and Gemini CLI.

Usage: trove [words...]
       trove --json [words...]
       trove --pick [--json] [words...]
       trove --show [--json] ID...

Type to search chat titles, folders and messages. Enter resumes the chat in
its folder, Esc quits.

When output is piped, matches are printed instead, best first, one per line,
with tabs between age, agent, folder, title and resume command.

      --json      Print matches as JSON, one chat per line, even in a terminal:
                  id, agent, model, folder, title, started, updated,
                  messages (how many), archived, file, resume, and matches
                  (the messages that match).
      --pick      Ask which chat, then print it instead of resuming it: its
                  resume command, or with --json the chat as one line of
                  JSON, without matches. Works inside $(...).
      --show      Print the chats with these ids, with every prompt and reply.
                  With --json, one chat per line, its messages in transcript.
  -h, --help      Print this help.
  -V, --version   Print the version.

Exits with 1 when nothing matches, an id isn't found, or you quit --pick with
Esc, and with 130 on Ctrl-C.
";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("trove: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    let mut words = Vec::new();
    let (mut literal, mut json, mut show, mut pick) = (false, false, false, false);
    for arg in std::env::args().skip(1) {
        if literal || !arg.starts_with('-') || arg == "-" {
            words.push(arg);
            continue;
        }
        match arg.as_str() {
            "--" => literal = true,
            "--json" => json = true,
            "--show" => show = true,
            "--pick" => pick = true,
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(ExitCode::SUCCESS);
            }
            "-V" | "--version" => {
                println!("trove {}", env!("CARGO_PKG_VERSION"));
                return Ok(ExitCode::SUCCESS);
            }
            _ => bail!("unknown option {arg} (see trove --help)"),
        }
    }
    let env = Env::from_system().context("can't find your home directory")?;
    let db = index::default_path()?;

    if show {
        if pick {
            bail!("--pick and --show don't go together (see trove --help)");
        }
        if words.is_empty() {
            bail!("--show needs the ids of the chats to show (see trove --help)");
        }
        return output::show(&env, &db, &words, json);
    }
    let query = words.join(" ");
    if pick {
        if !io::stdin().is_terminal() {
            bail!("--pick asks which chat, so it needs a terminal");
        }
    } else if json || !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        return output::matches(&env, &db, &query, json);
    }
    match ui::pick(&env, &db, &query)? {
        Outcome::Picked(session) if pick => output::picked(&env, &session, json),
        Outcome::Picked(session) => resume::resume(&session, &env),
        Outcome::Quit if pick => Ok(ExitCode::FAILURE),
        Outcome::Quit => Ok(ExitCode::SUCCESS),
        Outcome::Interrupted => Ok(ExitCode::from(130)),
    }
}
