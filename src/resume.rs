use std::path::Path;
use std::process::{Command, ExitCode};

use anyhow::Result;

use crate::display::{printable, tilde};
use crate::env::Env;
use crate::search::Session;
use crate::shell::{self, Quoting, quote};

pub fn command(s: &Session, q: Quoting) -> String {
    let words: Vec<String> = s
        .agent
        .resume(&s.sid, &s.file)
        .iter()
        .map(|w| quote(w, q))
        .collect();
    format!("cd {} && {}", quote(&s.cwd, q), words.join(" "))
}

pub fn resume(s: &Session, env: &Env) -> Result<ExitCode> {
    let cmd = command(s, Quoting::of(env));
    let cwd = Path::new(&s.cwd);
    if !cwd.is_absolute() || !cwd.is_dir() {
        eprintln!(
            "trove: this chat's folder no longer exists. To resume it anyway:\n  {}",
            printable(&cmd)
        );
        return Ok(ExitCode::FAILURE);
    }
    shell::sync(env);
    // Under the shell hook, the shell runs the command itself, so it stays in the chat's folder.
    if let Some(file) = env.var("TROVE_CMD_FILE") {
        std::fs::write(file, &cmd)?;
        return Ok(ExitCode::SUCCESS);
    }
    shell::offer(env);

    let words = s.agent.resume(&s.sid, &s.file);
    let (bin, args) = words.split_first().expect("a resume command has a program");
    eprintln!(
        "\x1b[2mcd {} && {}\x1b[0m",
        printable(&tilde(&s.cwd, env.home_str())),
        printable(&words.join(" "))
    );
    let mut child = Command::new(bin);
    // Shells and wrapper scripts trust $PWD over the real working directory.
    child.args(args).current_dir(&s.cwd).env("PWD", &s.cwd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = child.exec();
        anyhow::bail!("couldn't start {bin}: {err}");
    }
    #[cfg(not(unix))]
    {
        let status = child
            .status()
            .map_err(|e| anyhow::anyhow!("couldn't start {bin}: {e}"))?;
        Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::agents::Agent;
    use crate::agents::claude::Claude;
    use crate::agents::codex::Codex;

    fn session(agent: &'static dyn Agent, cwd: &str) -> Session {
        Session {
            id: 1,
            agent,
            sid: "7c6b5a49-3827-4165-8e0d-9f8a7b6c5d4e".into(),
            cwd: cwd.into(),
            file: PathBuf::new(),
            title: String::new(),
            last_prompt: String::new(),
            model: String::new(),
            messages: 1,
            started: 0,
            updated: 0,
            archived: false,
        }
    }

    #[test]
    fn commands() {
        assert_eq!(
            command(&session(&Claude, "/Users/a/shop"), Quoting::Posix),
            "cd /Users/a/shop && claude --resume 7c6b5a49-3827-4165-8e0d-9f8a7b6c5d4e"
        );
        assert_eq!(
            command(&session(&Codex, "/Users/a/my shop"), Quoting::Posix),
            "cd '/Users/a/my shop' && codex resume 7c6b5a49-3827-4165-8e0d-9f8a7b6c5d4e"
        );
    }
}
