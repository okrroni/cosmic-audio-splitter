# Audio Splitter for COSMIC

Audio Splitter is a native COSMIC desktop application that plays the same desktop audio through two or more Linux audio outputs. Select the outputs and press **Start splitting**; no PipeWire configuration files or service restarts are required.

Splitting remains active while the application is open. Closing the application stops the temporary route and restores the previous output. A background service is intentionally outside the 1.0 scope so session ownership and recovery remain predictable.

Each output can be auditioned with a short test tone. During an active split, selected outputs have independent volume and mute controls. Named presets remember the selected outputs, their levels and mute state, and the delay-compensation option.

## How it works

The application asks PipeWire's PulseAudio-compatible control service to create a temporary `module-combine-sink`. PipeWire remains responsible for audio fan-out, resampling, and clock handling—the application never copies audio samples itself. Existing playback streams are moved to the temporary output when splitting starts, and the previous default output is restored when it stops.

The output test is generated locally at runtime and sent through `pactl`'s sample cache to one explicit device. It does not require a bundled audio file or a separate media player.

Minimum-latency mode is the default. **Compensate device delay** can reduce drift between unlike devices, but it may add buffering. Bluetooth devices always add their own codec and radio latency, which software cannot eliminate completely.

## Requirements

- Linux with PipeWire and `pipewire-pulse`
- `pactl` (usually supplied by `pulseaudio-utils` or `pipewire-utils`)
- Rust 1.93 or newer to build
- libcosmic build dependencies

Fedora 44 COSMIC:

```sh
sudo dnf install rust cargo rustfmt clippy gcc gcc-c++ cmake make just \
  fontconfig-devel freetype-devel libxkbcommon-devel wayland-devel \
  libX11-devel libXi-devel libXcursor-devel libxkbcommon-x11-devel \
  mesa-libEGL-devel
```

Pop!_OS / Ubuntu with COSMIC packages:

```sh
sudo apt install cargo cmake just libexpat1-dev libfontconfig-dev \
  libfreetype-dev libxkbcommon-dev pkgconf pulseaudio-utils
```

## Build and run

```sh
cargo build --release
cargo run --release
```

Or, with `just`:

```sh
just check
just run
sudo just install
```

## Recovery

The application removes its temporary output on normal exit. It also detects and removes its own stale combine module the next time it starts after a crash. To remove one manually:

```sh
pactl list short modules
pactl unload-module MODULE_ID
```

Only unload the `module-combine-sink` whose arguments include `sink_name=cosmic_audio_splitter`.

## Project status

This is a pre-release implementation. The core routing and configuration parsers have unit tests; hardware behavior should also be checked with wired, HDMI, USB, and Bluetooth combinations because latency depends on each device and driver.

The 1.0 readiness contract and architecture decisions are tracked in [`docs/RELEASE_PLAN.md`](docs/RELEASE_PLAN.md).

## License

MIT
