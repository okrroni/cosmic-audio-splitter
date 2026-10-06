# Audio Splitter 1.0 contract

## Product boundary

Audio Splitter mirrors desktop playback to two or more local PipeWire outputs. A split session is temporary and is owned by the running application. Closing the application stops the split and restores a usable physical output.

Version 1.0 will not install a background service, change persistent PipeWire configuration, record audio, or promise sample-accurate synchronization for Bluetooth devices.

## Required user journeys

1. Discover available outputs and select at least two.
2. Start a split without restarting PipeWire or applications.
3. Route existing and newly opened playback streams through the split output.
4. React safely when an output is connected, disconnected, or changes profile.
5. Stop repeatedly without leaving a stale module or invalid default output.
6. Recover the previous default output after an application crash on the next launch.
7. Run in a Flatpak with only display, graphics, IPC, and PulseAudio socket permissions.
8. Audition each output before starting a split and adjust active output levels independently.
9. Save and reapply named output presets without making the audio backend part of the configuration format.

## Architecture boundaries

- The UI depends on `AudioController`, not on `pactl` command details.
- `AudioBackend` contains the control-plane operations and may later be implemented with a native libpulse or PipeWire client.
- `SessionStore` journals every session before the first audio mutation.
- Start and stop are transactions: failures trigger rollback and every operation is safe to retry.
- Device events are event-driven through `pactl subscribe`; discovery is not continuously polled.
- The virtual sink keeps the historical `cosmic_audio_splitter` name so older stale sessions remain detectable.

## Phase status

- Phase 1: the public identity is `io.github.okrroni.cosmic_audio_splitter`. Flathub demangles its final component to `cosmic-audio-splitter`, while the underscore form remains a valid D-Bus name for libcosmic single-instance activation. Before submission, rename the GitHub repository to `cosmic-audio-splitter` or agree an ID exception with reviewers; code-hosting IDs are normally resolved from their final component.
- Phase 2: the `pactl` backend is isolated, commands have timeouts, sessions are journaled atomically, start/stop have rollback and verification, operations are serialized, and PipeWire changes trigger event-driven refresh and safe stop on output removal.
- Phase 3: named presets persist output selection, levels, mute state, and delay compensation; active outputs expose independent volume and mute controls; every connected output can play a generated test tone without a bundled media asset or external player dependency.
- Remaining validation: exercise the real backend on physical hardware and from inside the future Flatpak sandbox before calling these phases release-complete.

## Phase 3 design decisions

- Presets are plain version-tolerant configuration data. They contain stable PipeWire output names and never serialize backend command details.
- Applying a preset stages its levels in the UI and applies them immediately before the transactional split starts. If a device is absent, it remains in the preset and the UI reports it instead of silently deleting user data.
- Test audio is a short PCM tone generated at runtime, uploaded to the PipeWire PulseAudio sample cache, played on one explicit sink, and removed immediately afterwards.
- Mute and volume are separate backend operations so a future native PipeWire implementation can replace `pactl` without changing UI or preset data.

## Deferred until after 1.0

- Background or login service.
- Manual per-device latency calibration.
- Network audio outputs.
- Per-application routing rules beyond moving active desktop streams.
