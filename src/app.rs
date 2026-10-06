// SPDX-License-Identifier: MIT

use crate::audio::{
    AudioController, AudioDevice, AudioSnapshot, OutputControl, SplitSession, StartOutcome,
};
use crate::config::{AppConfig, AudioPreset, PresetOutput};
use cosmic::app::{Task, context_drawer};
use cosmic::iced::alignment::Horizontal;
use cosmic::iced::{Length, Subscription};
use cosmic::prelude::*;
use cosmic::widget::{self, about::About, icon, settings};
use std::collections::HashSet;

const APP_ICON: &[u8] = include_bytes!(
    "../resources/icons/hicolor/scalable/apps/io.github.okrroni.cosmic_audio_splitter.svg"
);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ContextPage {
    #[default]
    About,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Busy {
    Loading,
    Refreshing,
    Starting,
    Stopping,
}

#[derive(Clone, Debug)]
pub enum Message {
    AudioChanged,
    DevicesLoaded(Result<AudioSnapshot, String>),
    DeviceMuteSet(String, bool, Result<(), String>),
    DeviceVolumeChanged(String, u32),
    DeviceVolumeSet(Result<(), String>),
    DismissNotice,
    LaunchUrl(String),
    ApplyPreset(usize),
    DeletePreset(usize),
    PresetNameChanged(String),
    Refresh,
    SavePreset,
    Start,
    Started(Result<StartOutcome, String>),
    Stop,
    Stopped(Result<(), String>),
    SetDeviceVolume(String),
    SetDeviceMuted(String, bool),
    TestOutput(String),
    OutputTested(Result<(), String>),
    ToggleContextPage(ContextPage),
    ToggleLatencyCompensation(bool),
    ToggleOutput(String, bool),
}

pub struct AppModel {
    core: cosmic::Core,
    about: About,
    context_page: ContextPage,
    config: AppConfig,
    devices: Vec<AudioDevice>,
    default_sink: Option<String>,
    session: Option<SplitSession>,
    busy: Option<Busy>,
    testing_output: Option<String>,
    preset_name: String,
    pending_preset: Option<AudioPreset>,
    notice: Option<String>,
}

impl cosmic::Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = crate::Flags;
    type Message = Message;

    const APP_ID: &'static str = "io.github.okrroni.cosmic_audio_splitter";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, _flags: Self::Flags) -> (Self, Task<Self::Message>) {
        let about = About::default()
            .name("Audio Splitter")
            .icon(widget::icon::from_svg_bytes(APP_ICON))
            .version(env!("CARGO_PKG_VERSION"))
            .license(env!("CARGO_PKG_LICENSE"))
            .license_url("https://opensource.org/license/mit");

        let mut app = Self {
            core,
            about,
            context_page: ContextPage::default(),
            config: AppConfig::load(),
            devices: Vec::new(),
            default_sink: None,
            session: None,
            busy: Some(Busy::Loading),
            testing_output: None,
            preset_name: String::new(),
            pending_preset: None,
            notice: None,
        };
        app.set_header_title("Audio Splitter".into());

        let title_task = app.core.main_window_id().map_or_else(Task::none, |id| {
            app.set_window_title("Audio Splitter".into(), id)
        });
        let discovery_task = discover_task(true);

        (app, cosmic::task::batch([title_task, discovery_task]))
    }

    fn header_start(&self) -> Vec<Element<'_, Self::Message>> {
        let refresh = widget::button::standard("Refresh")
            .leading_icon(icon::from_name("view-refresh-symbolic"))
            .on_press_maybe(self.busy.is_none().then_some(Message::Refresh));
        vec![refresh.into()]
    }

    fn header_end(&self) -> Vec<Element<'_, Self::Message>> {
        let about =
            widget::button::text("About").on_press(Message::ToggleContextPage(ContextPage::About));
        vec![about.into()]
    }

    fn context_drawer(&self) -> Option<context_drawer::ContextDrawer<'_, Self::Message>> {
        if !self.core.window.show_context {
            return None;
        }

        Some(match self.context_page {
            ContextPage::About => context_drawer::about(
                &self.about,
                |url| Message::LaunchUrl(url.to_owned()),
                Message::ToggleContextPage(ContextPage::About),
            ),
        })
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        AudioController::subscription().map(|()| Message::AudioChanged)
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let spacing = cosmic::theme::spacing();
        let selected = self.present_selection();
        let selected_count = selected.len();
        let is_active = self.session.is_some();
        let is_busy = self.busy.is_some() || self.testing_output.is_some();

        let intro = widget::column::with_capacity(2)
            .push(widget::text::title1("Play audio everywhere"))
            .push(widget::text::body(
                "Choose two or more outputs. Audio Splitter creates one temporary output and routes current and new audio through it.",
            ))
            .spacing(spacing.space_xxs);

        let status_title;
        let status_description;
        let status_icon;
        let status_control: Element<'_, Message>;

        if let Some(busy) = self.busy {
            (status_title, status_description, status_icon) = match busy {
                Busy::Loading => (
                    "Finding audio outputs".to_owned(),
                    "Connecting to PipeWire…".to_owned(),
                    "view-refresh-symbolic",
                ),
                Busy::Refreshing => (
                    "Refreshing outputs".to_owned(),
                    "Checking the current PipeWire devices…".to_owned(),
                    "view-refresh-symbolic",
                ),
                Busy::Starting => (
                    "Starting audio split".to_owned(),
                    "Creating the combined output and moving active audio…".to_owned(),
                    "audio-speakers-symbolic",
                ),
                Busy::Stopping => (
                    "Stopping audio split".to_owned(),
                    "Restoring the previous default output…".to_owned(),
                    "audio-speakers-symbolic",
                ),
            };
            status_control = widget::text::caption("Please wait").into();
        } else if let Some(session) = &self.session {
            status_title = "Playing on multiple outputs".to_owned();
            status_description = format!(
                "Audio is currently mirrored to {} devices.",
                session.outputs.len()
            );
            status_icon = "audio-volume-high-symbolic";
            status_control = widget::button::destructive("Stop")
                .on_press_maybe((!is_busy).then_some(Message::Stop))
                .into();
        } else {
            status_title = if selected_count >= 2 {
                "Ready to split".to_owned()
            } else {
                "Select at least two outputs".to_owned()
            };
            status_description = if selected_count >= 2 {
                format!("{selected_count} outputs selected")
            } else {
                "The split starts only after two available outputs are selected.".to_owned()
            };
            status_icon = "audio-speakers-symbolic";
            status_control = widget::button::suggested("Start splitting")
                .on_press_maybe((selected_count >= 2 && !is_busy).then_some(Message::Start))
                .into();
        }

        let status = settings::section().title("Status").add(
            settings::item::builder(status_title)
                .description(status_description)
                .icon(icon::from_name(status_icon).size(24).icon())
                .control(status_control),
        );

        let mut outputs = settings::section().title("Audio outputs");
        if self.devices.is_empty() && self.busy != Some(Busy::Loading) {
            outputs = outputs.add(
                settings::item::builder("No outputs found")
                    .description("Connect an audio device, then choose Refresh.")
                    .icon(icon::from_name("audio-speakers-symbolic").size(24).icon())
                    .control(widget::text::caption("PipeWire")),
            );
        }

        for device in &self.devices {
            let name = device.name.clone();
            let checked = selected.contains(&device.name);
            let default_suffix = if self.default_sink.as_deref() == Some(device.name.as_str()) {
                " · Current default"
            } else {
                ""
            };
            let mute_suffix = if device.muted { " · Muted" } else { "" };
            let description = format!("{}{default_suffix}{mute_suffix}", device.detail);
            let item = settings::item::builder(device.description.clone())
                .description(description)
                .icon(icon::from_name(device.icon_name.clone()).size(24).icon());

            let test_label = if self.testing_output.as_deref() == Some(device.name.as_str()) {
                "Playing…"
            } else {
                "Test"
            };
            let test = widget::button::standard(test_label).on_press_maybe(
                (!is_busy && !device.muted).then(|| Message::TestOutput(name.clone())),
            );

            outputs = if is_active && !is_busy && checked {
                let change_name = name.clone();
                let slider = widget::slider(0..=100, device.volume_percent, move |value| {
                    Message::DeviceVolumeChanged(change_name.clone(), value)
                })
                .on_release(Message::SetDeviceVolume(name.clone()))
                .width(Length::Fixed(130.0));
                let mute = widget::button::standard(if device.muted { "Unmute" } else { "Mute" })
                    .on_press(Message::SetDeviceMuted(name.clone(), !device.muted));
                let controls = widget::row::with_capacity(6)
                    .push(widget::checkbox(true))
                    .push(test)
                    .push(mute)
                    .push(slider)
                    .push(
                        widget::text::caption(format!("{}%", device.volume_percent))
                            .width(Length::Fixed(40.0)),
                    )
                    .spacing(spacing.space_xs)
                    .align_y(cosmic::iced::Alignment::Center);
                outputs.add(item.control(controls))
            } else if is_active || is_busy {
                outputs.add(
                    item.control(
                        widget::row::with_capacity(2)
                            .push(widget::checkbox(checked))
                            .push(test)
                            .spacing(spacing.space_xs)
                            .align_y(cosmic::iced::Alignment::Center),
                    ),
                )
            } else {
                let toggle_name = name.clone();
                outputs.add(
                    item.control(
                        widget::row::with_capacity(2)
                            .push(widget::checkbox(checked).on_toggle(move |value| {
                                Message::ToggleOutput(toggle_name.clone(), value)
                            }))
                            .push(test)
                            .spacing(spacing.space_xs)
                            .align_y(cosmic::iced::Alignment::Center),
                    ),
                )
            };
        }

        let save_preset = widget::row::with_capacity(2)
            .push(
                widget::text_input::text_input("Preset name", &self.preset_name)
                    .on_input(Message::PresetNameChanged)
                    .on_submit(|_| Message::SavePreset)
                    .width(Length::Fixed(190.0)),
            )
            .push(
                widget::button::standard("Save")
                    .on_press_maybe((!is_active && !is_busy).then_some(Message::SavePreset)),
            )
            .spacing(spacing.space_xs)
            .align_y(cosmic::iced::Alignment::Center);
        let mut presets = settings::section().title("Presets").add(
            settings::item::builder("Save current setup")
                .description(
                    "Stores selected outputs, their levels, mute state, and delay setting.",
                )
                .icon(icon::from_name("document-save-symbolic").size(24).icon())
                .control(save_preset),
        );
        for (index, preset) in self.config.presets.iter().enumerate() {
            let available = preset
                .outputs
                .iter()
                .filter(|output| self.devices.iter().any(|device| device.name == output.name))
                .count();
            let description = format!(
                "{available}/{} outputs available · delay compensation {}",
                preset.outputs.len(),
                if preset.latency_compensation {
                    "on"
                } else {
                    "off"
                }
            );
            let controls = widget::row::with_capacity(2)
                .push(widget::button::standard("Apply").on_press_maybe(
                    (!is_active && !is_busy).then_some(Message::ApplyPreset(index)),
                ))
                .push(widget::button::standard("Delete").on_press_maybe(
                    (!is_active && !is_busy).then_some(Message::DeletePreset(index)),
                ))
                .spacing(spacing.space_xs);
            presets = presets.add(
                settings::item::builder(preset.name.clone())
                    .description(description)
                    .icon(icon::from_name("audio-speakers-symbolic").size(24).icon())
                    .control(controls),
            );
        }

        let advanced = settings::section().title("Advanced").add(
            settings::item::builder("Compensate device delay")
                .description(
                    "Keeps mismatched devices closer in sync, but may add buffering. Leave off for minimum latency.",
                )
                .toggler_maybe(
                    self.config.latency_compensation,
                    (!is_active && !is_busy).then_some(Message::ToggleLatencyCompensation),
                ),
        );

        let mut sections: Vec<Element<'_, Message>> = vec![
            intro.into(),
            status.into(),
            outputs.into(),
            presets.into(),
            advanced.into(),
        ];

        if let Some(notice) = &self.notice {
            sections.insert(
                1,
                settings::section()
                    .add(
                        settings::item::builder("Audio Splitter needs attention")
                            .description(notice)
                            .icon(icon::from_name("dialog-warning-symbolic").size(24).icon())
                            .control(
                                widget::button::standard("Dismiss")
                                    .on_press(Message::DismissNotice),
                            ),
                    )
                    .into(),
            );
        }

        let content = settings::view_column(sections)
            .width(Length::Fill)
            .padding([spacing.space_l, spacing.space_l]);

        widget::container(widget::scrollable(content).width(Length::Fill))
            .max_width(800)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .into()
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::AudioChanged => {
                if self.busy.is_none() && self.testing_output.is_none() {
                    self.busy = Some(Busy::Refreshing);
                    return discover_task(false);
                }
            }
            Message::DevicesLoaded(result) => {
                self.busy = None;
                match result {
                    Ok(snapshot) => {
                        let split_active = snapshot.split_active;
                        let recovery_notice = snapshot.notice;
                        self.devices = snapshot.devices;
                        self.default_sink = snapshot.default_sink;
                        self.reapply_pending_preset();
                        self.initialize_selection();

                        if let Some(notice) = recovery_notice {
                            self.notice = Some(notice);
                        }

                        if let Some(session) = &self.session {
                            if !split_active {
                                self.session = None;
                                self.notice = Some(
                                    "The temporary audio route stopped outside Audio Splitter. The available outputs were refreshed."
                                        .to_owned(),
                                );
                                self.busy = Some(Busy::Refreshing);
                                return discover_task(true);
                            } else {
                                let available = self
                                    .devices
                                    .iter()
                                    .map(|device| device.name.as_str())
                                    .collect::<HashSet<_>>();
                                let missing = session
                                    .outputs
                                    .iter()
                                    .filter(|output| !available.contains(output.as_str()))
                                    .cloned()
                                    .collect::<Vec<_>>();
                                if !missing.is_empty() {
                                    self.notice = Some(format!(
                                        "An active output disconnected ({}). The split will stop safely.",
                                        missing.join(", ")
                                    ));
                                    self.busy = Some(Busy::Stopping);
                                    return stop_task(session.clone());
                                }
                            }
                        }
                    }
                    Err(error) => {
                        self.notice = Some(error);
                    }
                }
            }
            Message::DeviceMuteSet(name, muted, result) => match result {
                Ok(()) => {
                    if let Some(device) = self.devices.iter_mut().find(|device| device.name == name)
                    {
                        device.muted = muted;
                    }
                }
                Err(error) => {
                    self.notice = Some(format!("Could not change device mute state: {error}"));
                }
            },
            Message::DeviceVolumeChanged(name, volume_percent) => {
                if let Some(device) = self.devices.iter_mut().find(|device| device.name == name) {
                    device.volume_percent = volume_percent;
                }
            }
            Message::DeviceVolumeSet(result) => {
                if let Err(error) = result {
                    self.notice = Some(format!("Could not change device volume: {error}"));
                }
            }
            Message::DismissNotice => self.notice = None,
            Message::LaunchUrl(url) => {
                if let Err(error) = open::that_detached(&url) {
                    self.notice = Some(format!("Could not open {url}: {error}"));
                }
            }
            Message::ApplyPreset(index) => {
                if self.busy.is_none() && self.session.is_none() {
                    self.apply_preset(index);
                }
            }
            Message::DeletePreset(index) => {
                if self.busy.is_none()
                    && self.session.is_none()
                    && index < self.config.presets.len()
                {
                    self.config.presets.remove(index);
                    self.save_config();
                }
            }
            Message::PresetNameChanged(value) => {
                self.preset_name = value.chars().take(64).collect();
            }
            Message::Refresh => {
                if self.busy.is_none() {
                    self.busy = Some(Busy::Refreshing);
                    return discover_task(false);
                }
            }
            Message::SavePreset => {
                if self.busy.is_none() && self.session.is_none() {
                    self.save_current_preset();
                }
            }
            Message::Start => {
                let outputs = self.present_selection();
                if self.busy.is_none()
                    && self.testing_output.is_none()
                    && self.session.is_none()
                    && outputs.len() >= 2
                {
                    self.busy = Some(Busy::Starting);
                    self.notice = None;
                    let latency_compensation = self.config.latency_compensation;
                    let controls = self.output_controls(&outputs);
                    return cosmic::task::future(async move {
                        Message::Started(
                            run_blocking(move || {
                                AudioController::configure_outputs(&controls)?;
                                AudioController::start(outputs, latency_compensation)
                            })
                            .await,
                        )
                    });
                }
            }
            Message::Started(result) => {
                self.busy = None;
                match result {
                    Ok(outcome) => {
                        self.notice = outcome.warning;
                        self.session = Some(outcome.session);
                        self.pending_preset = None;
                    }
                    Err(error) => self.notice = Some(error),
                }
            }
            Message::Stop => {
                if self.busy.is_none()
                    && let Some(session) = self.session.clone()
                {
                    self.busy = Some(Busy::Stopping);
                    return stop_task(session);
                }
            }
            Message::Stopped(result) => {
                self.busy = None;
                match result {
                    Ok(()) => {
                        self.session = None;
                        self.busy = Some(Busy::Refreshing);
                        return discover_task(false);
                    }
                    Err(error) => self.notice = Some(error),
                }
            }
            Message::SetDeviceVolume(name) => {
                let is_active_output = self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.outputs.iter().any(|output| output == &name));
                if self.busy.is_none()
                    && is_active_output
                    && let Some(volume_percent) = self
                        .devices
                        .iter()
                        .find(|device| device.name == name)
                        .map(|device| device.volume_percent)
                {
                    return cosmic::task::future(async move {
                        Message::DeviceVolumeSet(
                            run_blocking(move || {
                                AudioController::set_volume(&name, volume_percent)
                            })
                            .await,
                        )
                    });
                }
            }
            Message::SetDeviceMuted(name, muted) => {
                let is_active_output = self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.outputs.iter().any(|output| output == &name));
                if self.busy.is_none() && is_active_output {
                    let result_name = name.clone();
                    return cosmic::task::future(async move {
                        Message::DeviceMuteSet(
                            result_name,
                            muted,
                            run_blocking(move || AudioController::set_muted(&name, muted)).await,
                        )
                    });
                }
            }
            Message::TestOutput(name) => {
                if self.busy.is_none() && self.testing_output.is_none() {
                    self.testing_output = Some(name.clone());
                    return cosmic::task::future(async move {
                        Message::OutputTested(
                            run_blocking(move || AudioController::test_output(&name)).await,
                        )
                    });
                }
            }
            Message::OutputTested(result) => {
                self.testing_output = None;
                if let Err(error) = result {
                    self.notice = Some(format!("Could not play the output test: {error}"));
                }
            }
            Message::ToggleContextPage(page) => {
                if self.context_page == page {
                    self.core.window.show_context = !self.core.window.show_context;
                } else {
                    self.context_page = page;
                    self.core.window.show_context = true;
                }
            }
            Message::ToggleLatencyCompensation(value) => {
                self.config.latency_compensation = value;
                self.save_config();
            }
            Message::ToggleOutput(name, selected) => {
                self.pending_preset = None;
                if selected {
                    if !self.config.selected_outputs.contains(&name) {
                        self.config.selected_outputs.push(name);
                    }
                } else {
                    self.config
                        .selected_outputs
                        .retain(|output| output != &name);
                }
                self.config.selection_initialized = true;
                self.save_config();
            }
        }

        Task::none()
    }

    fn on_app_exit(&mut self) -> Option<Self::Message> {
        self.stop_before_exit();
        None
    }
}

