// SPDX-License-Identifier: MIT

//! Transactional PipeWire routing through its PulseAudio-compatible protocol.
//!
//! The UI only talks to [`AudioController`]. Command execution, persistent
//! recovery state, and routing transactions remain behind replaceable traits.

mod journal;
mod pactl;

use cosmic::iced::Subscription;
use journal::{FileSessionStore, SessionStore};
use pactl::PactlBackend;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::Duration;
use thiserror::Error;

pub const VIRTUAL_SINK_NAME: &str = "cosmic_audio_splitter";
pub(super) const VIRTUAL_SINK_DESCRIPTION: &str = "Audio Splitter";
static AUDIO_TRANSACTION: Mutex<()> = Mutex::new(());

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
    pub split_active: bool,
    pub notice: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MovedInput {
    input_index: u32,
    original_sink: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    Starting,
    Active,
    Stopping,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SplitSession {
    pub module_index: Option<u32>,
    pub previous_default: Option<String>,
    pub outputs: Vec<String>,
    moved_inputs: Vec<MovedInput>,
    phase: SessionPhase,
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
    #[error("audio control timed out while running {0}")]
    Timeout(String),
    #[error("audio command failed: {0}")]
    Command(String),
    #[error("PipeWire returned data that could not be read: {0}")]
    InvalidData(String),
    #[error("Audio Splitter requires pipewire-pulse, but the current server is {0}")]
    UnsupportedServer(String),
    #[error("select at least two available audio outputs")]
    NotEnoughOutputs,
    #[error("an audio output has an unsupported internal name: {0}")]
    UnsafeName(String),
    #[error("these selected outputs are no longer available: {0}")]
    MissingOutputs(String),
    #[error("session recovery state failed: {0}")]
    Journal(String),
    #[error("audio splitting could not start: {0}")]
    StartFailed(String),
    #[error("audio splitting could not be stopped safely: {0}")]
    StopFailed(String),
}

#[derive(Clone, Debug)]
pub(super) struct SinkRecord {
    index: u32,
    name: String,
    description: Option<String>,
    state: Option<String>,
    sample_specification: Option<String>,
    volume_percent: u32,
    icon_name: String,
}

#[derive(Clone, Debug)]
pub(super) struct ModuleRecord {
    index: Option<u32>,
    name: String,
    argument: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct InputRecord {
    index: u32,
    sink: u32,
}

pub(super) trait AudioBackend {
    fn server_name(&self) -> Result<String, AudioError>;
    fn list_sinks(&self) -> Result<Vec<SinkRecord>, AudioError>;
    fn list_modules(&self) -> Result<Vec<ModuleRecord>, AudioError>;
    fn list_inputs(&self) -> Result<Vec<InputRecord>, AudioError>;
    fn default_sink(&self) -> Result<String, AudioError>;
    fn load_combine_sink(
        &self,
        outputs: &[String],
        latency_compensation: bool,
    ) -> Result<u32, AudioError>;
    fn set_default_sink(&self, name: &str) -> Result<(), AudioError>;
    fn move_input(&self, index: u32, sink: &str) -> Result<(), AudioError>;
    fn unload_module(&self, index: u32) -> Result<(), AudioError>;
    fn set_volume(&self, name: &str, volume_percent: u32) -> Result<(), AudioError>;
}

pub struct AudioController;

impl AudioController {
    pub fn prepare() -> Result<AudioSnapshot, AudioError> {
        let _guard = transaction_guard()?;
        system_service()?.prepare()
    }

    pub fn discover() -> Result<AudioSnapshot, AudioError> {
        system_service()?.discover()
    }

    pub fn start(
        outputs: Vec<String>,
        latency_compensation: bool,
    ) -> Result<StartOutcome, AudioError> {
        let _guard = transaction_guard()?;
        system_service()?.start(outputs, latency_compensation)
    }

    pub fn stop(session: &SplitSession) -> Result<(), AudioError> {
        let _guard = transaction_guard()?;
        system_service()?.stop(session)
    }

    pub fn set_volume(name: &str, volume_percent: u32) -> Result<(), AudioError> {
        if !is_safe_pulse_name(name) {
            return Err(AudioError::UnsafeName(name.to_owned()));
        }
        PactlBackend::default().set_volume(name, volume_percent)
    }

    pub fn subscription() -> Subscription<()> {
        Subscription::run(pactl::event_stream)
    }
}

fn transaction_guard() -> Result<MutexGuard<'static, ()>, AudioError> {
    AUDIO_TRANSACTION
        .lock()
        .map_err(|_| AudioError::Command("the audio transaction lock was poisoned".to_owned()))
}

fn system_service() -> Result<AudioService<PactlBackend, FileSessionStore>, AudioError> {
    Ok(AudioService::new(
        PactlBackend::default(),
        FileSessionStore::system()?,
    ))
}

struct AudioService<B, S> {
    backend: B,
    store: S,
}

impl<B: AudioBackend, S: SessionStore> AudioService<B, S> {
    fn new(backend: B, store: S) -> Self {
        Self { backend, store }
    }

    fn prepare(&self) -> Result<AudioSnapshot, AudioError> {
        self.ensure_pipewire()?;
        let loaded = self.store.load()?;
        let mut notices = loaded.warning.into_iter().collect::<Vec<_>>();

        if let Some(session) = loaded.session {
            self.stop_transaction(&session)?;
            notices.push("Recovered the audio route from an interrupted session.".to_owned());
        } else {
            let removed = self.cleanup_owned_modules(None)?;
            if removed > 0 {
                notices.push(format!(
                    "Removed {removed} temporary audio route(s) left by an interrupted session."
                ));
            }
        }

        let mut snapshot = self.discover()?;
        snapshot.notice = combine_notices(notices);
        Ok(snapshot)
    }

    fn discover(&self) -> Result<AudioSnapshot, AudioError> {
        let sinks = self.backend.list_sinks()?;
        let split_active = sinks.iter().any(|sink| sink.name == VIRTUAL_SINK_NAME);
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
            default_sink: Some(self.backend.default_sink()?),
            split_active,
            notice: None,
        })
    }

    fn start(
        &self,
        outputs: Vec<String>,
        latency_compensation: bool,
    ) -> Result<StartOutcome, AudioError> {
        self.ensure_pipewire()?;

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

        let existing = self.store.load()?;
        let mut warnings = existing.warning.into_iter().collect::<Vec<_>>();
        if let Some(session) = existing.session {
            self.stop_transaction(&session)?;
            warnings.push("Recovered an unfinished audio session before starting.".to_owned());
        }
        let removed = self.cleanup_owned_modules(outputs.first().map(String::as_str))?;
        if removed > 0 {
            warnings.push(format!("Removed {removed} stale temporary audio route(s)."));
        }

        let available = self.backend.list_sinks()?;
        let available_names = available
            .iter()
            .filter(|sink| sink.name != VIRTUAL_SINK_NAME)
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

        let sink_names_by_index = available
            .iter()
            .map(|sink| (sink.index, sink.name.clone()))
            .collect::<HashMap<_, _>>();
        let moved_inputs = self
            .backend
            .list_inputs()?
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

        let mut session = SplitSession {
            module_index: None,
            previous_default: Some(self.backend.default_sink()?),
            outputs,
            moved_inputs,
            phase: SessionPhase::Starting,
        };
        self.store.save(&session)?;

        let start_result = (|| {
            let module_index = self
                .backend
                .load_combine_sink(&session.outputs, latency_compensation)?;
            session.module_index = Some(module_index);
            self.store.save(&session)?;

            self.wait_for_sink(VIRTUAL_SINK_NAME)?;
            self.backend.set_default_sink(VIRTUAL_SINK_NAME)?;

            let mut move_failures = 0usize;
            for input in &session.moved_inputs {
                if self
                    .backend
                    .move_input(input.input_index, VIRTUAL_SINK_NAME)
                    .is_err()
                {
                    move_failures += 1;
                }
            }
            if move_failures > 0 {
                warnings.push(format!(
                    "{move_failures} audio stream(s) ended before they could be moved; new audio will still use the split output"
                ));
            }

            session.phase = SessionPhase::Active;
            self.store.save(&session)
        })();

        if let Err(error) = start_result {
            let rollback = self.stop_transaction(&session).err();
            let message = rollback.map_or_else(
                || error.to_string(),
                |rollback| format!("{error}; rollback also failed: {rollback}"),
            );
            return Err(AudioError::StartFailed(message));
        }

        Ok(StartOutcome {
            session,
            warning: combine_notices(warnings),
        })
    }

    fn stop(&self, session: &SplitSession) -> Result<(), AudioError> {
        self.stop_transaction(session)
    }

    fn stop_transaction(&self, session: &SplitSession) -> Result<(), AudioError> {
        let mut warnings = Vec::new();
        let mut stopping = session.clone();
        stopping.phase = SessionPhase::Stopping;
        if let Err(error) = self.store.save(&stopping) {
            warnings.push(error.to_string());
        }

        let sinks = self.backend.list_sinks().map_err(|error| {
            AudioError::StopFailed(format!(
                "could not inspect outputs before recovery: {error}"
            ))
        })?;
        let available_names = sinks
            .iter()
            .map(|sink| sink.name.as_str())
            .collect::<HashSet<_>>();
        let virtual_index = sinks
            .iter()
            .find(|sink| sink.name == VIRTUAL_SINK_NAME)
            .map(|sink| sink.index);
        let fallback = choose_fallback(session, &sinks);

        if self.backend.default_sink().ok().as_deref() == Some(VIRTUAL_SINK_NAME) {
            if let Some(fallback) = fallback.as_deref() {
                if let Err(error) = self.backend.set_default_sink(fallback) {
                    warnings.push(error.to_string());
                }
            } else {
                warnings.push("no physical output is available to become the default".to_owned());
            }
        }

        if let Some(virtual_index) = virtual_index {
            match self.backend.list_inputs() {
                Ok(inputs) => {
                    let originals = session
                        .moved_inputs
                        .iter()
                        .map(|input| (input.input_index, input.original_sink.as_str()))
                        .collect::<HashMap<_, _>>();
                    for input in inputs
                        .into_iter()
                        .filter(|input| input.sink == virtual_index)
                    {
                        let destination = originals
                            .get(&input.index)
                            .copied()
                            .filter(|name| available_names.contains(name))
                            .or(fallback.as_deref());
                        if let Some(destination) = destination
                            && let Err(error) = self.backend.move_input(input.index, destination)
                        {
                            warnings.push(error.to_string());
                        }
                    }
                }
                Err(error) => warnings.push(error.to_string()),
            }
        }

        let mut module_indices = match self.backend.list_modules() {
            Ok(modules) => owned_module_indices(&modules),
            Err(error) => {
                warnings.push(error.to_string());
                session.module_index.into_iter().collect()
            }
        };
        if let Some(module_index) = session.module_index
            && !module_indices.contains(&module_index)
            && virtual_index.is_some()
        {
            module_indices.push(module_index);
        }
        for module_index in module_indices {
            if let Err(error) = self.backend.unload_module(module_index) {
                warnings.push(error.to_string());
            }
        }

        let final_sinks = self.backend.list_sinks().map_err(|error| {
            AudioError::StopFailed(format!("could not verify recovered outputs: {error}"))
        })?;
        let virtual_remains = final_sinks
            .iter()
            .any(|sink| sink.name == VIRTUAL_SINK_NAME);
        let mut default_is_virtual =
            self.backend.default_sink().ok().as_deref() == Some(VIRTUAL_SINK_NAME);
        if default_is_virtual
            && let Some(fallback) = choose_fallback(session, &final_sinks)
            && self.backend.set_default_sink(&fallback).is_ok()
        {
            default_is_virtual =
                self.backend.default_sink().ok().as_deref() == Some(VIRTUAL_SINK_NAME);
        }

        if virtual_remains || default_is_virtual {
            warnings.push("the temporary output is still active after recovery".to_owned());
            return Err(AudioError::StopFailed(warnings.join("; ")));
        }

        self.store.clear()?;
        for warning in warnings {
            tracing::warn!(%warning, "non-fatal audio recovery warning");
        }
        Ok(())
    }

    fn cleanup_owned_modules(&self, preferred_fallback: Option<&str>) -> Result<usize, AudioError> {
        let modules = self.backend.list_modules()?;
        let owned = owned_module_indices(&modules);
        if owned.is_empty() {
            return Ok(0);
        }

        let session = SplitSession {
            module_index: owned.first().copied(),
            previous_default: preferred_fallback.map(str::to_owned),
            outputs: preferred_fallback.into_iter().map(str::to_owned).collect(),
            moved_inputs: Vec::new(),
            phase: SessionPhase::Stopping,
        };
        self.stop_transaction(&session)?;
        Ok(owned.len())
    }

    fn wait_for_sink(&self, name: &str) -> Result<(), AudioError> {
        for _ in 0..20 {
            if self
                .backend
                .list_sinks()?
                .iter()
                .any(|sink| sink.name == name)
            {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
        }
        Err(AudioError::Command(
            "the split output did not appear in PipeWire".to_owned(),
        ))
    }

    fn ensure_pipewire(&self) -> Result<(), AudioError> {
        let server_name = self.backend.server_name()?;
        if server_name.to_ascii_lowercase().contains("pipewire") {
            Ok(())
        } else {
            Err(AudioError::UnsupportedServer(server_name))
        }
    }
}

