// SPDX-License-Identifier: MIT

use crate::audio::{AudioController, AudioDevice, AudioSnapshot, SplitSession, StartOutcome};
use crate::config::AppConfig;
use cosmic::app::{Task, context_drawer};
use cosmic::iced::Length;
use cosmic::iced::alignment::Horizontal;
use cosmic::prelude::*;
use cosmic::widget::{self, about::About, icon, settings};
use std::collections::HashSet;

const APP_ICON: &[u8] =
    include_bytes!("../resources/icons/hicolor/scalable/apps/io.github.okrroni.AudioSplitter.svg");

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
    DevicesLoaded(Result<AudioSnapshot, String>),
    DeviceVolumeChanged(String, u32),
    DeviceVolumeSet(Result<(), String>),
    DismissNotice,
    LaunchUrl(String),
    Refresh,
    Start,
    Started(Result<StartOutcome, String>),
    Stop,
    Stopped(Result<(), String>),
    SetDeviceVolume(String),
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
    notice: Option<String>,
}

impl cosmic::Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = crate::Flags;
    type Message = Message;

    const APP_ID: &'static str = "io.github.okrroni.AudioSplitter";

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

    fn view(&self) -> Element<'_, Self::Message> {
        let spacing = cosmic::theme::spacing();
        let selected = self.present_selection();
        let selected_count = selected.len();
        let is_active = self.session.is_some();
        let is_busy = self.busy.is_some();

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
                .on_press(Message::Stop)
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
                .on_press_maybe((selected_count >= 2).then_some(Message::Start))
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
            let description = format!("{}{default_suffix}", device.detail);
            let item = settings::item::builder(device.description.clone())
                .description(description)
                .icon(icon::from_name(device.icon_name.clone()).size(24).icon());

            outputs = if is_active && !is_busy && checked {
                let change_name = name.clone();
                let slider = widget::slider(0..=100, device.volume_percent, move |value| {
                    Message::DeviceVolumeChanged(change_name.clone(), value)
                })
                .on_release(Message::SetDeviceVolume(name))
                .width(Length::Fixed(160.0));
                let controls = widget::row::with_capacity(3)
                    .push(widget::checkbox(true))
                    .push(slider)
                    .push(
                        widget::text::caption(format!("{}%", device.volume_percent))
                            .width(Length::Fixed(40.0)),
                    )
                    .spacing(spacing.space_xs)
                    .align_y(cosmic::iced::Alignment::Center);
                outputs.add(item.control(controls))
            } else if is_active || is_busy {
                outputs.add(item.control(widget::checkbox(checked)))
            } else {
                outputs.add(item.checkbox(checked, move |value| {
                    Message::ToggleOutput(name.clone(), value)
                }))
            };
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

        let mut sections: Vec<Element<'_, Message>> =
            vec![intro.into(), status.into(), outputs.into(), advanced.into()];

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
            Message::DevicesLoaded(result) => {
                self.busy = None;
                match result {
                    Ok(snapshot) => {
                        self.devices = snapshot.devices;
                        self.default_sink = snapshot.default_sink;
                        self.initialize_selection();
                    }
                    Err(error) => {
                        self.devices.clear();
                        self.default_sink = None;
                        self.notice = Some(error);
                    }
                }
            }
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
            Message::Refresh => {
                if self.busy.is_none() {
                    self.busy = Some(Busy::Refreshing);
                    return discover_task(false);
                }
            }
            Message::Start => {
                let outputs = self.present_selection();
                if self.busy.is_none() && self.session.is_none() && outputs.len() >= 2 {
                    self.busy = Some(Busy::Starting);
                    self.notice = None;
                    let latency_compensation = self.config.latency_compensation;
                    return cosmic::task::future(async move {
                        Message::Started(
                            run_blocking(move || {
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
                    }
                    Err(error) => self.notice = Some(error),
                }
            }
            Message::Stop => {
                if self.busy.is_none()
                    && let Some(session) = self.session.clone()
                {
                    self.busy = Some(Busy::Stopping);
                    return cosmic::task::future(async move {
                        Message::Stopped(
                            run_blocking(move || AudioController::stop(&session)).await,
                        )
                    });
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
