// SPDX-License-Identifier: MIT

use super::{
    AudioBackend, AudioError, InputRecord, ModuleRecord, SinkRecord, VIRTUAL_SINK_DESCRIPTION,
    VIRTUAL_SINK_NAME,
};
use cosmic::iced::futures::StreamExt;
use cosmic::iced::futures::stream::{self, BoxStream};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command as TokioCommand};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(4);
const MONITOR_RETRY_DELAY: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
pub(super) struct PactlBackend {
    command: PathBuf,
    timeout: Duration,
}

impl Default for PactlBackend {
    fn default() -> Self {
        Self {
            command: PathBuf::from("pactl"),
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl PactlBackend {
    #[cfg(test)]
    fn for_test(command: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            command: command.into(),
            timeout,
        }
    }

    fn run(&self, args: &[String]) -> Result<String, AudioError> {
        let mut child = Command::new(&self.command)
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| AudioError::Unavailable(error.to_string()))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AudioError::InvalidData("pactl stdout was not available".to_owned()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AudioError::InvalidData("pactl stderr was not available".to_owned()))?;
        let stdout_reader = thread::spawn(move || read_all(stdout));
        let stderr_reader = thread::spawn(move || read_all(stderr));

        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(AudioError::Command(error.to_string()));
                }
            }

            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(AudioError::Timeout(format!("pactl {}", args.join(" "))));
            }

            thread::sleep(Duration::from_millis(20));
        };

        let stdout = join_reader(stdout_reader, "stdout")?;
        let stderr = join_reader(stderr_reader, "stderr")?;
        command_result(status, stdout, stderr)
    }

    fn json<T: for<'de> Deserialize<'de>>(&self, args: &[&str]) -> Result<T, AudioError> {
        let args = args
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        let output = self.run(&args)?;
        serde_json::from_str(&output).map_err(|error| AudioError::InvalidData(error.to_string()))
    }
}

impl AudioBackend for PactlBackend {
    fn server_name(&self) -> Result<String, AudioError> {
        let output = self.run(&["info".to_owned()])?;
        output
            .lines()
            .find_map(|line| line.strip_prefix("Server Name:").map(str::trim))
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| AudioError::InvalidData("pactl did not report a server name".to_owned()))
    }

    fn list_sinks(&self) -> Result<Vec<SinkRecord>, AudioError> {
        let sinks: Vec<PactlSink> = self.json(&["--format=json", "list", "sinks"])?;
        Ok(sinks
            .into_iter()
            .map(|sink| SinkRecord {
                index: sink.index,
                name: sink.name,
                description: sink.description,
                state: sink.state,
                sample_specification: sink.sample_specification,
                volume_percent: average_volume_percent(&sink.volume),
                icon_name: string_property(&sink.properties, "device.icon_name")
                    .unwrap_or("audio-speakers-symbolic")
                    .to_owned(),
            })
            .collect())
    }

    fn list_modules(&self) -> Result<Vec<ModuleRecord>, AudioError> {
        let modules: Vec<PactlModule> = self.json(&["--format=json", "list", "modules"])?;
        Ok(modules
            .into_iter()
            .map(|module| ModuleRecord {
                index: module.index,
                name: module.name,
                argument: module.argument,
            })
            .collect())
    }

    fn list_inputs(&self) -> Result<Vec<InputRecord>, AudioError> {
        let inputs: Vec<PactlSinkInput> = self.json(&["--format=json", "list", "sink-inputs"])?;
        Ok(inputs
            .into_iter()
            .map(|input| InputRecord {
                index: input.index,
                sink: input.sink,
            })
            .collect())
    }

    fn default_sink(&self) -> Result<String, AudioError> {
        let output = self.run(&["get-default-sink".to_owned()])?;
        let value = output.trim();
        if value.is_empty() {
            Err(AudioError::InvalidData(
                "default output was empty".to_owned(),
            ))
        } else {
            Ok(value.to_owned())
        }
    }

    fn load_combine_sink(
        &self,
        outputs: &[String],
        latency_compensation: bool,
    ) -> Result<u32, AudioError> {
        let args = vec![
            "load-module".to_owned(),
            "module-combine-sink".to_owned(),
            format!("sink_name={VIRTUAL_SINK_NAME}"),
            format!("sinks={}", outputs.join(",")),
            format!("sink_properties='device.description=\"{VIRTUAL_SINK_DESCRIPTION}\"'"),
            format!("latency_compensate={latency_compensation}"),
        ];
        self.run(&args)?
            .trim()
            .parse::<u32>()
            .map_err(|error| AudioError::InvalidData(format!("invalid module ID: {error}")))
    }

    fn set_default_sink(&self, name: &str) -> Result<(), AudioError> {
        self.run(&["set-default-sink".to_owned(), name.to_owned()])
            .map(|_| ())
    }

    fn move_input(&self, index: u32, sink: &str) -> Result<(), AudioError> {
        self.run(&[
            "move-sink-input".to_owned(),
            index.to_string(),
            sink.to_owned(),
        ])
        .map(|_| ())
    }

    fn unload_module(&self, index: u32) -> Result<(), AudioError> {
        self.run(&["unload-module".to_owned(), index.to_string()])
            .map(|_| ())
    }

    fn set_volume(&self, name: &str, volume_percent: u32) -> Result<(), AudioError> {
        self.run(&[
            "set-sink-volume".to_owned(),
            name.to_owned(),
            format!("{}%", volume_percent.min(100)),
        ])
        .map(|_| ())
    }
}

