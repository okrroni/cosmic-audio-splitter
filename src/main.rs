// SPDX-License-Identifier: MIT

mod app;
mod audio;
mod config;

use cosmic::iced::{Limits, Size};

#[derive(Clone, Debug, Default)]
pub struct Flags;

impl cosmic::app::CosmicFlags for Flags {
    type SubCommand = String;
    type Args = Vec<String>;
}

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cosmic_audio_splitter=info".into()),
        )
        .init();

    let settings = cosmic::app::Settings::default()
        .size(Size::new(760.0, 720.0))
        .size_limits(Limits::NONE.min_width(440.0).min_height(420.0));

    cosmic::app::run_single_instance::<app::AppModel>(settings, Flags)
}
