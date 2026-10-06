// SPDX-License-Identifier: MIT

use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const CONFIG_DIRECTORY: &str = "cosmic-audio-splitter";
const CONFIG_FILE: &str = "config.json";

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub selected_outputs: Vec<String>,
    pub latency_compensation: bool,
    pub selection_initialized: bool,
}

impl AppConfig {
    pub fn load() -> Self {
        config_path()
            .and_then(|path| Self::load_from(&path).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> io::Result<()> {
        let path = config_path().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no user configuration directory")
        })?;
        self.save_to(&path)
    }

    fn load_from(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }

    fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let temporary = path.with_extension("json.tmp");
        let contents = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        fs::write(&temporary, contents)?;
        fs::rename(temporary, path)
    }
}

fn config_path() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })
        .map(|directory| directory.join(CONFIG_DIRECTORY).join(CONFIG_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn config_round_trip() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "cosmic-audio-splitter-{}-{unique}/config.json",
            std::process::id()
        ));
        let expected = AppConfig {
            selected_outputs: vec!["first".to_owned(), "second".to_owned()],
            latency_compensation: true,
            selection_initialized: true,
        };

        expected.save_to(&path).expect("save should succeed");
        let actual = AppConfig::load_from(&path).expect("load should succeed");

        assert_eq!(actual, expected);
        if let Some(directory) = path.parent() {
            let _ = fs::remove_dir_all(directory);
        }
    }
}
