// SPDX-License-Identifier: MIT

//! PipeWire routing through its PulseAudio-compatible control protocol.
//!
//! PipeWire performs the actual fan-out, resampling, and clock synchronization.
//! This module only manages the graph through `pactl`, which keeps audio samples
//! out of the application process and out of the UI event loop.

use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::thread;
use std::time::Duration;
use thiserror::Error;

pub const VIRTUAL_SINK_NAME: &str = "cosmic_audio_splitter";
const VIRTUAL_SINK_DESCRIPTION: &str = "Audio Splitter";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioDevice {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub detail: String,
    pub icon_name: String,
    pub volume_percent: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioSnapshot {
    pub devices: Vec<AudioDevice>,
    pub default_sink: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MovedInput {
    input_index: u32,
    original_sink: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SplitSession {
    pub module_index: u32,
    pub previous_default: Option<String>,
    pub outputs: Vec<String>,
    moved_inputs: Vec<MovedInput>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartOutcome {
    pub session: SplitSession,
    pub warning: Option<String>,
}

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("PipeWire audio controls are unavailable: {0}")]
    Unavailable(String),
    #[error("audio command failed: {0}")]
    Command(String),
    #[error("PipeWire returned data that could not be read: {0}")]
    InvalidData(String),
    #[error("select at least two available audio outputs")]
    NotEnoughOutputs,
    #[error("an audio output has an unsupported internal name: {0}")]
    UnsafeName(String),
    #[error("these selected outputs are no longer available: {0}")]
    MissingOutputs(String),
    #[error("audio splitting stopped with errors: {0}")]
    StopFailed(String),
}

#[derive(Debug, Deserialize)]
struct PactlSink {
    index: u32,
    name: String,
    description: Option<String>,
    state: Option<String>,
    sample_specification: Option<String>,
    #[serde(default)]
    volume: HashMap<String, PactlChannelVolume>,
    #[serde(default)]
    properties: HashMap<String, Value>,
}

#[derive(Debug, Deserialize)]
struct PactlChannelVolume {
    value_percent: String,
}

#[derive(Debug, Deserialize)]
struct PactlModule {
    /// PipeWire's built-in modules are visible through pactl but do not have a
    /// PulseAudio module index. Only dynamically loaded modules can be unloaded.
    index: Option<u32>,
    name: String,
    argument: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PactlSinkInput {
    index: u32,
    sink: u32,
}

pub struct AudioController;

impl AudioController {
    /// Remove a combine sink left behind by a previous crash, then enumerate outputs.
    pub fn prepare() -> Result<AudioSnapshot, AudioError> {
        Self::cleanup_owned_modules(None)?;
        Self::discover()
    }

    pub fn discover() -> Result<AudioSnapshot, AudioError> {
        let sinks = list_sinks()?;
        let mut devices = sinks
            .into_iter()
            .filter(|sink| sink.name != VIRTUAL_SINK_NAME)
            .map(to_audio_device)
            .collect::<Vec<_>>();

        devices.sort_by(|left, right| {
            left.description
                .to_lowercase()
                .cmp(&right.description.to_lowercase())
                .then_with(|| left.name.cmp(&right.name))
        });

        Ok(AudioSnapshot {
            devices,
            default_sink: get_default_sink().ok(),
        })
    }

    pub fn start(
        outputs: Vec<String>,
        latency_compensation: bool,
    ) -> Result<StartOutcome, AudioError> {
        let mut seen = HashSet::new();
        let outputs = outputs
            .into_iter()
            .filter(|output| seen.insert(output.clone()))
            .collect::<Vec<_>>();

        if outputs.len() < 2 {
            return Err(AudioError::NotEnoughOutputs);
        }

        for output in &outputs {
            if !is_safe_pulse_name(output) {
                return Err(AudioError::UnsafeName(output.clone()));
            }
        }

        let available = list_sinks()?;
        let available_names = available
            .iter()
            .map(|sink| sink.name.as_str())
            .collect::<HashSet<_>>();
        let missing = outputs
            .iter()
            .filter(|output| !available_names.contains(output.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(AudioError::MissingOutputs(missing.join(", ")));
        }

        Self::cleanup_owned_modules(outputs.first().map(String::as_str))?;

        let previous_default = get_default_sink().ok();
        let sink_names_by_index = available
            .iter()
            .map(|sink| (sink.index, sink.name.clone()))
            .collect::<HashMap<_, _>>();
        let moved_inputs = list_sink_inputs()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|input| {
                sink_names_by_index
                    .get(&input.sink)
                    .cloned()
                    .map(|original_sink| MovedInput {
                        input_index: input.index,
                        original_sink,
                    })
            })
            .collect::<Vec<_>>();

        let module_args = module_arguments(&outputs, latency_compensation);
        let module_index = run_pactl(&module_args)?
            .trim()
            .parse::<u32>()
            .map_err(|error| AudioError::InvalidData(format!("invalid module ID: {error}")))?;

        if let Err(error) = wait_for_sink(VIRTUAL_SINK_NAME) {
            let _ = unload_module(module_index);
            return Err(error);
        }

        if let Err(error) = set_default_sink(VIRTUAL_SINK_NAME) {
            let _ = unload_module(module_index);
            return Err(error);
        }

        let mut move_failures = 0usize;
        for input in &moved_inputs {
            if move_sink_input(input.input_index, VIRTUAL_SINK_NAME).is_err() {
                // A stream may disappear naturally while Start is being processed.
                move_failures += 1;
            }
        }

        let warning = (move_failures > 0).then(|| {
            format!(
                "{move_failures} audio stream(s) ended before they could be moved; new audio will still use the split output"
            )
        });

        Ok(StartOutcome {
            session: SplitSession {
                module_index,
                previous_default,
                outputs,
                moved_inputs,
            },
            warning,
        })
    }

    pub fn set_volume(name: &str, volume_percent: u32) -> Result<(), AudioError> {
        if !is_safe_pulse_name(name) {
            return Err(AudioError::UnsafeName(name.to_owned()));
        }

        run_pactl(&[
            "set-sink-volume".to_owned(),
            name.to_owned(),
            format!("{}%", volume_percent.min(100)),
        ])
        .map(|_| ())
    }

    pub fn stop(session: &SplitSession) -> Result<(), AudioError> {
        let mut errors = Vec::new();
        let sinks = list_sinks().unwrap_or_default();
        let sink_names_by_index = sinks
            .iter()
            .map(|sink| (sink.index, sink.name.as_str()))
            .collect::<HashMap<_, _>>();
        let virtual_index = sinks
            .iter()
            .find(|sink| sink.name == VIRTUAL_SINK_NAME)
            .map(|sink| sink.index);

        if get_default_sink().ok().as_deref() == Some(VIRTUAL_SINK_NAME)
            && let Some(previous) = session
                .previous_default
                .as_deref()
                .filter(|name| sinks.iter().any(|sink| sink.name == *name))
            && let Err(error) = set_default_sink(previous)
        {
            errors.push(error.to_string());
        }

        if let Some(virtual_index) = virtual_index {
            let current_inputs = list_sink_inputs()
                .unwrap_or_default()
                .into_iter()
                .map(|input| (input.index, input.sink))
                .collect::<HashMap<_, _>>();

            for moved in &session.moved_inputs {
                let still_on_splitter =
                    current_inputs.get(&moved.input_index) == Some(&virtual_index);
                let original_exists = sink_names_by_index
                    .values()
                    .any(|name| *name == moved.original_sink);
                if still_on_splitter
                    && original_exists
                    && let Err(error) = move_sink_input(moved.input_index, &moved.original_sink)
                {
                    errors.push(error.to_string());
                }
            }
        }

        if let Err(error) = unload_module(session.module_index) {
            errors.push(error.to_string());
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(AudioError::StopFailed(errors.join("; ")))
        }
    }

    fn cleanup_owned_modules(fallback: Option<&str>) -> Result<(), AudioError> {
        let modules = list_modules()?;
        let stale = modules
            .into_iter()
            .filter(|module| {
                module.name == "module-combine-sink"
                    && module.argument.as_deref().is_some_and(|arguments| {
                        arguments
                            .split_whitespace()
                            .any(|arg| arg.strip_prefix("sink_name=") == Some(VIRTUAL_SINK_NAME))
                    })
            })
            .filter_map(|module| module.index)
            .collect::<Vec<_>>();

        if stale.is_empty() {
            return Ok(());
        }

        if get_default_sink().ok().as_deref() == Some(VIRTUAL_SINK_NAME) {
            let fallback = fallback.map(str::to_owned).or_else(|| {
                list_sinks().ok().and_then(|sinks| {
                    sinks
                        .into_iter()
                        .find(|sink| sink.name != VIRTUAL_SINK_NAME)
                        .map(|sink| sink.name)
                })
            });
            if let Some(fallback) = fallback {
                set_default_sink(&fallback)?;
            }
        }

        for module_index in stale {
            unload_module(module_index)?;
        }
        Ok(())
    }
}

fn to_audio_device(sink: PactlSink) -> AudioDevice {
    let icon_name = string_property(&sink.properties, "device.icon_name")
        .unwrap_or("audio-speakers-symbolic")
        .to_owned();
    let description = sink
        .description
        .filter(|description| !description.trim().is_empty())
        .unwrap_or_else(|| sink.name.clone());
    let state = match sink.state.as_deref() {
        Some("RUNNING") => "Playing",
        Some("IDLE") => "Ready",
        Some("SUSPENDED") => "Available",
        _ => "Audio output",
    };
    let detail = sink
        .sample_specification
        .filter(|specification| !specification.trim().is_empty())
        .map_or_else(
            || state.to_owned(),
            |specification| format!("{state} · {specification}"),
        );
    let volume_percent = average_volume_percent(&sink.volume);

    AudioDevice {
        index: sink.index,
        name: sink.name,
        description,
        detail,
        icon_name,
        volume_percent,
    }
}

fn average_volume_percent(volume: &HashMap<String, PactlChannelVolume>) -> u32 {
    let percentages = volume
        .values()
        .filter_map(|channel| {
            channel
                .value_percent
                .trim_end_matches('%')
                .parse::<u32>()
                .ok()
        })
        .collect::<Vec<_>>();

    if percentages.is_empty() {
        100
    } else {
        (percentages.iter().sum::<u32>() / percentages.len() as u32).min(100)
    }
}

fn string_property<'a>(properties: &'a HashMap<String, Value>, key: &str) -> Option<&'a str> {
    properties.get(key).and_then(Value::as_str)
}

fn list_sinks() -> Result<Vec<PactlSink>, AudioError> {
    parse_json(&run_pactl(&[
        "--format=json".to_owned(),
        "list".to_owned(),
        "sinks".to_owned(),
    ])?)
}

fn list_modules() -> Result<Vec<PactlModule>, AudioError> {
    parse_json(&run_pactl(&[
        "--format=json".to_owned(),
        "list".to_owned(),
        "modules".to_owned(),
    ])?)
}

fn list_sink_inputs() -> Result<Vec<PactlSinkInput>, AudioError> {
    parse_json(&run_pactl(&[
        "--format=json".to_owned(),
        "list".to_owned(),
        "sink-inputs".to_owned(),
    ])?)
}

fn parse_json<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T, AudioError> {
    serde_json::from_str(value).map_err(|error| AudioError::InvalidData(error.to_string()))
}

fn get_default_sink() -> Result<String, AudioError> {
    let value = run_pactl(&["get-default-sink".to_owned()])?;
    let value = value.trim();
    if value.is_empty() {
        Err(AudioError::InvalidData(
            "default output was empty".to_owned(),
        ))
    } else {
        Ok(value.to_owned())
    }
}

fn set_default_sink(name: &str) -> Result<(), AudioError> {
    run_pactl(&["set-default-sink".to_owned(), name.to_owned()]).map(|_| ())
}

fn move_sink_input(index: u32, sink: &str) -> Result<(), AudioError> {
    run_pactl(&[
        "move-sink-input".to_owned(),
        index.to_string(),
        sink.to_owned(),
    ])
    .map(|_| ())
}

fn unload_module(index: u32) -> Result<(), AudioError> {
    run_pactl(&["unload-module".to_owned(), index.to_string()]).map(|_| ())
}

fn wait_for_sink(name: &str) -> Result<(), AudioError> {
    for _ in 0..10 {
        if list_sinks()?.iter().any(|sink| sink.name == name) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }

    Err(AudioError::Command(
        "the split output did not appear in PipeWire".to_owned(),
    ))
}

fn module_arguments(outputs: &[String], latency_compensation: bool) -> Vec<String> {
    vec![
        "load-module".to_owned(),
        "module-combine-sink".to_owned(),
        format!("sink_name={VIRTUAL_SINK_NAME}"),
        format!("sinks={}", outputs.join(",")),
        format!("sink_properties='device.description=\"{VIRTUAL_SINK_DESCRIPTION}\"'"),
        format!("latency_compensate={latency_compensation}"),
    ]
}

fn is_safe_pulse_name(name: &str) -> bool {
    !name.is_empty()
        && name != VIRTUAL_SINK_NAME
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn run_pactl(args: &[String]) -> Result<String, AudioError> {
    let output = Command::new("pactl")
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .map_err(|error| AudioError::Unavailable(error.to_string()))?;

    if output.status.success() {
        String::from_utf8(output.stdout).map_err(|error| AudioError::InvalidData(error.to_string()))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let message = if stderr.is_empty() {
            format!("pactl exited with {}", output.status)
        } else {
            stderr
        };
        Err(AudioError::Command(message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SINKS_JSON: &str = r#"
    [
      {
        "index": 41,
        "state": "RUNNING",
        "name": "alsa_output.usb-DAC.analog-stereo",
        "description": "USB DAC",
        "sample_specification": "float32le 2ch 48000Hz",
        "volume": {
          "front-left": {"value": 42598, "value_percent": "65%", "db": "-11.21 dB"},
          "front-right": {"value": 42598, "value_percent": "65%", "db": "-11.21 dB"}
        },
        "properties": {"device.icon_name": "audio-card-usb"}
      },
      {
        "index": 42,
        "state": "SUSPENDED",
        "name": "alsa_output.pci-HDMI.hdmi-stereo",
        "description": "Living Room TV",
        "properties": {}
      }
    ]"#;

    const MODULES_JSON: &str = r#"
    [
      {
        "name": "libpipewire-module-rt",
        "argument": "{ rt.prio = 60 }",
        "properties": {"object.id": "1"}
      },
      {
        "index": 536870916,
        "name": "module-combine-sink",
        "argument": "sink_name=cosmic_audio_splitter sinks=alsa_output.one,bluez_output.two"
      }
    ]"#;

    #[test]
    fn parses_pipewire_sinks() {
        let sinks: Vec<PactlSink> = parse_json(SINKS_JSON).expect("fixture should parse");
        let first = to_audio_device(sinks.into_iter().next().expect("one sink"));

        assert_eq!(first.index, 41);
        assert_eq!(first.description, "USB DAC");
        assert_eq!(first.icon_name, "audio-card-usb");
        assert_eq!(first.detail, "Playing · float32le 2ch 48000Hz");
        assert_eq!(first.volume_percent, 65);
    }

    #[test]
    fn defaults_to_full_volume_when_pipewire_omits_volume_data() {
        let sinks: Vec<PactlSink> = parse_json(SINKS_JSON).expect("fixture should parse");
        let second = to_audio_device(sinks.into_iter().nth(1).expect("second sink"));

        assert_eq!(second.volume_percent, 100);
    }

    #[test]
    fn accepts_builtin_modules_without_an_index() {
        let modules: Vec<PactlModule> =
            parse_json(MODULES_JSON).expect("PipeWire module fixture should parse");

        assert_eq!(modules[0].index, None);
        assert_eq!(modules[1].index, Some(536_870_916));
    }

    #[test]
    fn module_arguments_target_each_selected_output() {
        let outputs = vec!["alsa_output.one".to_owned(), "bluez_output.two".to_owned()];
        let arguments = module_arguments(&outputs, false);

        assert!(
            arguments
                .iter()
                .any(|arg| arg == "sinks=alsa_output.one,bluez_output.two")
        );
        assert!(
            arguments
                .iter()
                .any(|arg| arg == "sink_properties='device.description=\"Audio Splitter\"'")
        );
        assert!(
            arguments
                .iter()
                .any(|arg| arg == "latency_compensate=false")
        );
    }

    #[test]
    fn only_accepts_pulseaudio_object_names() {
        assert!(is_safe_pulse_name(
            "alsa_output.pci-0000_00_1f.3.analog-stereo"
        ));
        assert!(!is_safe_pulse_name("output with spaces"));
        assert!(!is_safe_pulse_name("output;bad"));
        assert!(!is_safe_pulse_name(VIRTUAL_SINK_NAME));
    }
}