pub(super) fn event_stream() -> BoxStream<'static, ()> {
    struct MonitorState {
        child: Option<Child>,
        lines: Option<Lines<BufReader<ChildStdout>>>,
    }

    stream::unfold(
        MonitorState {
            child: None,
            lines: None,
        },
        |mut state| async move {
            loop {
                if state.lines.is_none() {
                    let mut command = TokioCommand::new("pactl");
                    command
                        .arg("subscribe")
                        .env("LC_ALL", "C")
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null())
                        .kill_on_drop(true);

                    match command.spawn() {
                        Ok(mut child) => {
                            let Some(stdout) = child.stdout.take() else {
                                tokio::time::sleep(MONITOR_RETRY_DELAY).await;
                                continue;
                            };
                            state.lines = Some(BufReader::new(stdout).lines());
                            state.child = Some(child);
                        }
                        Err(_) => {
                            tokio::time::sleep(MONITOR_RETRY_DELAY).await;
                            continue;
                        }
                    }
                }

                let next = match state.lines.as_mut() {
                    Some(lines) => lines.next_line().await,
                    None => continue,
                };

                match next {
                    Ok(Some(line)) if is_relevant_event(&line) => return Some(((), state)),
                    Ok(Some(_)) => continue,
                    Ok(None) | Err(_) => {
                        if let Some(mut child) = state.child.take() {
                            let _ = child.kill().await;
                        }
                        state.lines = None;
                        tokio::time::sleep(MONITOR_RETRY_DELAY).await;
                        continue;
                    }
                }
            }
        },
    )
    .boxed()
}

fn is_relevant_event(line: &str) -> bool {
    line.contains(" on sink ")
        || line.contains(" on sink-input ")
        || line.contains(" on server ")
        || line.contains(" on module ")
}

fn read_all(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn join_reader(
    reader: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    stream_name: &str,
) -> Result<Vec<u8>, AudioError> {
    reader
        .join()
        .map_err(|_| AudioError::InvalidData(format!("pactl {stream_name} reader panicked")))?
        .map_err(|error| {
            AudioError::InvalidData(format!("could not read pactl {stream_name}: {error}"))
        })
}

fn command_result(
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
) -> Result<String, AudioError> {
    if status.success() {
        String::from_utf8(stdout).map_err(|error| AudioError::InvalidData(error.to_string()))
    } else {
        let stderr = String::from_utf8_lossy(&stderr).trim().to_owned();
        let message = if stderr.is_empty() {
            format!("pactl exited with {status}")
        } else {
            stderr
        };
        Err(AudioError::Command(message))
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
    index: Option<u32>,
    name: String,
    argument: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PactlSinkInput {
    index: u32,
    sink: u32,
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
          "front-left": {"value_percent": "65%"},
          "front-right": {"value_percent": "65%"}
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
        "argument": "sink_name=cosmic_audio_splitter sinks=one,two"
      }
    ]"#;

    #[test]
    fn filters_subscription_events() {
        assert!(is_relevant_event("Event 'change' on sink #42"));
        assert!(is_relevant_event("Event 'new' on sink-input #7"));
        assert!(is_relevant_event("Event 'remove' on module #9"));
        assert!(!is_relevant_event("Event 'change' on source #2"));
    }

    #[test]
    fn parses_sink_data_and_volume() {
        let sinks: Vec<PactlSink> = serde_json::from_str(SINKS_JSON).expect("sinks should parse");
        assert_eq!(sinks[0].index, 41);
        assert_eq!(sinks[0].description.as_deref(), Some("USB DAC"));
        assert_eq!(average_volume_percent(&sinks[0].volume), 65);
        assert_eq!(average_volume_percent(&sinks[1].volume), 100);
    }

    #[test]
    fn accepts_builtin_modules_without_an_index() {
        let modules: Vec<PactlModule> =
            serde_json::from_str(MODULES_JSON).expect("modules should parse");
        assert_eq!(modules[0].index, None);
        assert_eq!(modules[1].index, Some(536_870_916));
    }

    #[test]
    fn terminates_commands_that_exceed_the_timeout() {
        let backend = PactlBackend::for_test("/bin/sh", Duration::from_millis(20));
        let error = backend
            .run(&["-c".to_owned(), "sleep 1".to_owned()])
            .expect_err("command should time out");
        assert!(matches!(error, AudioError::Timeout(_)));
    }
}
