use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The home folder and environment variables trove goes by, which tests make up.
#[derive(Clone, Debug)]
pub struct Env {
    pub home: PathBuf,
    vars: HashMap<String, PathBuf>,
}

impl Env {
    pub fn new(home: PathBuf) -> Env {
        Env {
            home,
            vars: HashMap::new(),
        }
    }

    pub fn from_system() -> Option<Env> {
        let vars = std::env::vars_os()
            .filter(|(_, v)| !v.is_empty())
            .filter_map(|(k, v)| Some((k.into_string().ok()?, PathBuf::from(v))))
            .collect();
        Some(Env {
            home: dirs::home_dir()?,
            vars,
        })
    }

    pub fn with(mut self, key: &str, value: impl Into<PathBuf>) -> Env {
        self.vars.insert(key.to_owned(), value.into());
        self
    }

    pub fn var(&self, key: &str) -> Option<&Path> {
        self.vars.get(key).map(PathBuf::as_path)
    }

    pub fn config_dir(&self) -> PathBuf {
        self.var("XDG_CONFIG_HOME")
            .map_or_else(|| self.home.join(".config"), Path::to_owned)
    }

    pub fn home_str(&self) -> &str {
        self.home.to_str().unwrap_or("")
    }
}