fn choose_fallback(session: &SplitSession, sinks: &[SinkRecord]) -> Option<String> {
    let available = sinks
        .iter()
        .filter(|sink| sink.name != VIRTUAL_SINK_NAME)
        .map(|sink| sink.name.as_str())
        .collect::<HashSet<_>>();
    session
        .previous_default
        .as_deref()
        .filter(|name| available.contains(name))
        .or_else(|| {
            session
                .outputs
                .iter()
                .map(String::as_str)
                .find(|name| available.contains(name))
        })
        .or_else(|| {
            sinks
                .iter()
                .find(|sink| sink.name != VIRTUAL_SINK_NAME)
                .map(|sink| sink.name.as_str())
        })
        .map(str::to_owned)
}

fn owned_module_indices(modules: &[ModuleRecord]) -> Vec<u32> {
    modules
        .iter()
        .filter(|module| {
            module.name == "module-combine-sink"
                && module.argument.as_deref().is_some_and(|arguments| {
                    arguments
                        .split_whitespace()
                        .any(|arg| arg.strip_prefix("sink_name=") == Some(VIRTUAL_SINK_NAME))
                })
        })
        .filter_map(|module| module.index)
        .collect()
}

fn to_audio_device(sink: SinkRecord) -> AudioDevice {
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

    AudioDevice {
        index: sink.index,
        name: sink.name,
        description,
        detail,
        icon_name: sink.icon_name,
        volume_percent: sink.volume_percent,
    }
}

