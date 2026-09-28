use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use crate::display::tilde;
use crate::env::Env;

const ZSH: &str = r#"# Installed by trove. Lets trove leave your shell in the chat's folder.
trove() {
  local f cmd code
  f=$(mktemp "${TMPDIR:-/tmp}/trove.XXXXXX") || return
  TROVE_CMD_FILE=$f command trove "$@"
  code=$?
  cmd=$(<"$f")
  command rm -f -- "$f"
  [[ -n $cmd ]] || return $code
  print -rs -- "$cmd"
  eval "$cmd"
}
"#;

const BASH: &str = r#"# Installed by trove. Lets trove leave your shell in the chat's folder.
trove() {
  local f cmd code
  f=$(mktemp "${TMPDIR:-/tmp}/trove.XXXXXX") || return
  TROVE_CMD_FILE=$f command trove "$@"
  code=$?
  cmd=$(<"$f")
  command rm -f -- "$f"
  [ -n "$cmd" ] || return $code
  history -s "$cmd"
  eval "$cmd"
}
"#;

const FISH: &str = r#"# Installed by trove. Lets trove leave your shell in the chat's folder.
function trove --description 'Find and resume coding agent chats'
    set -l f (mktemp -t trove.XXXXXX); or return
    TROVE_CMD_FILE=$f TROVE_SHELL=fish command trove $argv
    set -l code $status
    set -l cmd (cat $f)
    command rm -f -- $f
    test -n "$cmd"; or return $code
    builtin history append -- "$cmd" 2>/dev/null
    eval $cmd
end
"#;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Quoting {
    Posix,
    Fish,
}

impl Quoting {
    /// The fish hook says it's fish; other shells quote the POSIX way.
    pub fn of(env: &Env) -> Quoting {
        if env.var("TROVE_SHELL") == Some(Path::new("fish")) {
            Quoting::Fish
        } else {
            Quoting::Posix
        }
    }
}

