use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::agents::{AGENTS, Agent};
use crate::env::Env;

#[derive(Clone, Debug)]
pub struct Found {
    pub path: PathBuf,
    pub agent: &'static dyn Agent,
    pub archived: bool,
    pub size: u64,
    pub mtime_ns: i64,
    pub ino: u64,
}

#[derive(Default, Debug)]
pub struct Discovery {
    pub files: Vec<Found>,
    pub title_files: Vec<(&'static dyn Agent, PathBuf)>,
}

pub fn discover(env: &Env) -> Discovery {
    let mut roots: Vec<PathBuf> = AGENTS.iter().flat_map(|a| a.roots(env)).collect();
    if let Ok(entries) = fs::read_dir(&env.home) {
        let mut dots: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| e.file_name().as_encoded_bytes().starts_with(b"."))
            .map(|e| e.path())
            .collect();
        dots.sort();
        roots.extend(dots);
    }
    roots.retain(|root| root.is_absolute());

    let mut seen = HashSet::new();
    let mut out = Discovery::default();
    for root in &roots {
        for &agent in AGENTS {
            for (store, archived) in agent.stores(root) {
                if first_visit(&mut seen, agent, &store) {
                    agent.chats(&store, &mut |path| {
                        out.files.extend(found(path, agent, archived))
                    });
                }
            }
            for file in agent.title_files(root) {
                if first_visit(&mut seen, agent, &file) {
                    out.title_files.push((agent, file));
                }
            }
        }
    }
    out
}

/// Whether `path` exists and `agent` hasn't seen it yet under another name: stores are often
/// shared between roots through symlinks.
fn first_visit(seen: &mut HashSet<(&str, PathBuf)>, agent: &dyn Agent, path: &Path) -> bool {
    match fs::canonicalize(path) {
        Ok(real) => seen.insert((agent.name(), real)),
        Err(_) => false,
    }
}

fn found(path: PathBuf, agent: &'static dyn Agent, archived: bool) -> Option<Found> {
    path.to_str()?;
    let meta = fs::metadata(&path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mtime_ns = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos() as i64;
    #[cfg(unix)]
    let ino = std::os::unix::fs::MetadataExt::ino(&meta);
    #[cfg(not(unix))]
    let ino = 0;
    Some(Found {
        path,
        agent,
        archived,
        size: meta.len(),
        mtime_ns,
        ino,
    })
}
