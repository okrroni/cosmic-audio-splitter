// SPDX-License-Identifier: MIT

use super::{AudioError, SplitSession};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const STATE_DIRECTORY: &str = "cosmic-audio-splitter";
const SESSION_FILE: &str = "session.json";

pub(super) struct JournalLoad {
    pub session: Option<SplitSession>,
    pub warning: Option<String>,
}

pub(super) trait SessionStore {
    fn load(&self) -> Result<JournalLoad, AudioError>;
    fn save(&self, session: &SplitSession) -> Result<(), AudioError>;
    fn clear(&self) -> Result<(), AudioError>;
}

pub(super) struct FileSessionStore {
    path: PathBuf,
}

impl FileSessionStore {
    pub fn system() -> Result<Self, AudioError> {
        let state_root = env::var_os("XDG_STATE_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("HOME")
                    .filter(|value| !value.is_empty())
                    .map(|home| PathBuf::from(home).join(".local/state"))
            })
            .ok_or_else(|| {
                AudioError::Journal("no user state directory is available".to_owned())
            })?;

        Ok(Self {
            path: state_root.join(STATE_DIRECTORY).join(SESSION_FILE),
        })
    }

    #[cfg(test)]
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    fn quarantine_invalid(&self) -> Result<PathBuf, AudioError> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let quarantine = self
            .path
            .with_file_name(format!("session.invalid-{timestamp}.json"));
        fs::rename(&self.path, &quarantine).map_err(|error| {
            AudioError::Journal(format!(
                "could not quarantine invalid session state: {error}"
            ))
        })?;
        Ok(quarantine)
    }

    fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), AudioError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                AudioError::Journal(format!("could not create state directory: {error}"))
            })?;
        }

        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, contents).map_err(|error| {
            AudioError::Journal(format!("could not write session state: {error}"))
        })?;
        fs::rename(&temporary, path).map_err(|error| {
            AudioError::Journal(format!("could not commit session state: {error}"))
        })
    }
}

impl SessionStore for FileSessionStore {
    fn load(&self) -> Result<JournalLoad, AudioError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(JournalLoad {
                    session: None,
                    warning: None,
                });
            }
            Err(error) => {
                return Err(AudioError::Journal(format!(
                    "could not read session state: {error}"
                )));
            }
        };

        match serde_json::from_slice(&bytes) {
            Ok(session) => Ok(JournalLoad {
                session: Some(session),
                warning: None,
            }),
            Err(error) => {
                let quarantine = self.quarantine_invalid()?;
                Ok(JournalLoad {
                    session: None,
                    warning: Some(format!(
                        "Invalid recovery state was moved to {}: {error}",
                        quarantine.display()
                    )),
                })
            }
        }
    }

    fn save(&self, session: &SplitSession) -> Result<(), AudioError> {
        let contents = serde_json::to_vec_pretty(session).map_err(|error| {
            AudioError::Journal(format!("could not serialize session state: {error}"))
        })?;
        Self::write_atomic(&self.path, &contents)
    }

    fn clear(&self) -> Result<(), AudioError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(AudioError::Journal(format!(
                "could not clear session state: {error}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{MovedInput, SessionPhase};

    fn temporary_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        env::temp_dir().join(format!(
            "cosmic-audio-splitter-{}-{unique}/{name}",
            std::process::id()
        ))
    }

    fn session() -> SplitSession {
        SplitSession {
            module_index: Some(42),
            previous_default: Some("old-output".to_owned()),
            outputs: vec!["one".to_owned(), "two".to_owned()],
            moved_inputs: vec![MovedInput {
                input_index: 7,
                original_sink: "old-output".to_owned(),
            }],
            phase: SessionPhase::Active,
        }
    }

    #[test]
    fn journal_round_trip_and_clear() {
        let path = temporary_path("session.json");
        let store = FileSessionStore::at(path.clone());
        let expected = session();

        store.save(&expected).expect("journal should save");
        let loaded = store.load().expect("journal should load");
        assert_eq!(loaded.session, Some(expected));
        assert_eq!(loaded.warning, None);

        store.clear().expect("journal should clear");
        assert!(!path.exists());
        if let Some(directory) = path.parent() {
            let _ = fs::remove_dir_all(directory);
        }
    }

    #[test]
    fn invalid_journal_is_quarantined() {
        let path = temporary_path("session.json");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("test directory should exist");
        }
        fs::write(&path, b"not json").expect("fixture should save");
        let store = FileSessionStore::at(path.clone());

        let loaded = store
            .load()
            .expect("invalid journal should not block startup");
        assert_eq!(loaded.session, None);
        assert!(loaded.warning.is_some());
        assert!(!path.exists());

        if let Some(directory) = path.parent() {
            let quarantined = fs::read_dir(directory)
                .expect("directory should be readable")
                .filter_map(Result::ok)
                .any(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("session.invalid-")
                });
            assert!(quarantined);
            let _ = fs::remove_dir_all(directory);
        }
    }
}
