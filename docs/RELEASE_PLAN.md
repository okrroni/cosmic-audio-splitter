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

## Architecture boundaries

- The UI depends on `AudioController`, not on `pactl` command details.
- `AudioBackend` contains the control-plane operations and may later be implemented with a native libpulse or PipeWire client.
- `SessionStore` journals every session before the first audio mutation.
- Start and stop are transactions: failures trigger rollback and every operation is safe to retry.
- Device events are event-driven through `pactl subscribe`; discovery is not continuously polled.
- The virtual sink keeps the historical `cosmic_audio_splitter` name so older stale sessions remain detectable.

## Phase status

- Phase 1: the public identity is `io.github.okrroni.splitter`, the repository URL is aligned, and the 1.0 boundary is defined above.
- Phase 2: the `pactl` backend is isolated, commands have timeouts, sessions are journaled atomically, start/stop have rollback and verification, operations are serialized, and PipeWire changes trigger event-driven refresh and safe stop on output removal.
- Remaining validation: exercise the real backend on physical hardware and from inside the future Flatpak sandbox before calling these phases release-complete.

## Deferred until after 1.0

- Background or login service.
- Manual per-device latency calibration.
- Network audio outputs.
- Per-application routing rules beyond moving active desktop streams.
