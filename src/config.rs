// SPDX-License-Identifier: MIT

use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const CONFIG_DIRECTORY: &str = "cosmic-audio-splitter";
const CONFIG_FILE: &str = "config.json";
const MAX_PRESET_NAME_CHARS: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PresetOutput {
    pub name: String,
    pub volume_percent: u32,
    pub muted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AudioPreset {
    pub name: String,
    pub outputs: Vec<PresetOutput>,
    pub latency_compensation: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub selected_outputs: Vec<String>,
    pub latency_compensation: bool,
    pub selection_initialized: bool,
    pub presets: Vec<AudioPreset>,
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

    pub fn upsert_preset(&mut self, mut preset: AudioPreset) -> Result<(), String> {
        preset.name = preset.name.trim().to_owned();
        if preset.name.is_empty() {
            return Err("Enter a name for the preset.".to_owned());
        }
        if preset.name.chars().count() > MAX_PRESET_NAME_CHARS {
            return Err(format!(
                "Preset names can contain at most {MAX_PRESET_NAME_CHARS} characters."
            ));
        }

        let mut seen = std::collections::HashSet::new();
        preset
            .outputs
            .retain(|output| seen.insert(output.name.clone()));
        if preset.outputs.len() < 2 {
            return Err("Select at least two available outputs before saving a preset.".to_owned());
        }
        for output in &mut preset.outputs {
            output.volume_percent = output.volume_percent.min(100);
        }

        if let Some(existing) = self
            .presets
            .iter_mut()
            .find(|existing| existing.name.eq_ignore_ascii_case(&preset.name))
        {
            *existing = preset;
        } else {
            self.presets.push(preset);
        }
        Ok(())
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
            presets: vec![AudioPreset {
                name: "Desk".to_owned(),
                outputs: vec![
                    PresetOutput {
                        name: "first".to_owned(),
                        volume_percent: 80,
                        muted: false,
                    },
                    PresetOutput {
                        name: "second".to_owned(),
                        volume_percent: 50,
                        muted: true,
                    },
                ],
                latency_compensation: true,
            }],
        };

        expected.save_to(&path).expect("save should succeed");
        let actual = AppConfig::load_from(&path).expect("load should succeed");

        assert_eq!(actual, expected);
        if let Some(directory) = path.parent() {
            let _ = fs::remove_dir_all(directory);
        }
    }

    #[test]
    fn older_config_without_presets_remains_compatible() {
        let config: AppConfig = serde_json::from_str(
            r#"{"selected_outputs":["one","two"],"selection_initialized":true}"#,
        )
        .expect("old config should deserialize");

        assert!(config.presets.is_empty());
        assert!(!config.latency_compensation);
    }

    #[test]
    fn presets_are_normalized_and_replaced_case_insensitively() {
        let mut config = AppConfig::default();
        config
            .upsert_preset(AudioPreset {
                name: "  Desk  ".to_owned(),
                outputs: vec![
                    PresetOutput {
                        name: "first".to_owned(),
                        volume_percent: 120,
                        muted: false,
                    },
                    PresetOutput {
                        name: "second".to_owned(),
                        volume_percent: 40,
                        muted: false,
                    },
                    PresetOutput {
                        name: "second".to_owned(),
                        volume_percent: 20,
                        muted: true,
                    },
                ],
                latency_compensation: false,
            })
            .expect("valid preset");
        config
            .upsert_preset(AudioPreset {
                name: "desk".to_owned(),
                outputs: vec![
                    PresetOutput {
                        name: "third".to_owned(),
                        volume_percent: 70,
                        muted: false,
                    },
                    PresetOutput {
                        name: "fourth".to_owned(),
                        volume_percent: 60,
                        muted: false,
                    },
                ],
                latency_compensation: true,
            })
            .expect("replacement preset");

        assert_eq!(config.presets.len(), 1);
        assert_eq!(config.presets[0].name, "desk");
        assert_eq!(config.presets[0].outputs[0].volume_percent, 70);
        assert!(config.presets[0].latency_compensation);
    }
}