fn is_safe_pulse_name(name: &str) -> bool {
    !name.is_empty()
        && name != VIRTUAL_SINK_NAME
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn combine_notices(notices: Vec<String>) -> Option<String> {
    (!notices.is_empty()).then(|| notices.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use journal::JournalLoad;
    use std::sync::{Arc, Mutex};

    const SPEAKERS: &str = "alsa_output.speakers";
    const HEADPHONES: &str = "bluez_output.headphones";

    #[derive(Clone)]
    struct FakeBackend {
        state: Arc<Mutex<FakeState>>,
    }

    struct FakeState {
        server_name: String,
        sinks: Vec<SinkRecord>,
        modules: Vec<ModuleRecord>,
        inputs: Vec<InputRecord>,
        default_sink: String,
        fail_once: Option<&'static str>,
    }

    impl FakeBackend {
        fn healthy() -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeState {
                    server_name: "PulseAudio (on PipeWire 1.4.8)".to_owned(),
                    sinks: vec![sink(1, SPEAKERS), sink(2, HEADPHONES)],
                    modules: Vec::new(),
                    inputs: vec![InputRecord { index: 10, sink: 1 }],
                    default_sink: SPEAKERS.to_owned(),
                    fail_once: None,
                })),
            }
        }

        fn fail_once(&self, operation: &'static str) {
            self.state.lock().expect("state lock").fail_once = Some(operation);
        }

        fn maybe_fail(state: &mut FakeState, operation: &'static str) -> Result<(), AudioError> {
            if state.fail_once == Some(operation) {
                state.fail_once = None;
                Err(AudioError::Command(format!("injected {operation} failure")))
            } else {
                Ok(())
            }
        }
    }

    impl AudioBackend for FakeBackend {
        fn server_name(&self) -> Result<String, AudioError> {
            Ok(self.state.lock().expect("state lock").server_name.clone())
        }

        fn list_sinks(&self) -> Result<Vec<SinkRecord>, AudioError> {
            Ok(self.state.lock().expect("state lock").sinks.clone())
        }

        fn list_modules(&self) -> Result<Vec<ModuleRecord>, AudioError> {
            Ok(self.state.lock().expect("state lock").modules.clone())
        }

        fn list_inputs(&self) -> Result<Vec<InputRecord>, AudioError> {
            Ok(self.state.lock().expect("state lock").inputs.clone())
        }

        fn default_sink(&self) -> Result<String, AudioError> {
            Ok(self.state.lock().expect("state lock").default_sink.clone())
        }

        fn load_combine_sink(
            &self,
            outputs: &[String],
            _latency_compensation: bool,
        ) -> Result<u32, AudioError> {
            let mut state = self.state.lock().expect("state lock");
            Self::maybe_fail(&mut state, "load")?;
            state.modules.push(ModuleRecord {
                index: Some(42),
                name: "module-combine-sink".to_owned(),
                argument: Some(format!(
                    "sink_name={VIRTUAL_SINK_NAME} sinks={}",
                    outputs.join(",")
                )),
            });
            state.sinks.push(sink(99, VIRTUAL_SINK_NAME));
            Ok(42)
        }

        fn set_default_sink(&self, name: &str) -> Result<(), AudioError> {
            let mut state = self.state.lock().expect("state lock");
            Self::maybe_fail(&mut state, "set_default")?;
            state.default_sink = name.to_owned();
            Ok(())
        }

        fn move_input(&self, index: u32, sink_name: &str) -> Result<(), AudioError> {
            let mut state = self.state.lock().expect("state lock");
            Self::maybe_fail(&mut state, "move")?;
            let sink_index = state
                .sinks
                .iter()
                .find(|sink| sink.name == sink_name)
                .map(|sink| sink.index)
                .ok_or_else(|| AudioError::Command("missing destination".to_owned()))?;
            if let Some(input) = state.inputs.iter_mut().find(|input| input.index == index) {
                input.sink = sink_index;
            }
            Ok(())
        }

        fn unload_module(&self, index: u32) -> Result<(), AudioError> {
            let mut state = self.state.lock().expect("state lock");
            Self::maybe_fail(&mut state, "unload")?;
            state.modules.retain(|module| module.index != Some(index));
            state.sinks.retain(|sink| sink.name != VIRTUAL_SINK_NAME);
            Ok(())
        }

        fn set_volume(&self, _name: &str, _volume_percent: u32) -> Result<(), AudioError> {
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct MemoryStore {
        session: Arc<Mutex<Option<SplitSession>>>,
    }

    impl SessionStore for MemoryStore {
        fn load(&self) -> Result<JournalLoad, AudioError> {
            Ok(JournalLoad {
                session: self.session.lock().expect("journal lock").clone(),
                warning: None,
            })
        }

        fn save(&self, session: &SplitSession) -> Result<(), AudioError> {
            *self.session.lock().expect("journal lock") = Some(session.clone());
            Ok(())
        }

        fn clear(&self) -> Result<(), AudioError> {
            *self.session.lock().expect("journal lock") = None;
            Ok(())
        }
    }

    fn sink(index: u32, name: &str) -> SinkRecord {
        SinkRecord {
            index,
            name: name.to_owned(),
            description: Some(name.to_owned()),
            state: Some("IDLE".to_owned()),
            sample_specification: Some("float32le 2ch 48000Hz".to_owned()),
            volume_percent: 65,
            icon_name: "audio-speakers-symbolic".to_owned(),
        }
    }

    fn outputs() -> Vec<String> {
        vec![SPEAKERS.to_owned(), HEADPHONES.to_owned()]
    }

    #[test]
    fn start_and_stop_are_a_reversible_transaction() {
        let backend = FakeBackend::healthy();
        let store = MemoryStore::default();
        let service = AudioService::new(backend.clone(), store.clone());

        let outcome = service.start(outputs(), false).expect("start should work");
        {
            let state = backend.state.lock().expect("state lock");
            assert_eq!(state.default_sink, VIRTUAL_SINK_NAME);
            assert!(
                state
                    .sinks
                    .iter()
                    .any(|sink| sink.name == VIRTUAL_SINK_NAME)
            );
            assert_eq!(state.inputs[0].sink, 99);
        }
        assert_eq!(
            store
                .session
                .lock()
                .expect("journal lock")
                .as_ref()
                .map(|value| value.phase),
            Some(SessionPhase::Active)
        );

        service.stop(&outcome.session).expect("stop should work");
        let state = backend.state.lock().expect("state lock");
        assert_eq!(state.default_sink, SPEAKERS);
        assert!(
            !state
                .sinks
                .iter()
                .any(|sink| sink.name == VIRTUAL_SINK_NAME)
        );
        assert_eq!(state.inputs[0].sink, 1);
        assert!(store.session.lock().expect("journal lock").is_none());
    }

    #[test]
    fn stop_is_safe_to_repeat() {
        let backend = FakeBackend::healthy();
        let store = MemoryStore::default();
        let service = AudioService::new(backend.clone(), store.clone());
        let session = service
            .start(outputs(), false)
            .expect("start should work")
            .session;

        service.stop(&session).expect("first stop should work");
        service.stop(&session).expect("second stop should work");

        let state = backend.state.lock().expect("state lock");
        assert_eq!(state.default_sink, SPEAKERS);
        assert!(state.modules.is_empty());
        assert!(store.session.lock().expect("journal lock").is_none());
    }

    #[test]
    fn failed_start_rolls_back_module_and_journal() {
        let backend = FakeBackend::healthy();
        backend.fail_once("set_default");
        let store = MemoryStore::default();
        let service = AudioService::new(backend.clone(), store.clone());

        let error = service
            .start(outputs(), false)
            .expect_err("start should fail");
        assert!(matches!(error, AudioError::StartFailed(_)));
        let state = backend.state.lock().expect("state lock");
        assert_eq!(state.default_sink, SPEAKERS);
        assert!(state.modules.is_empty());
        assert!(
            !state
                .sinks
                .iter()
                .any(|sink| sink.name == VIRTUAL_SINK_NAME)
        );
        assert!(store.session.lock().expect("journal lock").is_none());
    }

    #[test]
    fn prepare_recovers_an_interrupted_active_session() {
        let backend = FakeBackend::healthy();
        let store = MemoryStore::default();
        let service = AudioService::new(backend.clone(), store.clone());
        let session = service
            .start(outputs(), true)
            .expect("start should work")
            .session;
        *store.session.lock().expect("journal lock") = Some(session);

        let snapshot = service.prepare().expect("recovery should work");
        assert!(
            snapshot
                .notice
                .as_deref()
                .is_some_and(|notice| notice.contains("Recovered"))
        );
        assert!(!snapshot.split_active);
        assert_eq!(snapshot.default_sink.as_deref(), Some(SPEAKERS));
        assert!(store.session.lock().expect("journal lock").is_none());
    }

    #[test]
    fn prepare_clears_a_journal_after_external_cleanup() {
        let backend = FakeBackend::healthy();
        let store = MemoryStore::default();
        *store.session.lock().expect("journal lock") = Some(SplitSession {
            module_index: Some(42),
            previous_default: Some(SPEAKERS.to_owned()),
            outputs: outputs(),
            moved_inputs: Vec::new(),
            phase: SessionPhase::Active,
        });
        let service = AudioService::new(backend, store.clone());

        let snapshot = service.prepare().expect("recovery should be idempotent");
        assert!(!snapshot.split_active);
        assert!(store.session.lock().expect("journal lock").is_none());
    }

    #[test]
    fn stop_uses_an_available_fallback_when_previous_output_disappears() {
        let backend = FakeBackend::healthy();
        let store = MemoryStore::default();
        let service = AudioService::new(backend.clone(), store);
        let session = service
            .start(outputs(), false)
            .expect("start should work")
            .session;
        backend
            .state
            .lock()
            .expect("state lock")
            .sinks
            .retain(|sink| sink.name != SPEAKERS);

        service.stop(&session).expect("fallback stop should work");
        assert_eq!(
            backend.state.lock().expect("state lock").default_sink,
            HEADPHONES
        );
    }

    #[test]
    fn rejects_non_pipewire_servers() {
        let backend = FakeBackend::healthy();
        backend.state.lock().expect("state lock").server_name = "pulseaudio".to_owned();
        let service = AudioService::new(backend, MemoryStore::default());

        assert!(matches!(
            service.prepare(),
            Err(AudioError::UnsupportedServer(_))
        ));
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