impl AppModel {
    fn output_controls(&self, outputs: &[String]) -> Vec<OutputControl> {
        let selected = outputs.iter().map(String::as_str).collect::<HashSet<_>>();
        self.devices
            .iter()
            .filter(|device| selected.contains(device.name.as_str()))
            .map(|device| OutputControl {
                name: device.name.clone(),
                volume_percent: device.volume_percent,
                muted: device.muted,
            })
            .collect()
    }

    fn reapply_pending_preset(&mut self) {
        let Some(preset) = &self.pending_preset else {
            return;
        };
        for output in &preset.outputs {
            if let Some(device) = self
                .devices
                .iter_mut()
                .find(|device| device.name == output.name)
            {
                device.volume_percent = output.volume_percent.min(100);
                device.muted = output.muted;
            }
        }
    }

    fn save_current_preset(&mut self) {
        let selected = self.present_selection();
        let outputs = selected
            .iter()
            .filter_map(|name| {
                self.devices
                    .iter()
                    .find(|device| &device.name == name)
                    .map(|device| PresetOutput {
                        name: device.name.clone(),
                        volume_percent: device.volume_percent,
                        muted: device.muted,
                    })
            })
            .collect();
        let preset = AudioPreset {
            name: self.preset_name.clone(),
            outputs,
            latency_compensation: self.config.latency_compensation,
        };

        match self.config.upsert_preset(preset) {
            Ok(()) => {
                self.preset_name.clear();
                self.save_config();
            }
            Err(error) => self.notice = Some(error),
        }
    }