pub fn quote(s: &str, q: Quoting) -> String {
    // `=` is only special at the start of a word (zsh's `=cmd`).
    let safe = |(i, b): (usize, u8)| {
        b.is_ascii_alphanumeric() || b"-_./@%+:,".contains(&b) || b == b'=' && i > 0
    };
    if !s.is_empty() && s.bytes().enumerate().all(safe) {
        return s.to_owned();
    }
    match q {
        Quoting::Posix => format!("'{}'", s.replace('\'', r"'\''")),
        // In fish's single quotes, `\` and `'` are still escapes.
        Quoting::Fish => format!("'{}'", s.replace('\\', r"\\").replace('\'', r"\'")),
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Shell {
    Zsh,
    Bash,
    Fish,
}

struct Setup {
    shell: Shell,
    hook: PathBuf,
    rc: Option<PathBuf>,
    declined: PathBuf,
}

impl Setup {
    fn detect(env: &Env) -> Option<Setup> {
        let shell = match env.var("SHELL")?.file_name()?.to_str()? {
            "zsh" => Shell::Zsh,
            "bash" => Shell::Bash,
            "fish" => Shell::Fish,
            _ => return None,
        };
        let config = env.config_dir();
        let dir = config.join("trove");
        let home = &env.home;
        let (hook, rc) = match shell {
            Shell::Zsh => (
                dir.join("hook.zsh"),
                Some(env.var("ZDOTDIR").unwrap_or(home).join(".zshrc")),
            ),
            Shell::Bash if cfg!(target_os = "macos") => {
                (dir.join("hook.bash"), Some(home.join(".bash_profile")))
            }
            Shell::Bash => (dir.join("hook.bash"), Some(home.join(".bashrc"))),
            Shell::Fish => (config.join("fish/functions/trove.fish"), None),
        };
        Some(Setup {
            shell,
            hook,
            rc,
            declined: dir.join("declined"),
        })
    }

    fn script(&self) -> &'static str {
        match self.shell {
            Shell::Zsh => ZSH,
            Shell::Bash => BASH,
            Shell::Fish => FISH,
        }
    }

    fn rc_line(&self) -> String {
        let hook = quote(&self.hook.to_string_lossy(), Quoting::Posix);
        format!("[ -f {hook} ] && . {hook}  # trove")
    }

    fn installed(&self) -> bool {
        match &self.rc {
            Some(rc) => {
                fs::read_to_string(rc).is_ok_and(|s| s.contains(&*self.hook.to_string_lossy()))
            }
            None => self.hook.exists(),
        }
    }

    fn install(&self) -> io::Result<()> {
        fs::create_dir_all(self.hook.parent().expect("hook path has a parent"))?;
        fs::write(&self.hook, self.script())?;
        if let Some(rc) = &self.rc {
            let mut f = fs::OpenOptions::new().create(true).append(true).open(rc)?;
            writeln!(
                f,
                "\n# trove: stay in the chat's folder after resuming it\n{}",
                self.rc_line()
            )?;
        }
        Ok(())
    }
}

pub fn sync(env: &Env) {
    if let Some(setup) = Setup::detect(env)
        && fs::read_to_string(&setup.hook).is_ok_and(|s| s != setup.script())
    {
        let _ = fs::write(&setup.hook, setup.script());
    }
}

pub fn offer(env: &Env) {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return;
    }
    let Some(setup) = Setup::detect(env) else {
        return;
    };
    if setup.declined.exists() || setup.installed() {
        return;
    }
    let target = setup.rc.as_ref().unwrap_or(&setup.hook);
    eprint!(
        "Stay in the chat's folder after it ends? This adds one line to {}. [Y/n] ",
        tilde(&target.to_string_lossy(), env.home_str())
    );
    let mut answer = String::new();
    if !matches!(io::stdin().lock().read_line(&mut answer), Ok(n) if n > 0) {
        eprintln!();
        return;
    }
    if matches!(answer.trim().to_lowercase().as_str(), "n" | "no") {
        if let Some(dir) = setup.declined.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(&setup.declined, "");
        return;
    }
    match setup.install() {
        Ok(()) if setup.rc.is_some() => {
            eprintln!("Done. New terminals will stay in the chat's folder.")
        }
        Ok(()) => eprintln!("Done. Takes effect the next time you run trove."),
        Err(e) => eprintln!(
            "Couldn't set it up ({e}). Add this line to your shell config yourself:\n  {}",
            setup.rc_line()
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    #[test]
    fn quoting() {
        use Quoting::*;
        assert_eq!(quote("/a/b-c_d.e", Posix), "/a/b-c_d.e");
        assert_eq!(quote("/a/it's", Posix), r"'/a/it'\''s'");
        assert_eq!(quote("/a/$HOME", Posix), "'/a/$HOME'");
        assert_eq!(quote("/a/~b", Posix), "'/a/~b'");
        assert_eq!(quote("", Posix), "''");
        assert_eq!(quote("--resume=3f2a", Posix), "--resume=3f2a");
        assert_eq!(quote("=ls", Posix), "'=ls'");
        assert_eq!(quote("/a/it's", Fish), r"'/a/it\'s'");
        assert_eq!(quote(r"/a/b\c", Fish), r"'/a/b\\c'");
        assert_eq!(quote(r"/a\'; touch x; '", Fish), r"'/a\\\'; touch x; \''");
    }

    const NASTY: [&str; 6] = [
        "/tmp/it's",
        r"/tmp/a\'; touch pwned; '",
        r"/tmp/trailing\",
        "/tmp/$(touch pwned) `touch pwned` $HOME",
        "/tmp/new\nline",
        "/tmp/\"double\" ; & | > <",
    ];

    fn round_trip(shell: &str, q: Quoting) {
        if !std::path::Path::new(shell).exists() {
            return;
        }
        for s in NASTY {
            let out = Command::new(shell)
                .args(["-c", &format!("printf '%s' {}", quote(s, q))])
                .current_dir(std::env::temp_dir())
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                s,
                "{shell} misread {s:?}"
            );
        }
    }

    #[test]
    fn posix_shells_read_quotes_back_exactly() {
        round_trip("/bin/sh", Quoting::Posix);
        round_trip("/bin/bash", Quoting::Posix);
        round_trip("/bin/zsh", Quoting::Posix);
    }

    #[test]
    fn fish_reads_its_quotes_back_exactly() {
        for fish in [
            "/opt/homebrew/bin/fish",
            "/usr/local/bin/fish",
            "/usr/bin/fish",
        ] {
            round_trip(fish, Quoting::Fish);
        }
    }
}