    fn apply_preset(&mut self, index: usize) {
        let Some(preset) = self.config.presets.get(index).cloned() else {
            return;
        };
        let available = self
            .devices
            .iter()
            .map(|device| device.name.as_str())
            .collect::<HashSet<_>>();
        let missing = preset
            .outputs
            .iter()
            .filter(|output| !available.contains(output.name.as_str()))
            .map(|output| output.name.clone())
            .collect::<Vec<_>>();

        self.config.selected_outputs = preset
            .outputs
            .iter()
            .map(|output| output.name.clone())
            .collect();
        self.config.latency_compensation = preset.latency_compensation;
        self.config.selection_initialized = true;
        for output in &preset.outputs {
            if let Some(device) = self
                .devices
                .iter_mut()
                .find(|device| device.name == output.name)
            {
                device.volume_percent = output.volume_percent.min(100);
                device.muted = output.muted;
            }
        }
        self.preset_name = preset.name.clone();
        self.pending_preset = Some(preset);
        self.notice = (!missing.is_empty()).then(|| {
            format!(
                "The preset was applied, but these outputs are not connected: {}",
                missing.join(", ")
            )
        });
        self.save_config();
    }

    fn initialize_selection(&mut self) {
        if self.config.selection_initialized {
            return;
        }

        if let Some(default) = &self.default_sink
            && self.devices.iter().any(|device| &device.name == default)
        {
            self.config.selected_outputs.push(default.clone());
        }

        for device in &self.devices {
            if self.config.selected_outputs.len() >= 2 {
                break;
            }
            if !self.config.selected_outputs.contains(&device.name) {
                self.config.selected_outputs.push(device.name.clone());
            }
        }

        self.config.selection_initialized = true;
        self.save_config();
    }

    fn present_selection(&self) -> Vec<String> {
        let available = self
            .devices
            .iter()
            .map(|device| device.name.as_str())
            .collect::<HashSet<_>>();
        self.config
            .selected_outputs
            .iter()
            .filter(|name| available.contains(name.as_str()))
            .cloned()
            .collect()
    }

    fn save_config(&mut self) {
        if let Err(error) = self.config.save() {
            tracing::warn!(%error, "failed to save configuration");
            self.notice = Some(format!("Your output selection could not be saved: {error}"));
        }
    }

    fn stop_before_exit(&mut self) {
        if let Some(session) = self.session.take()
            && let Err(error) = AudioController::stop(&session)
        {
            tracing::warn!(%error, "failed to stop audio splitting during exit");
        }
    }
}

impl Drop for AppModel {
    fn drop(&mut self) {
        self.stop_before_exit();
    }
}

fn discover_task(prepare: bool) -> Task<Message> {
    cosmic::task::future(async move {
        Message::DevicesLoaded(
            run_blocking(move || {
                if prepare {
                    AudioController::prepare()
                } else {
                    AudioController::discover()
                }
            })
            .await,
        )
    })
}

fn stop_task(session: SplitSession) -> Task<Message> {
    cosmic::task::future(async move {
        Message::Stopped(run_blocking(move || AudioController::stop(&session)).await)
    })
}

async fn run_blocking<T, E>(
    operation: impl FnOnce() -> Result<T, E> + Send + 'static,
) -> Result<T, String>
where
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| format!("audio task could not finish: {error}"))?
        .map_err(|error| error.to_string())
}
